mod web;
mod email;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Extension, Json},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use tower_cookies::CookieManagerLayer;
use chrono::{Local, NaiveDate};
use leasetrack_core::{
    add_record, compute_report_data, compute_year_stats, find_user_by_key, load_data,
    load_user_data, load_users, save_data, save_user_data, LeaseConfig, LeaseData,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};

// ─── Auth Middleware ─────────────────────────────────────────────────────────

/// Who a JSON API request is acting as.
///
/// Requests authenticated with a registered user's key act on that user's own
/// data file, mirroring the web UI. The legacy single-tenant modes (the
/// `API_KEY` env var, or an unconfigured development instance) have no user
/// attached and keep operating on the shared data file.
#[derive(Clone, Debug)]
enum Identity {
    User(String),
    Legacy,
}

impl Identity {
    fn load(&self) -> Result<LeaseData, String> {
        match self {
            Identity::User(email) => load_user_data(email),
            Identity::Legacy => load_data(),
        }
    }

    fn save(&self, data: &LeaseData) -> Result<(), String> {
        match self {
            Identity::User(email) => save_user_data(email, data),
            Identity::Legacy => save_data(data),
        }
    }
}

/// If a users file exists and has users, every request must include an
/// `X-Api-Key` header with a key matching a registered user.
/// Falls back to the legacy `API_KEY` env var if the users file is empty or missing.
async fn check_api_key(mut req: Request<Body>, next: Next) -> Response {
    let provided = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Primary: validate against users file. The matched user's own data file is
    // what the handler will read and write.
    if let Some(user) = find_user_by_key(provided) {
        req.extensions_mut().insert(Identity::User(user.email));
        return next.run(req).await;
    }

    // Once any user is registered, a valid key is mandatory. Without this the
    // request would fall through to the open development path below and get
    // access to the shared data file.
    let users_exist = load_users().map(|u| !u.users.is_empty()).unwrap_or(false);
    if users_exist {
        let body = serde_json::json!({"error": "missing or invalid X-Api-Key header"});
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }

    // Fallback: legacy single API_KEY env var (no users file configured)
    if let Ok(expected) = std::env::var("API_KEY") {
        if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            req.extensions_mut().insert(Identity::Legacy);
            return next.run(req).await;
        }
        let body = serde_json::json!({"error": "missing or invalid X-Api-Key header"});
        return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
    }

    // No users file and no API_KEY set → open (development mode)
    req.extensions_mut().insert(Identity::Legacy);
    next.run(req).await
}

/// Constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ─── Error Handling ───────────────────────────────────────────────────────────

struct AppError(String);

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError(s)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.0 });
        (StatusCode::BAD_REQUEST, Json(body)).into_response()
    }
}

// ─── Request / Response Types ─────────────────────────────────────────────────

#[derive(Deserialize)]
struct InitRequest {
    car_name: String,
    lease_start: NaiveDate,
    lease_years: u32,
    allowed_km_per_year: u32,
    #[serde(default)]
    start_odometer: u32,
}

#[derive(Deserialize)]
struct RecordRequest {
    odometer: u32,
    date: Option<NaiveDate>,
}

