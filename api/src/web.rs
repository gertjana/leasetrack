use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Form,
};
use axum_extra::extract::cookie::{Cookie, CookieJar};
use leasetrack_core::{add_record, compute_report_data, load_data, save_data};
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
        env.add_template_owned("login".to_string(), LOGIN_HTML.to_string())
            .expect("login template");
        env.add_template_owned("dashboard".to_string(), DASHBOARD_HTML.to_string())
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
    match std::env::var("API_KEY") {
        Ok(expected) => constant_time_eq(key.as_bytes(), expected.as_bytes()),
        Err(_) => true, // no API_KEY set → any key (or empty) is fine
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    render(&state, "login", context! { error => "" })
}

#[derive(Deserialize)]
pub struct LoginForm {
    api_key: String,
}

/// `POST /login`
pub async fn login_post(
    State(state): State<WebState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Response {
    if !is_valid_key(&form.api_key) {
        return render(
            &state,
            "login",
            context! { error => "Invalid API key. Please try again." },
        );
    }

    let cookie = Cookie::build((COOKIE_NAME, form.api_key))
        .path("/")
        .http_only(true)
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
        Err(e) => return render(state, "dashboard", context! {
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
            context! {
                year_num => y.year_num,
                km => km as u32,
                pct => pct,
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
        "dashboard",
        context! {
            error => "",
            car_name => report.car_name,
            lease_start => report.lease_start.to_string(),
            lease_end => report.lease_end.to_string(),
            lease_years => report.lease_years,
            km_allowed_per_year => report.km_allowed_per_year,
            km_allowed_total => report.km_allowed_total,
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
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: 'Courier New', monospace;
      background: #0d1117;
      color: #c9d1d9;
      display: flex;
      align-items: center;
      justify-content: center;
      min-height: 100vh;
    }
    .card {
      background: #161b22;
      border: 1px solid #30363d;
      border-radius: 8px;
      padding: 2.5rem 2rem;
      width: 100%;
      max-width: 380px;
    }
    h1 { font-size: 1.4rem; margin-bottom: 0.25rem; color: #58a6ff; }
    .subtitle { font-size: 0.8rem; color: #8b949e; margin-bottom: 2rem; }
    label { display: block; font-size: 0.85rem; margin-bottom: 0.4rem; color: #8b949e; }
    input[type=password] {
      width: 100%;
      padding: 0.6rem 0.75rem;
      background: #0d1117;
      border: 1px solid #30363d;
      border-radius: 6px;
      color: #c9d1d9;
      font-family: inherit;
      font-size: 0.95rem;
      margin-bottom: 1.25rem;
    }
    input[type=password]:focus { outline: none; border-color: #58a6ff; }
    button {
      width: 100%;
      padding: 0.65rem;
      background: #238636;
      border: none;
      border-radius: 6px;
      color: #fff;
      font-family: inherit;
      font-size: 1rem;
      cursor: pointer;
    }
    button:hover { background: #2ea043; }
    .error {
      background: #3d1e1e;
      border: 1px solid #f85149;
      border-radius: 6px;
      color: #f85149;
      padding: 0.6rem 0.75rem;
      font-size: 0.85rem;
      margin-bottom: 1rem;
    }
  </style>
</head>
<body>
  <div class="card">
    <h1>LeaseTrack</h1>
    <p class="subtitle">Enter your API key to continue</p>
    {% if error %}<div class="error">{{ error }}</div>{% endif %}
    <form method="post" action="/login">
      <label for="api_key">API Key</label>
      <input type="password" id="api_key" name="api_key" autofocus placeholder="••••••••••••••••">
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
    *, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
    body {
      font-family: 'Courier New', monospace;
      background: #0d1117;
      color: #c9d1d9;
      min-height: 100vh;
    }
    header {
      background: #161b22;
      border-bottom: 1px solid #30363d;
      padding: 0.75rem 1.5rem;
      display: flex;
      align-items: center;
      justify-content: space-between;
    }
    header h1 { font-size: 1.1rem; color: #58a6ff; }
    header a { font-size: 0.8rem; color: #8b949e; text-decoration: none; }
    header a:hover { color: #c9d1d9; }
    main { max-width: 1100px; margin: 0 auto; padding: 1.5rem; display: grid; grid-template-columns: 1fr 1fr; gap: 1rem; }
    @media(max-width:720px) { main { grid-template-columns: 1fr; } }
    .panel {
      background: #161b22;
      border: 1px solid #30363d;
      border-radius: 8px;
      padding: 1.25rem;
    }
    .panel h2 {
      font-size: 0.8rem;
      text-transform: uppercase;
      letter-spacing: 0.08em;
      color: #8b949e;
      border-bottom: 1px solid #30363d;
      padding-bottom: 0.5rem;
      margin-bottom: 1rem;
    }
    .info-row { display: flex; justify-content: space-between; padding: 0.3rem 0; font-size: 0.9rem; border-bottom: 1px solid #21262d; }
    .info-row:last-child { border-bottom: none; }
    .info-row span:first-child { color: #8b949e; }
    .info-row span:last-child { color: #c9d1d9; font-weight: bold; }
    /* Bar chart */
    .bar-row { display: flex; align-items: center; gap: 0.75rem; margin-bottom: 0.6rem; font-size: 0.85rem; }
    .bar-label { width: 3.5rem; text-align: right; color: #8b949e; flex-shrink: 0; }
    .bar-track { flex: 1; background: #21262d; border-radius: 3px; height: 18px; position: relative; }
    .bar-fill { height: 100%; border-radius: 3px; transition: width 0.3s; }
    .bar-fill.ok      { background: #238636; }
    .bar-fill.over    { background: #f85149; }
    .bar-fill.current { background: #d29922; }
    .bar-fill.future  { background: #30363d; }
    .bar-km { width: 5.5rem; text-align: right; color: #8b949e; font-size: 0.8rem; flex-shrink: 0; }
    /* Projection */
    .proj-row { padding: 0.5rem 0; font-size: 0.9rem; border-bottom: 1px solid #21262d; }
    .proj-row:last-child { border-bottom: none; }
    .proj-label { color: #8b949e; font-size: 0.8rem; margin-bottom: 0.2rem; }
    .proj-val { font-size: 1rem; font-weight: bold; }
    .green { color: #3fb950; }
    .red   { color: #f85149; }
    /* Records */
    .records-table { width: 100%; border-collapse: collapse; font-size: 0.85rem; }
    .records-table th { color: #8b949e; font-weight: normal; text-align: left; padding: 0.3rem 0.4rem; border-bottom: 1px solid #30363d; }
    .records-table td { padding: 0.35rem 0.4rem; border-bottom: 1px solid #21262d; }
    .records-table tr:last-child td { border-bottom: none; }
    .delta.pos { color: #3fb950; }
    .delta.neg { color: #f85149; }
    .span-full { grid-column: 1 / -1; }
    .error-box {
      background: #3d1e1e;
      border: 1px solid #f85149;
      border-radius: 6px;
      color: #f85149;
      padding: 1rem;
    }
    .success-box {
      background: #1a2e1a;
      border: 1px solid #3fb950;
      border-radius: 6px;
      color: #3fb950;
      padding: 0.6rem 0.75rem;
      font-size: 0.85rem;
      margin-bottom: 1rem;
    }
    .record-form input[type=number], .record-form input[type=date] {
      width: 100%;
      padding: 0.55rem 0.75rem;
      background: #0d1117;
      border: 1px solid #30363d;
      border-radius: 6px;
      color: #c9d1d9;
      font-family: inherit;
      font-size: 0.9rem;
      margin-bottom: 0.75rem;
    }
    .record-form input:focus { outline: none; border-color: #58a6ff; }
    .record-form label { font-size: 0.8rem; color: #8b949e; display: block; margin-bottom: 0.3rem; }
    .record-form button {
      width: 100%;
      padding: 0.6rem;
      background: #238636;
      border: none;
      border-radius: 6px;
      color: #fff;
      font-family: inherit;
      font-size: 0.9rem;
      cursor: pointer;
    }
    .record-form button:hover { background: #2ea043; }
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

    <!-- Car Info -->
    <div class="panel">
      <h2>Lease Info</h2>
      <div class="info-row"><span>Car</span><span>{{ car_name }}</span></div>
      <div class="info-row"><span>Period</span><span>{{ lease_start }} → {{ lease_end }}</span></div>
      <div class="info-row"><span>Years</span><span>{{ lease_years }}</span></div>
      <div class="info-row"><span>Allowed / year</span><span>{{ km_allowed_per_year|int }} km</span></div>
      <div class="info-row"><span>Allowed total</span><span>{{ km_allowed_total|int }} km</span></div>
      <div class="info-row"><span>Total driven</span><span>{{ total_driven|int }} km</span></div>
      {% if last_odometer %}
      <div class="info-row"><span>Last reading</span><span>{{ last_odometer|int }} km ({{ last_date }})</span></div>
      {% endif %}
      {% if avg_daily_rate %}
      <div class="info-row"><span>Avg daily rate</span><span>{{ avg_daily_rate|int }} km/day</span></div>
      {% endif %}
    </div>

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
    <div class="panel">
      <h2>Projections</h2>
      {% if proj_year_diff is defined and proj_year_diff is not none %}
      <div class="proj-row">
        <div class="proj-label">End of current year vs annual limit</div>
        <div class="proj-val {% if proj_year_diff > 0 %}red{% else %}green{% endif %}">
          {% if proj_year_diff > 0 %}+{% endif %}{{ proj_year_diff }} km &nbsp;
          <small style="color:#8b949e">(proj. {{ proj_year_total|int }} km)</small>
        </div>
      </div>
      {% endif %}
      {% if proj_total_diff is defined and proj_total_diff is not none %}
      <div class="proj-row">
        <div class="proj-label">End of lease vs total allowed</div>
        <div class="proj-val {% if proj_total_diff > 0 %}red{% else %}green{% endif %}">
          {% if proj_total_diff > 0 %}+{% endif %}{{ proj_total_diff }} km
        </div>
      </div>
      {% endif %}
      {% if proj_year_diff is not defined or proj_year_diff is none %}
      <p style="color:#8b949e;font-size:0.85rem">No projection data yet.</p>
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
