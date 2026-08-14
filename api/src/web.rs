use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use leasetrack_core::{add_record, authenticate_user, compute_report_data, find_user_by_key, load_data, save_data, LeaseConfig, LeaseData};
use minijinja::{context, Environment};
use serde::Deserialize;
use std::sync::Arc;

// ─── State ────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebState {
    pub env: Arc<Environment<'static>>,
}

impl WebState {
    pub fn new() -> Self {
        let mut env = Environment::new();
        env.add_template_owned("login.html".to_string(), LOGIN_HTML.to_string())
            .expect("login template");
        env.add_template_owned("dashboard.html".to_string(), DASHBOARD_HTML.to_string())
            .expect("dashboard template");
        WebState {
            env: Arc::new(env),
        }
    }
}

// ─── Cookie helpers ───────────────────────────────────────────────────────────

const COOKIE_NAME: &str = "lt_api_key";

fn api_key_from_cookie(jar: &CookieJar) -> Option<String> {
    jar.get(COOKIE_NAME).map(|c| c.value().to_owned())
}

fn is_valid_key(key: &str) -> bool {
    find_user_by_key(key).is_some()
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

/// `GET /` — redirect to dashboard if already authenticated, else login.
pub async fn index(jar: CookieJar) -> Response {
    if api_key_from_cookie(&jar)
        .as_deref()
        .map(is_valid_key)
        .unwrap_or(false)
    {
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
    if api_key_from_cookie(&jar)
        .as_deref()
        .map(is_valid_key)
        .unwrap_or(false)
    {
        return Redirect::to("/dashboard").into_response();
    }
    render(&state, "login.html", context! { error => "" })
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
    if authenticate_user(&form.email, &form.api_key).is_none() {
        return render(
            &state,
            "login.html",
            context! { error => "Invalid email or API key. Please try again." },
        );
    }

    let cookie = Cookie::build((COOKIE_NAME, form.api_key))
        .path("/")
        .http_only(true)
        .secure(true)
        .build();

    (jar.add(cookie), Redirect::to("/dashboard")).into_response()
}

/// `GET /logout`
pub async fn logout(jar: CookieJar) -> Response {
    let removal = Cookie::build((COOKIE_NAME, "")).path("/").build();
    (jar.remove(removal), Redirect::to("/login")).into_response()
}

#[derive(Deserialize)]
pub struct RecordForm {
    odometer: String,
    date: String,
}

/// `POST /web/record`
pub async fn web_record(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<RecordForm>,
) -> Response {
    let key = match api_key_from_cookie(&jar) {
        Some(k) if is_valid_key(&k) => k,
        _ => return Redirect::to("/login").into_response(),
    };

    let odometer: Result<u32, _> = form.odometer.trim().parse();
    let date = chrono::NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d");

    let (record_success, record_error) = match (odometer, date) {
        (Ok(odo), Ok(d)) => {
            let mut data = match load_data() {
                Ok(d) => d,
                Err(e) => {
                    return render_dashboard(&state, jar, key, Some(e), None).await;
                }
            };
            match add_record(&mut data, odo, d) {
                Ok(warnings) => {
                    let _ = save_data(&data);
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

    render_dashboard(&state, jar, key, record_error, record_success).await
}

#[derive(Deserialize)]
pub struct ConfigForm {
    car_name: String,
    lease_start: String,
    lease_years: String,
    allowed_km_per_year: String,
    start_odometer: String,
}

/// `POST /web/config`
pub async fn web_config(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<ConfigForm>,
) -> Response {
    let key = match api_key_from_cookie(&jar) {
        Some(k) if is_valid_key(&k) => k,
        _ => return Redirect::to("/login").into_response(),
    };

    let parse_err = |msg: &str| render_dashboard(&state, jar.clone(), key.clone(), Some(msg.into()), None);

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

    let mut data = match load_data() {
        Ok(d) => d,
        Err(_) => LeaseData { config: LeaseConfig { car_name: car_name.clone(), lease_start, lease_years, allowed_km_per_year, start_odometer }, records: vec![] },
    };

    data.config = LeaseConfig { car_name, lease_start, lease_years, allowed_km_per_year, start_odometer };

    if let Err(e) = save_data(&data) {
        return render_dashboard(&state, jar, key, Some(e), None).await;
    }

    render_dashboard(&state, jar, key, None, Some("Configuration saved.".into())).await
}

/// `GET /dashboard`
pub async fn dashboard(
    State(state): State<WebState>,
    jar: CookieJar,
) -> Response {
    let key = match api_key_from_cookie(&jar) {
        Some(k) if is_valid_key(&k) => k,
        _ => return Redirect::to("/login").into_response(),
    };
    render_dashboard(&state, jar, key, None, None).await
}

async fn render_dashboard(
    state: &WebState,
    _jar: CookieJar,
    _key: String,
    record_error: Option<String>,
    record_success: Option<String>,
) -> Response {
    let today = chrono::Local::now().date_naive().to_string();

    let data = match load_data() {
        Ok(d) => d,
        Err(e) => return render(state, "dashboard.html", context! {
            error => e,
            car_name => "",
            today => today,
            record_error => "",
            record_success => "",
        }),
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
  <header>
    <h1>LeaseTrack — {{ car_name }}</h1>
    <a href="/logout">Sign out</a>
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