#[derive(Serialize)]
struct RecordResponse {
    message: String,
    warnings: Vec<String>,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// `POST /init` — create or overwrite lease configuration.
///
/// Body (JSON):
/// ```json
/// {
///   "car_name": "Tesla Model 3",
///   "lease_start": "2024-01-01",
///   "lease_years": 4,
///   "allowed_km_per_year": 20000,
///   "start_odometer": 0
/// }
/// ```
async fn init(
    Extension(identity): Extension<Identity>,
    Json(req): Json<InitRequest>,
) -> Result<impl IntoResponse, AppError> {
    if req.car_name.trim().is_empty() {
        return Err(AppError("car_name cannot be empty".into()));
    }
    if req.car_name.len() > 100 {
        return Err(AppError("car_name must be 100 characters or fewer".into()));
    }
    if !(1..=10).contains(&req.lease_years) {
        return Err(AppError("lease_years must be between 1 and 10".into()));
    }
    if req.allowed_km_per_year == 0 {
        return Err(AppError("allowed_km_per_year must be greater than 0".into()));
    }

    let data = LeaseData {
        config: LeaseConfig {
            car_name: req.car_name.clone(),
            lease_start: req.lease_start,
            lease_years: req.lease_years,
            allowed_km_per_year: req.allowed_km_per_year,
            start_odometer: req.start_odometer,
        },
        records: Vec::new(),
    };

    identity.save(&data).map_err(AppError::from)?;

    Ok(Json(serde_json::json!({
        "message": format!("Lease car '{}' configured.", req.car_name),
        "config": data.config,
    })))
}

/// `POST /record` — add an odometer reading.
///
/// Body (JSON):
/// ```json
/// { "odometer": 25000, "date": "2025-06-15" }
/// ```
/// `date` is optional and defaults to today.
async fn record(
    Extension(identity): Extension<Identity>,
    Json(req): Json<RecordRequest>,
) -> Result<impl IntoResponse, AppError> {
    let mut data = identity.load().map_err(AppError::from)?;

    let date = req.date.unwrap_or_else(|| Local::now().date_naive());
    let warnings = add_record(&mut data, req.odometer, date).map_err(AppError::from)?;
    identity.save(&data).map_err(AppError::from)?;

    Ok(Json(RecordResponse {
        message: format!(
            "Recorded {} km on {}",
            leasetrack_core::fmt_km(req.odometer),
            date
        ),
        warnings,
    }))
}

/// `GET /report` — full report with projections (JSON).
async fn report(Extension(identity): Extension<Identity>) -> Result<impl IntoResponse, AppError> {
    let data = identity.load().map_err(AppError::from)?;
    let report = compute_report_data(&data);
    Ok(Json(report))
}

/// `GET /graph` — per-year stats suitable for rendering a chart.
async fn graph(Extension(identity): Extension<Identity>) -> Result<impl IntoResponse, AppError> {
    let data = identity.load().map_err(AppError::from)?;
    let stats = compute_year_stats(&data);
    Ok(Json(serde_json::json!({
        "car_name": data.config.car_name,
        "allowed_km_per_year": data.config.allowed_km_per_year,
        "years": stats,
    })))
}

/// `GET /list` — all recorded odometer readings plus config.
async fn list(Extension(identity): Extension<Identity>) -> Result<impl IntoResponse, AppError> {
    let data = identity.load().map_err(AppError::from)?;
    Ok(Json(data))
}

// ─── Main ─────────────────────────────────────────────────────────────────────

/// Build a CORS layer from the `CORS_ORIGINS` environment variable.
///
/// - Not set → no cross-origin requests allowed (same-origin only for browsers).
/// - `CORS_ORIGINS=*` → any origin (permissive; only use in development).
/// - `CORS_ORIGINS=https://app.example.com,https://admin.example.com` → listed origins only.
fn build_cors() -> CorsLayer {
    match std::env::var("CORS_ORIGINS").as_deref() {
        Ok("*") => CorsLayer::permissive(),
        Ok(origins) => {
            let parsed: Vec<axum::http::HeaderValue> = origins
                .split(',')
                .filter_map(|o| o.trim().parse().ok())
                .collect();
            CorsLayer::new().allow_origin(AllowOrigin::list(parsed))
        }
        Err(_) => CorsLayer::new(), // deny all cross-origin by default
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "leasetrack_api=info,tower_http=info".into()),
        )
        .init();

    let web_state = web::WebState::new();

    let web_routes = Router::new()
        .route("/", get(web::index))
        .route("/login", get(web::login_page).post(web::login_post))
        .route("/logout", get(web::logout))
        .route("/register", get(web::register_page).post(web::register_post))
        .route("/setup", get(web::setup_page).post(web::setup_post))
        .route("/forgot", get(web::forgot_page).post(web::forgot_post))
        .route("/reset", get(web::reset_page))
        .route("/dashboard", get(web::dashboard))
        .route("/web/record", post(web::web_record))
        .route("/web/config", post(web::web_config))
        .with_state(web_state);

    let cors = build_cors();

    // Protected routes require X-Api-Key when API_KEY env var is set.
    let protected = Router::new()
        .route("/init", post(init))
        .route("/record", post(record))
        .route("/report", get(report))
        .route("/graph", get(graph))
        .route("/list", get(list))
        .layer(middleware::from_fn(check_api_key));

    let app = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .merge(web_routes)
        // Reject bodies larger than 64 KB — more than enough for lease data.
        .layer(DefaultBodyLimit::max(65_536))
        .layer(CookieManagerLayer::new())
        .layer(cors);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    tracing::info!("leasetrack-api listening on {}", addr);
    tracing::info!("Users file: {}", leasetrack_core::users_path().display());
    match leasetrack_core::migrate_users_to_hashed_keys() {
        Ok(0) => {}
        Ok(n) => tracing::info!("Migrated {n} user(s) to hashed API keys"),
        Err(e) => tracing::error!("Failed to migrate users file to hashed keys: {e}"),
    }
    let users = leasetrack_core::load_users().unwrap_or_default();
    if !users.users.is_empty() {
        tracing::info!("User authentication enabled ({} user(s))", users.users.len());
    } else if std::env::var("API_KEY").is_ok() {
        tracing::info!("API key authentication enabled (legacy API_KEY env var)");
    } else {
        tracing::warn!("Authentication is DISABLED — add users to the users file or set API_KEY");
    }
    axum::serve(listener, app).await.expect("server error");
}
