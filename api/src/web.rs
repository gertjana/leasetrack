use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use leasetrack_core::{add_record, authenticate_user, compute_report_data, generate_api_key, generate_token, issue_reset_token, load_user_data, redeem_reset_token, save_user_data, load_users, save_users, secret_eq, User, LeaseConfig, LeaseData};
use minijinja::{context, Environment};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// ─── State ────────────────────────────────────────────────────────────────────

/// How long a signed-in session stays valid.
const SESSION_TTL_SECS: i64 = 12 * 60 * 60;

/// A server-side session. The browser only ever holds the opaque session id, so
/// signing out (or expiry) revokes access immediately — unlike the API key,
/// which is a long-lived credential that a cookie could otherwise leak forever.
#[derive(Clone)]
pub struct Session {
    pub email: String,
    pub csrf: String,
    pub expires: i64,
}

#[derive(Clone)]
pub struct WebState {
    pub env: Arc<Environment<'static>>,
    pub app_env: String,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
}

impl WebState {
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.add_template_owned("login.html".to_string(), LOGIN_HTML.to_string())
            .expect("login template");
        env.add_template_owned("dashboard.html".to_string(), DASHBOARD_HTML.to_string())
            .expect("dashboard template");
        env.add_template_owned("register.html".to_string(), REGISTER_HTML.to_string())
            .expect("register template");
        env.add_template_owned("setup.html".to_string(), SETUP_HTML.to_string())
            .expect("setup template");
        env.add_template_owned("forgot.html".to_string(), FORGOT_HTML.to_string())
            .expect("forgot template");
        env.add_template_owned("reset.html".to_string(), RESET_HTML.to_string())
            .expect("reset template");
        let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());
        WebState {
            env: Arc::new(env),
            app_env,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// True when running in production, where cookies must be HTTPS-only.
    fn is_production(&self) -> bool {
        self.app_env == "production"
    }

    /// Create a session for `email` and return `(session_id, csrf_token)`.
    fn create_session(&self, email: String) -> (String, String) {
        let id = generate_token();
        let csrf = generate_token();
        let expires = chrono::Local::now().timestamp() + SESSION_TTL_SECS;
        let mut sessions = self.sessions.lock().expect("session lock poisoned");
        // Opportunistically drop anything already expired so the map cannot grow
        // without bound in a long-running process.
        let now = chrono::Local::now().timestamp();
        sessions.retain(|_, s| s.expires > now);
        sessions.insert(id.clone(), Session { email, csrf: csrf.clone(), expires });
        (id, csrf)
    }

    fn get_session(&self, id: &str) -> Option<Session> {
        let mut sessions = self.sessions.lock().expect("session lock poisoned");
        let session = sessions.get(id)?.clone();
        if session.expires <= chrono::Local::now().timestamp() {
            sessions.remove(id);
            return None;
        }
        Some(session)
    }

    fn destroy_session(&self, id: &str) {
        self.sessions.lock().expect("session lock poisoned").remove(id);
    }

    /// Invalidate every session belonging to `email` (used when the API key
    /// changes, so old sessions cannot outlive a credential reset).
    fn destroy_sessions_for(&self, email: &str) {
        self.sessions
            .lock()
            .expect("session lock poisoned")
            .retain(|_, s| !s.email.eq_ignore_ascii_case(email));
    }
}

// ─── Cookie / session helpers ─────────────────────────────────────────────────

const COOKIE_SESSION: &str = "lt_session";

/// Build a hardened cookie: HttpOnly always, SameSite=Strict to blunt CSRF, and
/// Secure whenever we are running in production over HTTPS.
fn build_cookie(state: &WebState, name: &'static str, value: String) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(state.is_production())
        .build()
}

/// Resolve the current session from the request cookies.
///
/// The caller's identity comes from the server-side session only. Nothing the
/// browser sends other than the opaque session id influences which account is
/// loaded, which is what prevents one user from acting as another.
fn current_session(state: &WebState, jar: &CookieJar) -> Option<Session> {
    let id = jar.get(COOKIE_SESSION)?.value().to_owned();
    state.get_session(&id)
}

/// Reject a POST whose CSRF token does not match the session's.
fn csrf_ok(session: &Session, submitted: &str) -> bool {
    secret_eq(&session.csrf, submitted)
}

fn csrf_rejected() -> Response {
    (StatusCode::FORBIDDEN, "Invalid or missing CSRF token. Please reload the page and try again.")
        .into_response()
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /` — redirect to dashboard if already authenticated, else login.
pub async fn index(State(state): State<WebState>, jar: CookieJar) -> Response {
    if current_session(&state, &jar).is_some() {
        Redirect::to("/dashboard").into_response()
    } else {
        Redirect::to("/login").into_response()
    }
}

/// `GET /login`
pub async fn login_page(
    State(state): State<WebState>,
    jar: CookieJar,
) -> Response {
    if current_session(&state, &jar).is_some() {
        return Redirect::to("/dashboard").into_response();
    }
    render(&state, "login.html", context! { error => "", app_env => state.app_env })
}

#[derive(Deserialize)]
pub struct LoginForm {
    email: String,
    api_key: String,
}

/// `POST /login`
pub async fn login_post(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Response {
    let user = match authenticate_user(form.email.trim(), &form.api_key) {
        Some(user) => user,
        None => {
            return render(
                &state,
                "login.html",
                context! { error => "Invalid email or API key. Please try again.", app_env => state.app_env },
            )
        }
    };

    // Identity comes from the stored user record, never from what was typed.
    let (session_id, _csrf) = state.create_session(user.email);
    let cookie = build_cookie(&state, COOKIE_SESSION, session_id);

    (jar.add(cookie), Redirect::to("/dashboard")).into_response()
}

/// `GET /logout` — destroys the session server-side, so the credential is
/// genuinely revoked rather than just dropped by the browser.
pub async fn logout(State(state): State<WebState>, jar: CookieJar) -> Response {
    if let Some(cookie) = jar.get(COOKIE_SESSION) {
        state.destroy_session(cookie.value());
    }
    let removal = Cookie::build((COOKIE_SESSION, "")).path("/").build();
    (jar.remove(removal), Redirect::to("/login")).into_response()
}

#[derive(Deserialize)]
pub struct RecordForm {
    odometer: String,
    date: String,
    #[serde(default)]
    csrf_token: String,
}

/// `POST /web/record`
pub async fn web_record(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<RecordForm>,
) -> Response {
    let session = match current_session(&state, &jar) {
        Some(s) => s,
        None => return Redirect::to("/login").into_response(),
    };
    if !csrf_ok(&session, &form.csrf_token) {
        return csrf_rejected();
    }
    let email = session.email.clone();

    let odometer: Result<u32, _> = form.odometer.trim().parse();
    let date = chrono::NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d");

    let (record_success, record_error) = match (odometer, date) {
        (Ok(odo), Ok(d)) => {
            let mut data = match load_user_data(&email) {
                Ok(d) => d,
                Err(e) => {
                    return render_dashboard(&state, &session, Some(e), None).await;
                }
            };
            match add_record(&mut data, odo, d) {
                Ok(warnings) => {
                    let _ = save_user_data(&email, &data);
                    let msg = if warnings.is_empty() {
                        format!("Recorded {} km on {}", odo, d)
                    } else {
                        format!("Recorded {} km on {} ({})", odo, d, warnings.join("; "))
                    };
                    (Some(msg), None)
                }
                Err(e) => (None, Some(e)),
            }
        }
        (Err(_), _) => (None, Some("Invalid odometer value — must be a number.".into())),
        (_, Err(_)) => (None, Some("Invalid date — use YYYY-MM-DD format.".into())),
    };

    render_dashboard(&state, &session, record_error, record_success).await
}

#[derive(Deserialize)]
pub struct ConfigForm {
    car_name: String,
    lease_start: String,
    lease_years: String,
    allowed_km_per_year: String,
    start_odometer: String,
    #[serde(default)]
    csrf_token: String,
}

/// `POST /web/config`
pub async fn web_config(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<ConfigForm>,
) -> Response {
    let session = match current_session(&state, &jar) {
        Some(s) => s,
        None => return Redirect::to("/login").into_response(),
    };
    if !csrf_ok(&session, &form.csrf_token) {
        return csrf_rejected();
    }
    let email = session.email.clone();

    let parse_err = |msg: &str| render_dashboard(&state, &session, Some(msg.into()), None);

    let lease_start = match chrono::NaiveDate::parse_from_str(form.lease_start.trim(), "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return parse_err("Invalid lease start date — use YYYY-MM-DD.").await,
    };
    let lease_years: u32 = match form.lease_years.trim().parse() {
        Ok(n) if (1..=10).contains(&n) => n,
        _ => return parse_err("Lease years must be between 1 and 10.").await,
    };
    let allowed_km_per_year: u32 = match form.allowed_km_per_year.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => return parse_err("Allowed km/year must be greater than 0.").await,
    };
    let start_odometer: u32 = form.start_odometer.trim().parse().unwrap_or(0);
    let car_name = form.car_name.trim().to_string();
    if car_name.is_empty() || car_name.len() > 100 {
        return parse_err("Car name must be between 1 and 100 characters.").await;
    }

    let mut data = match load_user_data(&email) {
        Ok(d) => d,
        Err(_) => LeaseData { config: LeaseConfig { car_name: car_name.clone(), lease_start, lease_years, allowed_km_per_year, start_odometer }, records: vec![] },
    };

    data.config = LeaseConfig { car_name, lease_start, lease_years, allowed_km_per_year, start_odometer };

    if let Err(e) = save_user_data(&email, &data) {
        return render_dashboard(&state, &session, Some(e), None).await;
    }

    render_dashboard(&state, &session, None, Some("Configuration saved.".into())).await
}

/// `GET /dashboard`
pub async fn dashboard(
    State(state): State<WebState>,
    jar: CookieJar,
) -> Response {
    let session = match current_session(&state, &jar) {
        Some(s) => s,
        None => return Redirect::to("/login").into_response(),
    };
    render_dashboard(&state, &session, None, None).await
}

// ─── Setup ────────────────────────────────────────────────────────────────────

/// `GET /setup` — initial lease configuration for new users.
pub async fn setup_page(State(state): State<WebState>, jar: CookieJar) -> Response {
    let session = match current_session(&state, &jar) {
        Some(s) => s,
        None => return Redirect::to("/login").into_response(),
    };
    // If already set up, go straight to dashboard
    if load_user_data(&session.email).is_ok() {
        return Redirect::to("/dashboard").into_response();
    }
    let today = chrono::Local::now().date_naive().to_string();
    render(&state, "setup.html", context! { error => "", today => today, csrf_token => session.csrf, app_env => state.app_env })
}

/// `POST /setup`
pub async fn setup_post(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<ConfigForm>,
) -> Response {
    let session = match current_session(&state, &jar) {
        Some(s) => s,
        None => return Redirect::to("/login").into_response(),
    };
    if !csrf_ok(&session, &form.csrf_token) {
        return csrf_rejected();
    }
    let email = session.email.clone();

    let today = chrono::Local::now().date_naive().to_string();
    let err = |msg: &str| render(&state, "setup.html", context! { error => msg, today => today, csrf_token => session.csrf, app_env => state.app_env });

    let lease_start = match chrono::NaiveDate::parse_from_str(form.lease_start.trim(), "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return err("Invalid lease start date — use YYYY-MM-DD."),
    };
    let lease_years: u32 = match form.lease_years.trim().parse() {
        Ok(n) if (1..=10).contains(&n) => n,
        _ => return err("Lease years must be between 1 and 10."),
    };
    let allowed_km_per_year: u32 = match form.allowed_km_per_year.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => return err("Allowed km/year must be greater than 0."),
    };
    let start_odometer: u32 = form.start_odometer.trim().parse().unwrap_or(0);
    let car_name = form.car_name.trim().to_string();
    if car_name.is_empty() || car_name.len() > 100 {
        return err("Car name must be between 1 and 100 characters.");
    }

    let data = LeaseData {
        config: LeaseConfig { car_name, lease_start, lease_years, allowed_km_per_year, start_odometer },
        records: vec![],
    };

    if let Err(e) = save_user_data(&email, &data) {
        return err(&e);
    }

    Redirect::to("/dashboard").into_response()
}

// ─── Forgot API key ───────────────────────────────────────────────────────────

/// `GET /forgot`
pub async fn forgot_page(State(state): State<WebState>) -> Response {
    render(&state, "forgot.html", context! { success => false, app_env => state.app_env })
}

#[derive(Deserialize)]
pub struct ForgotForm {
    email: String,
}

/// `POST /forgot` — email a single-use reset link if the address is registered.
///
/// The existing API key is left intact. Only following the emailed link rotates
/// it, so an unauthenticated request cannot lock a user out of their account.
pub async fn forgot_post(
    State(state): State<WebState>,
    Form(form): Form<ForgotForm>,
) -> Response {
    let email = form.email.trim().to_lowercase();

    match issue_reset_token(&email) {
        Ok(Some(token)) => {
            let link = format!("{}/reset?token={}", base_url(), token);
            if let Err(e) = crate::email::send_reset_email(&email, &link).await {
                tracing::error!("Failed to send reset email: {e}");
            }
        }
        Ok(None) => {
            // No such user. Fall through to the same response so the page does
            // not reveal which addresses are registered.
        }
        Err(e) => tracing::error!("Failed to issue reset token: {e}"),
    }

    render(&state, "forgot.html", context! { success => true, app_env => state.app_env })
}

#[derive(Deserialize)]
pub struct ResetQuery {
    #[serde(default)]
    token: String,
}

/// `GET /reset?token=…` — redeem a reset link and show the new API key once.
pub async fn reset_page(
    State(state): State<WebState>,
    Query(query): Query<ResetQuery>,
) -> Response {
    match redeem_reset_token(&query.token) {
        Ok((email, new_key)) => {
            // The old key is gone, so any session resting on it must go too.
            state.destroy_sessions_for(&email);
            if let Err(e) = crate::email::send_registration_email(&email, &new_key).await {
                tracing::error!("Failed to send new key email: {e}");
            }
            render(
                &state,
                "reset.html",
                context! { error => "", api_key => new_key, app_env => state.app_env },
            )
        }
        Err(e) => render(
            &state,
            "reset.html",
            context! { error => e, api_key => "", app_env => state.app_env },
        ),
    }
}

/// Public base URL used to build links in outgoing email.
fn base_url() -> String {
    std::env::var("APP_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string())
        .trim_end_matches('/')
        .to_string()
}

// ─── Registration ─────────────────────────────────────────────────────────────

/// `GET /register`
pub async fn register_page(State(state): State<WebState>) -> Response {
    render(&state, "register.html", context! { error => "", success => false, app_env => state.app_env })
}

#[derive(Deserialize)]
pub struct RegisterForm {
    email: String,
}

/// `POST /register`
pub async fn register_post(
    State(state): State<WebState>,
    Form(form): Form<RegisterForm>,
) -> Response {
    let email = form.email.trim().to_lowercase();

    // Basic email sanity check
    if !email.contains('@') || email.len() < 3 {
        return render(
            &state,
            "register.html",
            context! { error => "Please enter a valid email address.", success => false, app_env => state.app_env },
        );
    }

    let mut users = load_users().unwrap_or_default();

    // Check for existing user — silently succeed to avoid leaking info
    let api_key = if let Some(existing) = users.users.iter().find(|u| u.email == email) {
        existing.api_key.clone()
    } else {
        let key = generate_api_key();
        users.users.push(User::new(email.clone(), key.clone()));
        if let Err(e) = save_users(&users) {
            tracing::error!("Failed to save users file: {e}");
            return render(
                &state,
                "register.html",
                context! { error => "Something went wrong. Please try again.", success => false, app_env => state.app_env },
            );
        }
        key
    };

    // Send email (or log to console locally)
    if let Err(e) = crate::email::send_registration_email(&email, &api_key).await {
        tracing::error!("Failed to send registration email: {e}");
        // Still show success — key is saved, user can retry
    }

    render(&state, "register.html", context! { error => "", success => true, app_env => state.app_env })
}

async fn render_dashboard(
    state: &WebState,
    session: &Session,
    record_error: Option<String>,
    record_success: Option<String>,
) -> Response {
    let email = session.email.clone();
    let today = chrono::Local::now().date_naive().to_string();

    let data = match load_user_data(&email) {
        Ok(d) => d,
        Err(_) => return Redirect::to("/setup").into_response(),
    };

    let report = compute_report_data(&data);

    // Build records list newest-first with delta
    let mut records: Vec<minijinja::Value> = Vec::new();
    let raw = &data.records;
    for (i, rec) in raw.iter().enumerate().rev() {
        let delta: Option<i64> = if i > 0 {
            Some(rec.odometer as i64 - raw[i - 1].odometer as i64)
        } else {
            None
        };
        records.push(context! {
            date => rec.date.to_string(),
            odometer => rec.odometer,
            delta => delta,
        });
    }

    // Year bars: pct of allowed, capped at 125%
    let allowed = report.km_allowed_per_year as f64;
    let proj_year_total_km = report.current_year.as_ref().map(|p| p.projected_year_total);
    let years: Vec<minijinja::Value> = report
        .years
        .iter()
        .map(|y| {
            let km = y.km_driven.unwrap_or(0.0);
            let pct = ((km / allowed) * 100.0).min(125.0) as u32;
            let status = if y.is_future {
                "future"
            } else if y.is_current {
                "current"
            } else if km > allowed {
                "over"
            } else {
                "ok"
            };
            // For the current year, add a projected-remainder segment
            let proj_pct: Option<u32> = if y.is_current {
                proj_year_total_km.map(|proj_total| {
                    let full_pct = (proj_total / allowed * 100.0).min(125.0) as u32;
                    full_pct.saturating_sub(pct)
                })
            } else {
                None
            };
            context! {
                year_num => y.year_num,
                km => km as u32,
                pct => pct,
                proj_pct => proj_pct,
                status => status,
            }
        })
        .collect();

    let proj = report.current_year.as_ref();
    let proj_year_diff = proj.map(|p| p.projected_diff as i64);
    let proj_year_total = proj.map(|p| p.projected_year_total as u32);
    let proj_total_diff = match (report.projected_total, report.km_allowed_total) {
        (Some(t), a) => Some(t as i64 - a as i64),
        _ => None,
    };
    let last_odometer = report.last_record.as_ref().map(|r| r.odometer);
    let last_date = report.last_record.as_ref().map(|r| r.date.to_string());

    render(
        state,
        "dashboard.html",
        context! {
            error => "",
            car_name => report.car_name,
            lease_start => report.lease_start.to_string(),
            lease_end => report.lease_end.to_string(),
            lease_years => report.lease_years,
            km_allowed_per_year => report.km_allowed_per_year,
            km_allowed_total => report.km_allowed_total,
            start_odometer => data.config.start_odometer,
            total_driven => report.total_driven as u32,
            last_odometer => last_odometer,
            last_date => last_date,
            avg_daily_rate => report.avg_daily_rate.map(|r| r as u32),
            proj_year_total => proj_year_total,
            proj_year_diff => proj_year_diff,
            proj_total_diff => proj_total_diff,
            years => years,
            records => records,
            today => today,
            record_error => record_error.unwrap_or_default(),
            record_success => record_success.unwrap_or_default(),
            email => email,
            csrf_token => session.csrf,
            app_env => state.app_env,
        },
    )
}

// ─── Render helper ────────────────────────────────────────────────────────────

fn render(state: &WebState, template: &str, ctx: minijinja::Value) -> Response {
    match state.env.get_template(template) {
        Ok(tmpl) => match tmpl.render(ctx) {
            Ok(html) => Html(html).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Template error: {e}"),
            )
                .into_response(),
        },
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Template not found: {e}"),
        )
            .into_response(),
    }
}

// ─── Templates ────────────────────────────────────────────────────────────────

const RESET_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="referrer" content="no-referrer">
  <title>LeaseTrack — Reset API Key</title>
  <style>
    :root {
      --bg:#ffffff; --bg-card:#f6f8fa; --border:#d0d7de; --text:#1f2328;
      --muted:#656d76; --accent:#0969da; --btn-bg:#1a7f37; --btn-hover:#2da44e;
      --input-bg:#ffffff; --ok-bg:#dafbe1; --ok-fg:#1a7f37; --ok-border:#2da44e;
      --err-bg:#ffebe9; --err-fg:#cf222e; --err-border:#cf222e;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:#0d1117; --bg-card:#161b22; --border:#30363d; --text:#c9d1d9;
        --muted:#8b949e; --accent:#58a6ff; --btn-bg:#238636; --btn-hover:#2ea043;
        --input-bg:#0d1117; --ok-bg:#1a2e1a; --ok-fg:#3fb950; --ok-border:#3fb950;
        --err-bg:#2d1214; --err-fg:#f85149; --err-border:#f85149;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: 'Courier New', monospace; background: var(--bg); color: var(--text); display: flex; align-items: center; justify-content: center; min-height: 100vh; }
    .card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 2.5rem 2rem; width: 100%; max-width: 420px; }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: var(--accent); }
    .subtitle { font-size: 0.8rem; color: var(--muted); margin-bottom: 2rem; }
    .success { background: var(--ok-bg); border: 1px solid var(--ok-border); border-radius: 6px; color: var(--ok-fg); padding: 0.75rem; font-size: 0.9rem; margin-bottom: 1rem; }
    .error { background: var(--err-bg); border: 1px solid var(--err-border); border-radius: 6px; color: var(--err-fg); padding: 0.75rem; font-size: 0.9rem; margin-bottom: 1rem; }
    .key { display: block; background: var(--input-bg); border: 1px solid var(--border); border-radius: 6px; padding: 1rem; font-size: 1.05rem; word-break: break-all; margin-bottom: 1rem; }
    .hint { font-size: 0.8rem; color: var(--muted); margin-bottom: 1rem; }
    .back { display: block; text-align: center; margin-top: 1rem; font-size: 0.8rem; color: var(--muted); text-decoration: none; }
    .back:hover { color: var(--text); }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;position:fixed;top:0;width:100%;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  {% endif %}
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Reset your API key</p>
    {% if error %}
      <div class="error">{{ error }}</div>
      <p class="hint">Reset links can only be used once and expire after 30 minutes.</p>
      <a class="back" href="/forgot">Request a new link</a>
    {% else %}
      <div class="success">Your API key has been reset.</div>
      <code class="key">{{ api_key }}</code>
      <p class="hint">Copy it now — this is the only time it is shown. A copy has also been emailed to you. Your previous key no longer works.</p>
      <a class="back" href="/login">Go to sign in</a>
    {% endif %}
  </div>
</body>
</html>"#;

const FORGOT_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>LeaseTrack — Forgot API Key</title>
  <style>
    :root {
      --bg:#ffffff; --bg-card:#f6f8fa; --border:#d0d7de; --text:#1f2328;
      --muted:#656d76; --accent:#0969da; --btn-bg:#1a7f37; --btn-hover:#2da44e;
      --input-bg:#ffffff; --ok-bg:#dafbe1; --ok-fg:#1a7f37; --ok-border:#2da44e;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:#0d1117; --bg-card:#161b22; --border:#30363d; --text:#c9d1d9;
        --muted:#8b949e; --accent:#58a6ff; --btn-bg:#238636; --btn-hover:#2ea043;
        --input-bg:#0d1117; --ok-bg:#1a2e1a; --ok-fg:#3fb950; --ok-border:#3fb950;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: 'Courier New', monospace; background: var(--bg); color: var(--text); display: flex; align-items: center; justify-content: center; min-height: 100vh; }
    .card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 2.5rem 2rem; width: 100%; max-width: 380px; }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: var(--accent); }
    .subtitle { font-size: 0.8rem; color: var(--muted); margin-bottom: 2rem; }
    label { display: block; font-size: 0.85rem; margin-bottom: 0.4rem; color: var(--muted); }
    input[type=text] { width: 100%; padding: 0.6rem 0.75rem; background: var(--input-bg); border: 1px solid var(--border); border-radius: 6px; color: var(--text); font-family: inherit; font-size: 0.95rem; margin-bottom: 1.25rem; }
    input[type=text]:focus { outline: none; border-color: var(--accent); }
    button { width: 100%; padding: 0.65rem; background: var(--btn-bg); border: none; border-radius: 6px; color: #fff; font-family: inherit; font-size: 1rem; cursor: pointer; }
    button:hover { background: var(--btn-hover); }
    .success { background: var(--ok-bg); border: 1px solid var(--ok-border); border-radius: 6px; color: var(--ok-fg); padding: 0.75rem; font-size: 0.9rem; margin-bottom: 1rem; }
    .back { display: block; text-align: center; margin-top: 1rem; font-size: 0.8rem; color: var(--muted); text-decoration: none; }
    .back:hover { color: var(--text); }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;position:fixed;top:0;width:100%;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  <div style="height:1.8rem"></div>
  {% endif %}
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Reset your API key</p>
    {% if success %}
      <div class="success">If that email is registered, a reset link is on its way. The link expires in 30 minutes.</div>
      <a class="back" href="/login">Back to sign in</a>
    {% else %}
      <form method="post" action="/forgot">
        <label for="email">Email address</label>
        <input type="text" id="email" name="email" autofocus placeholder="you@example.com" autocomplete="email">
        <button type="submit">Send reset link</button>
      </form>
      <a class="back" href="/login">Back to sign in</a>
    {% endif %}
  </div>
</body>
</html>"#;

const SETUP_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>LeaseTrack — Setup</title>
  <style>
    :root {
      --bg:#ffffff; --bg-card:#f6f8fa; --border:#d0d7de; --text:#1f2328;
      --muted:#656d76; --accent:#0969da; --btn-bg:#1a7f37; --btn-hover:#2da44e;
      --input-bg:#ffffff; --err-bg:#fff0ee; --err-fg:#cf222e; --err-border:#f85149;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:#0d1117; --bg-card:#161b22; --border:#30363d; --text:#c9d1d9;
        --muted:#8b949e; --accent:#58a6ff; --btn-bg:#238636; --btn-hover:#2ea043;
        --input-bg:#0d1117; --err-bg:#3d1e1e; --err-fg:#f85149; --err-border:#f85149;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: 'Courier New', monospace; background: var(--bg); color: var(--text); min-height: 100vh; display: flex; align-items: center; justify-content: center; }
    .card { background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px; padding: 2.5rem 2rem; width: 100%; max-width: 440px; }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: var(--accent); }
    .subtitle { font-size: 0.8rem; color: var(--muted); margin-bottom: 2rem; }
    label { display: block; font-size: 0.85rem; color: var(--muted); margin-bottom: 0.3rem; margin-top: 1rem; }
    label:first-of-type { margin-top: 0; }
    input[type=text], input[type=number], input[type=date] {
      width: 100%; padding: 0.6rem 0.75rem; background: var(--input-bg);
      border: 1px solid var(--border); border-radius: 6px; color: var(--text);
      font-family: inherit; font-size: 0.95rem;
    }
    input:focus { outline: none; border-color: var(--accent); }
    button {
      margin-top: 1.5rem; width: 100%; padding: 0.65rem; background: var(--btn-bg);
      border: none; border-radius: 6px; color: #fff; font-family: inherit; font-size: 1rem; cursor: pointer;
    }
    button:hover { background: var(--btn-hover); }
    .error {
      background: var(--err-bg); border: 1px solid var(--err-border); border-radius: 6px;
      color: var(--err-fg); padding: 0.6rem 0.75rem; font-size: 0.85rem; margin-bottom: 1.25rem;
    }
    .hint { font-size: 0.75rem; color: var(--muted); margin-top: 0.25rem; }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;position:fixed;top:0;width:100%;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  <div style="height:1.8rem"></div>
  {% endif %}
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Let's set up your lease</p>
    {% if error %}<div class="error">{{ error }}</div>{% endif %}
    <form method="post" action="/setup">
      <input type="hidden" name="csrf_token" value="{{ csrf_token }}">
      <label for="car_name">Car name</label>
      <input type="text" id="car_name" name="car_name" placeholder="e.g. Tesla Model 3" maxlength="100" required autofocus>

      <label for="lease_start">Lease start date</label>
      <input type="date" id="lease_start" name="lease_start" value="{{ today }}" required oninput="calcEnd()">

      <label for="lease_years">Lease duration (years)</label>
      <input type="number" id="lease_years" name="lease_years" value="3" min="1" max="10" required oninput="calcEnd()">
      <p class="hint">End date: <span id="end-date">—</span></p>

      <label for="allowed_km_per_year">Allowed km per year</label>
      <input type="number" id="allowed_km_per_year" name="allowed_km_per_year" value="20000" min="1" required>

      <label for="start_odometer">Start odometer (km)</label>
      <input type="number" id="start_odometer" name="start_odometer" value="0" min="0" required>

      <button type="submit">Start tracking</button>
    </form>
  </div>
  <script>
    function calcEnd() {
      const start = document.getElementById('lease_start').value;
      const years = parseInt(document.getElementById('lease_years').value, 10);
      const el = document.getElementById('end-date');
      if (start && years >= 1) {
        const d = new Date(start);
        d.setFullYear(d.getFullYear() + years);
        el.textContent = d.toISOString().slice(0, 10);
      }
    }
    calcEnd();
  </script>
</body>
</html>"#;

const REGISTER_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>LeaseTrack — Register</title>
  <style>
    :root {
      --bg:#ffffff; --bg-card:#f6f8fa; --border:#d0d7de; --text:#1f2328;
      --muted:#656d76; --accent:#0969da; --btn-bg:#1a7f37; --btn-hover:#2da44e;
      --input-bg:#ffffff; --err-bg:#fff0ee; --err-fg:#cf222e; --err-border:#f85149;
      --ok-bg:#dafbe1; --ok-fg:#1a7f37; --ok-border:#2da44e;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:#0d1117; --bg-card:#161b22; --border:#30363d; --text:#c9d1d9;
        --muted:#8b949e; --accent:#58a6ff; --btn-bg:#238636; --btn-hover:#2ea043;
        --input-bg:#0d1117; --err-bg:#3d1e1e; --err-fg:#f85149; --err-border:#f85149;
        --ok-bg:#1a2e1a; --ok-fg:#3fb950; --ok-border:#3fb950;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: 'Courier New', monospace; background: var(--bg); color: var(--text);
      display: flex; align-items: center; justify-content: center; min-height: 100vh;
    }
    .card {
      background: var(--bg-card); border: 1px solid var(--border); border-radius: 8px;
      padding: 2.5rem 2rem; width: 100%; max-width: 380px;
    }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: var(--accent); }
    .subtitle { font-size: 0.8rem; color: var(--muted); margin-bottom: 2rem; }
    label { display: block; font-size: 0.85rem; margin-bottom: 0.4rem; color: var(--muted); }
    input[type=text] {
      width: 100%; padding: 0.6rem 0.75rem; background: var(--input-bg);
      border: 1px solid var(--border); border-radius: 6px; color: var(--text);
      font-family: inherit; font-size: 0.95rem; margin-bottom: 1.25rem;
    }
    input[type=text]:focus { outline: none; border-color: var(--accent); }
    button {
      width: 100%; padding: 0.65rem; background: var(--btn-bg); border: none;
      border-radius: 6px; color: #fff; font-family: inherit; font-size: 1rem; cursor: pointer;
    }
    button:hover { background: var(--btn-hover); }
    .error {
      background: var(--err-bg); border: 1px solid var(--err-border); border-radius: 6px;
      color: var(--err-fg); padding: 0.6rem 0.75rem; font-size: 0.85rem; margin-bottom: 1rem;
    }
    .success {
      background: var(--ok-bg); border: 1px solid var(--ok-border); border-radius: 6px;
      color: var(--ok-fg); padding: 0.75rem; font-size: 0.9rem; margin-bottom: 1rem;
    }
    .back { display: block; text-align: center; margin-top: 1rem; font-size: 0.8rem; color: var(--muted); text-decoration: none; }
    .back:hover { color: var(--text); }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;position:fixed;top:0;width:100%;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  <div style="height:1.8rem"></div>
  {% endif %}
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Create your account</p>
    {% if success %}
      <div class="success">
        Check your email — we've sent your API key to sign in with.
      </div>
      <a class="back" href="/login">Back to sign in</a>
    {% else %}
      {% if error %}<div class="error">{{ error }}</div>{% endif %}
      <form method="post" action="/register">
        <label for="email">Email address</label>
        <input type="text" id="email" name="email" autofocus placeholder="you@example.com" autocomplete="email">
        <button type="submit">Send me my API key</button>
      </form>
      <a class="back" href="/login">Already have an account? Sign in</a>
    {% endif %}
  </div>
</body>
</html>"#;

const LOGIN_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>LeaseTrack — Login</title>
  <style>
    :root {
      --bg:        #ffffff;
      --bg-card:   #f6f8fa;
      --border:    #d0d7de;
      --border-sub:#eaeef2;
      --text:      #1f2328;
      --muted:     #656d76;
      --accent:    #0969da;
      --btn-bg:    #1a7f37;
      --btn-hover: #2da44e;
      --input-bg:  #ffffff;
      --err-bg:    #fff0ee;
      --err-fg:    #cf222e;
      --err-border:#f85149;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:        #0d1117;
        --bg-card:   #161b22;
        --border:    #30363d;
        --border-sub:#21262d;
        --text:      #c9d1d9;
        --muted:     #8b949e;
        --accent:    #58a6ff;
        --btn-bg:    #238636;
        --btn-hover: #2ea043;
        --input-bg:  #0d1117;
        --err-bg:    #3d1e1e;
        --err-fg:    #f85149;
        --err-border:#f85149;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: 'Courier New', monospace;
      background: var(--bg);
      color: var(--text);
      display: flex;
      align-items: center;
      justify-content: center;
      min-height: 100vh;
    }
    .card {
      background: var(--bg-card);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 2.5rem 2rem;
      width: 100%;
      max-width: 380px;
    }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: var(--accent); }
    .subtitle { font-size: 0.8rem; color: var(--muted); margin-bottom: 2rem; }
    label { display: block; font-size: 0.85rem; margin-bottom: 0.4rem; color: var(--muted); }
    input[type=text], input[type=password] {
      width: 100%;
      padding: 0.6rem 0.75rem;
      background: var(--input-bg);
      border: 1px solid var(--border);
      border-radius: 6px;
      color: var(--text);
      font-family: inherit;
      font-size: 0.95rem;
      margin-bottom: 1.25rem;
    }
    input[type=text]:focus, input[type=password]:focus { outline: none; border-color: var(--accent); }
    button {
      width: 100%;
      padding: 0.65rem;
      background: var(--btn-bg);
      border: none;
      border-radius: 6px;
      color: #fff;
      font-family: inherit;
      font-size: 1rem;
      cursor: pointer;
    }
    button:hover { background: var(--btn-hover); }
    .error {
      background: var(--err-bg);
      border: 1px solid var(--err-border);
      border-radius: 6px;
      color: var(--err-fg);
      padding: 0.6rem 0.75rem;
      font-size: 0.85rem;
      margin-bottom: 1rem;
    }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;position:fixed;top:0;width:100%;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  <div style="height:1.8rem"></div>
  {% endif %}
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Sign in with your email and API key</p>
    {% if error %}<div class="error">{{ error }}</div>{% endif %}
    <form method="post" action="/login">
      <label for="email">Email</label>
      <input type="text" id="email" name="email" autofocus placeholder="you@example.com" autocomplete="email">
      <label for="api_key">API Key</label>
      <input type="password" id="api_key" name="api_key" placeholder="••••••••••••••••">
      <button type="submit">Sign in</button>
    </form>
    <a href="/register" style="display:block;text-align:center;margin-top:1rem;font-size:0.8rem;color:var(--muted);text-decoration:none;">No account yet? Register</a>
    <a href="/forgot" style="display:block;text-align:center;margin-top:0.5rem;font-size:0.8rem;color:var(--muted);text-decoration:none;">Forgot your API key?</a>
  </div>
</body>
</html>"#;

const DASHBOARD_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>LeaseTrack — {{ car_name }}</title>
  <style>
    :root {
      --bg:          #ffffff;
      --bg-panel:    #f6f8fa;
      --bg-header:   #f6f8fa;
      --bg-input:    #ffffff;
      --bg-bar:      #eaeef2;
      --bg-bar-fut:  #d0d7de;
      --border:      #d0d7de;
      --border-sub:  #eaeef2;
      --text:        #1f2328;
      --muted:       #656d76;
      --accent:      #0969da;
      --green:       #1a7f37;
      --red:         #cf222e;
      --yellow:      #9a6700;
      --btn-bg:      #1a7f37;
      --btn-hover:   #2da44e;
      --err-bg:      #fff0ee;
      --err-fg:      #cf222e;
      --err-border:  #f85149;
      --ok-bg:       #dafbe1;
      --ok-fg:       #1a7f37;
      --ok-border:   #2da44e;
    }
    @media (prefers-color-scheme: dark) {
      :root {
        --bg:          #0d1117;
        --bg-panel:    #161b22;
        --bg-header:   #161b22;
        --bg-input:    #0d1117;
        --bg-bar:      #21262d;
        --bg-bar-fut:  #30363d;
        --border:      #30363d;
        --border-sub:  #21262d;
        --text:        #c9d1d9;
        --muted:       #8b949e;
        --accent:      #58a6ff;
        --green:       #3fb950;
        --red:         #f85149;
        --yellow:      #d29922;
        --btn-bg:      #238636;
        --btn-hover:   #2ea043;
        --err-bg:      #3d1e1e;
        --err-fg:      #f85149;
        --err-border:  #f85149;
        --ok-bg:       #1a2e1a;
        --ok-fg:       #3fb950;
        --ok-border:   #3fb950;
      }
    }
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body { font-family: 'Courier New', monospace; background: var(--bg); color: var(--text); min-height: 100vh; }
    header {
      background: var(--bg-header);
      border-bottom: 1px solid var(--border);
      padding: 0.75rem 1.5rem;
      display: flex; align-items: center; justify-content: space-between;
    }
    header h1 { font-size: 1.1rem; color: var(--accent); }
    header a { font-size: 0.8rem; color: var(--muted); text-decoration: none; }
    header a:hover { color: var(--text); }
    header a.brand { font-size: inherit; }
    header a.brand h1 { transition: opacity 0.15s; }
    header a.brand:hover h1 { opacity: 0.75; }
    main { max-width: 1100px; margin: 0 auto; padding: 1.5rem; display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
    @media(max-width:720px) { main { grid-template-columns: 1fr; } }
    .panel { background: var(--bg-panel); border: 1px solid var(--border); border-radius: 8px; padding: 1.25rem; }
    .panel h2 {
      font-size: 0.8rem; text-transform: uppercase; letter-spacing: 0.08em;
      color: var(--muted); border-bottom: 1px solid var(--border);
      padding-bottom: 0.5rem; margin-bottom: 1rem;
    }
    .info-row { display: flex; justify-content: space-between; align-items: center; padding: 0.3rem 0; font-size: 0.9rem; border-bottom: 1px solid var(--border-sub); }
    .info-row:last-child { border-bottom: none; }
    .info-row span:first-child { color: var(--muted); flex-shrink: 0; margin-right: 1rem; }
    .info-row span:last-child { font-weight: bold; }
    .info-row input[type=text], .info-row input[type=number], .info-row input[type=date] {
      background: var(--bg-input);
      border: 1px solid var(--border);
      border-radius: 4px;
      color: var(--text);
      font-family: inherit;
      font-size: 0.9rem;
      font-weight: bold;
      padding: 0.15rem 0.4rem;
      text-align: right;
      width: 10rem;
    }
    .info-row input:focus { outline: none; border-color: var(--accent); }
    .info-row .computed { color: var(--muted); font-size: 0.85rem; font-weight: normal; }
    .panel h2 { display: flex; align-items: center; justify-content: space-between; }
    .edit-toggle {
      font-size: 0.75rem; padding: 0.2rem 0.65rem;
      background: transparent; border: 1px solid var(--accent);
      border-radius: 4px; color: var(--accent); cursor: pointer;
      font-family: inherit;
    }
    .edit-toggle:hover { background: var(--accent); color: #fff; }
    .config-save {
      margin-top: 1rem;
      width: 100%;
      padding: 0.55rem;
      background: var(--btn-bg);
      border: none;
      border-radius: 6px;
      color: #fff;
      font-family: inherit;
      font-size: 0.9rem;
      cursor: pointer;
    }
    .config-save:hover { background: var(--btn-hover); }
    .bar-row { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 0.6rem; font-size: 0.85rem; }
    .bar-label { width: 3.5rem; text-align: right; color: var(--muted); flex-shrink: 0; }
    .bar-track { flex: 1; background: var(--bg-bar); border-radius: 3px; height: 18px; display: flex; }
    .bar-fill { height: 100%; border-radius: 3px; transition: width 0.3s; }
    .bar-fill.ok      { background: var(--green); }
    .bar-fill.over    { background: var(--red); }
    .bar-fill.current { background: var(--yellow); border-radius: 3px 0 0 3px; }
    .bar-fill.future  { background: var(--bg-bar-fut); }
    .bar-fill.proj    { background: var(--yellow); opacity: 0.35; border-radius: 0 3px 3px 0; }
    .bar-km { width: 5.5rem; text-align: right; color: var(--muted); font-size: 0.8rem; flex-shrink: 0; }
    .proj-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
    @media(max-width:720px) { .proj-grid { grid-template-columns: 1fr; } }
    .proj-card { background: var(--bg); border: 1px solid var(--border-sub); border-radius: 6px; padding: 1rem; }
    .proj-label { color: var(--muted); font-size: 0.8rem; margin-bottom: 0.4rem; }
    .proj-val { font-size: 1.5rem; font-weight: bold; margin-bottom: 0.25rem; }
    .proj-sub { color: var(--muted); font-size: 0.78rem; }
    .green { color: var(--green); }
    .red   { color: var(--red); }
    .records-table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
    .records-table th { color: var(--muted); font-weight: normal; text-align: left; padding: 0.3rem 0.4rem; border-bottom: 1px solid var(--border); }
    .records-table td { padding: 0.35rem 0.4rem; border-bottom: 1px solid var(--border-sub); }
    .records-table tr:last-child td { border-bottom: none; }
    .delta.pos { color: var(--green); }
    .delta.neg { color: var(--red); }
    .span-full { grid-column: 1 / -1; }
    .error-box { background: var(--err-bg); border: 1px solid var(--err-border); border-radius: 6px; color: var(--err-fg); padding: 1rem; }
    .success-box { background: var(--ok-bg); border: 1px solid var(--ok-border); border-radius: 6px; color: var(--ok-fg); padding: 0.6rem 0.75rem; font-size: 0.85rem; margin-bottom: 1rem; }
    .record-form input[type=number], .record-form input[type=date] {
      width: 100%; padding: 0.55rem 0.75rem;
      background: var(--bg-input); border: 1px solid var(--border); border-radius: 6px;
      color: var(--text); font-family: inherit; font-size: 0.9rem; margin-bottom: 0.75rem;
    }
    .record-form input:focus { outline: none; border-color: var(--accent); }
    .record-form label { font-size: 0.8rem; color: var(--muted); display: block; margin-bottom: 0.3rem; }
    .record-form button {
      width: 100%; padding: 0.6rem; background: var(--btn-bg); border: none;
      border-radius: 6px; color: #fff; font-family: inherit; font-size: 0.9rem; cursor: pointer;
    }
    .record-form button:hover { background: var(--btn-hover); }
  </style>
</head>
<body>
  {% if app_env and app_env != "production" %}
  <div style="background:#9a6700;color:#fff;text-align:center;padding:0.4rem;font-size:0.8rem;font-family:'Courier New',monospace;letter-spacing:0.05em;">
    ⚠ PREVIEW — {{ app_env }}
  </div>
  {% endif %}
  <header>
    <a class="brand" href="/dashboard"><h1>LeaseTrack — {{ car_name }}</h1></a>
    <div style="display:flex;align-items:center;gap:1rem;">
      <span style="font-size:0.8rem;color:var(--muted)">{{ email }}</span>
      <a href="/logout" style="font-size:0.8rem;padding:0.3rem 0.75rem;border:1px solid var(--border);border-radius:6px;color:var(--muted);text-decoration:none;">Sign out</a>
    </div>
  </header>

  {% if error %}
  <main><div class="panel span-full"><div class="error-box">{{ error }}</div></div></main>
  {% else %}
  <main>

    <!-- Car Info (editable) -->
    <div class="panel">
      <h2>Lease Info <button type="button" class="edit-toggle" id="cfg-edit-btn" onclick="toggleLeaseEdit()">Edit</button></h2>
      <!-- Read-only view -->
      <div id="cfg-view">
        <div class="info-row"><span>Car</span><span>{{ car_name }}</span></div>
        <div class="info-row"><span>Start date</span><span>{{ lease_start }}</span></div>
        <div class="info-row"><span>Years</span><span>{{ lease_years }}</span></div>
        <div class="info-row"><span>End date</span><span>{{ lease_end }}</span></div>
        <div class="info-row"><span>Allowed / year</span><span>{{ km_allowed_per_year|int }} km</span></div>
        <div class="info-row"><span>Start odometer</span><span>{{ start_odometer|int }} km</span></div>
        <div class="info-row"><span>Allowed total</span><span>{{ km_allowed_total|int }} km</span></div>
        <div class="info-row"><span>Total driven</span><span>{{ total_driven|int }} km</span></div>
        {% if last_odometer %}
        <div class="info-row"><span>Last reading</span><span>{{ last_odometer|int }} km ({{ last_date }})</span></div>
        {% endif %}
        {% if avg_daily_rate %}
        <div class="info-row"><span>Avg daily rate</span><span>{{ avg_daily_rate|int }} km/day</span></div>
        {% endif %}
      </div>
      <!-- Edit form (hidden by default) -->
      <form method="post" action="/web/config" id="cfg-form" style="display:none">
        <input type="hidden" name="csrf_token" value="{{ csrf_token }}">
        <div class="info-row">
          <span>Car</span>
          <input type="text" name="car_name" value="{{ car_name }}" maxlength="100" required>
        </div>
        <div class="info-row">
          <span>Start date</span>
          <input type="date" id="cfg-start" name="lease_start" value="{{ lease_start }}" required oninput="calcEnd()">
        </div>
        <div class="info-row">
          <span>Years</span>
          <input type="number" id="cfg-years" name="lease_years" value="{{ lease_years }}" min="1" max="10" required oninput="calcEnd()">
        </div>
        <div class="info-row">
          <span>End date</span>
          <span class="computed" id="cfg-end">{{ lease_end }}</span>
        </div>
        <div class="info-row">
          <span>Allowed / year</span>
          <input type="number" name="allowed_km_per_year" value="{{ km_allowed_per_year }}" min="1" required>
        </div>
        <div class="info-row">
          <span>Start odometer</span>
          <input type="number" name="start_odometer" value="{{ start_odometer }}" min="0" required>
        </div>
        <div class="info-row"><span>Allowed total</span><span>{{ km_allowed_total|int }} km</span></div>
        <div class="info-row"><span>Total driven</span><span>{{ total_driven|int }} km</span></div>
        {% if last_odometer %}
        <div class="info-row"><span>Last reading</span><span>{{ last_odometer|int }} km ({{ last_date }})</span></div>
        {% endif %}
        {% if avg_daily_rate %}
        <div class="info-row"><span>Avg daily rate</span><span>{{ avg_daily_rate|int }} km/day</span></div>
        {% endif %}
        <button type="submit" class="config-save">Save</button>
      </form>
    </div>
    <script>
    function toggleLeaseEdit() {
      const view = document.getElementById('cfg-view');
      const form = document.getElementById('cfg-form');
      const btn  = document.getElementById('cfg-edit-btn');
      const editing = form.style.display !== 'none';
      view.style.display = editing ? '' : 'none';
      form.style.display = editing ? 'none' : '';
      btn.textContent = editing ? 'Edit' : 'Cancel';
    }
    function calcEnd() {
      const start = document.getElementById('cfg-start').value;
      const years = parseInt(document.getElementById('cfg-years').value, 10);
      const el = document.getElementById('cfg-end');
      if (start && years >= 1) {
        const d = new Date(start);
        d.setFullYear(d.getFullYear() + years);
        el.textContent = d.toISOString().slice(0, 10);
      }
    }
    </script>

    <!-- Record Form -->
    <div class="panel">
      <h2>Record Odometer</h2>
      {% if record_success %}<div class="success-box">{{ record_success }}</div>{% endif %}
      {% if record_error %}<div class="error-box" style="margin-bottom:1rem">{{ record_error }}</div>{% endif %}
      <form method="post" action="/web/record" class="record-form">
        <input type="hidden" name="csrf_token" value="{{ csrf_token }}">
        <label for="odometer">Odometer (km)</label>
        <input type="number" id="odometer" name="odometer" min="0" placeholder="e.g. 25000" required>
        <label for="date">Date</label>
        <input type="date" id="date" name="date" value="{{ today }}" required>
        <button type="submit">Record</button>
      </form>
    </div>

    <!-- Projections -->
    <div class="panel span-full">
      <h2>Projections</h2>
      {% if proj_year_diff is defined and proj_year_diff is not none %}
      <div class="proj-grid">
        <div class="proj-card">
          <div class="proj-label">End of current year vs annual limit</div>
          <div class="proj-val {% if proj_year_diff > 0 %}red{% else %}green{% endif %}">
            {% if proj_year_diff > 0 %}+{% endif %}{{ proj_year_diff }} km
          </div>
          <div class="proj-sub">projected {{ proj_year_total|int }} km / {{ km_allowed_per_year|int }} km allowed</div>
        </div>
        {% if proj_total_diff is defined and proj_total_diff is not none %}
        <div class="proj-card">
          <div class="proj-label">End of lease vs total allowed</div>
          <div class="proj-val {% if proj_total_diff > 0 %}red{% else %}green{% endif %}">
            {% if proj_total_diff > 0 %}+{% endif %}{{ proj_total_diff }} km
          </div>
          <div class="proj-sub">projected vs {{ km_allowed_total|int }} km total allowed</div>
        </div>
        {% endif %}
      </div>
      {% else %}
      <p style="color:var(--muted);font-size:0.85rem">No projection data yet.</p>
      {% endif %}
    </div>

    <!-- Year Graph -->
    <div class="panel span-full">
      <h2>Km per year</h2>
      {% for y in years %}
      <div class="bar-row">
        <div class="bar-label">Year {{ y.year_num }}</div>
        <div class="bar-track">
          <div class="bar-fill {{ y.status }}" style="width:{{ y.pct }}%"></div>
          {% if y.proj_pct is defined and y.proj_pct is not none and y.proj_pct > 0 %}
          <div class="bar-fill proj" style="width:{{ y.proj_pct }}%"></div>
          {% endif %}
        </div>
        <div class="bar-km">
          {% if y.status != "future" %}{{ y.km }} km{% else %}—{% endif %}
        </div>
      </div>
      {% endfor %}
    </div>

    <!-- Records -->
    <div class="panel span-full">
      <h2>Odometer records</h2>
      <table class="records-table">
        <thead>
          <tr><th>Date</th><th>Odometer</th><th>Delta</th></tr>
        </thead>
        <tbody>
          {% for r in records %}
          <tr>
            <td>{{ r.date }}</td>
            <td>{{ r.odometer }} km</td>
            <td>
              {% if r.delta is defined and r.delta is not none %}
                <span class="delta {% if r.delta >= 0 %}pos{% else %}neg{% endif %}">
                  {% if r.delta >= 0 %}+{% endif %}{{ r.delta }} km
                </span>
              {% else %}—{% endif %}
            </td>
          </tr>
          {% endfor %}
        </tbody>
      </table>
    </div>

  </main>
  {% endif %}
</body>
</html>"#;
