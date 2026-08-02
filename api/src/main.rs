use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Json},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use chrono::{Local, NaiveDate};
use leasetrack_core::{
    add_record, compute_report_data, compute_year_stats, load_data, save_data, LeaseConfig,
    LeaseData,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::{AllowOrigin, CorsLayer};

// ─── Auth Middleware ─────────────────────────────────────────────────────────

/// If the `API_KEY` environment variable is set, every request (except `/health`)
/// must include an `X-Api-Key` header with the matching value.
async fn check_api_key(req: Request<Body>, next: Next) -> Response {
    if let Ok(expected) = std::env::var("API_KEY") {
        let provided = req
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        // Constant-time comparison to prevent timing attacks.
        if !constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            let body = serde_json::json!({"error": "missing or invalid X-Api-Key header"});
            return (StatusCode::UNAUTHORIZED, Json(body)).into_response();
        }
    }
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
async fn init(Json(req): Json<InitRequest>) -> Result<impl IntoResponse, AppError> {
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

    save_data(&data).map_err(AppError::from)?;

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
async fn record(Json(req): Json<RecordRequest>) -> Result<impl IntoResponse, AppError> {
    let mut data = load_data().map_err(AppError::from)?;

    let date = req.date.unwrap_or_else(|| Local::now().date_naive());
    let warnings = add_record(&mut data, req.odometer, date).map_err(AppError::from)?;
    save_data(&data).map_err(AppError::from)?;

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
async fn report() -> Result<impl IntoResponse, AppError> {
    let data = load_data().map_err(AppError::from)?;
    let report = compute_report_data(&data);
    Ok(Json(report))
}

/// `GET /graph` — per-year stats suitable for rendering a chart.
async fn graph() -> Result<impl IntoResponse, AppError> {
    let data = load_data().map_err(AppError::from)?;
    let stats = compute_year_stats(&data);
    Ok(Json(serde_json::json!({
        "car_name": data.config.car_name,
        "allowed_km_per_year": data.config.allowed_km_per_year,
        "years": stats,
    })))
}

/// `GET /list` — all recorded odometer readings plus config.
async fn list() -> Result<impl IntoResponse, AppError> {
    let data = load_data().map_err(AppError::from)?;
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
        // Reject bodies larger than 64 KB — more than enough for lease data.
        .layer(DefaultBodyLimit::max(65_536))
        .layer(cors);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let addr = format!("0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    tracing::info!("leasetrack-api listening on {}", addr);
    if std::env::var("API_KEY").is_ok() {
        tracing::info!("API key authentication is enabled");
    } else {
        tracing::warn!("API key authentication is DISABLED — set API_KEY env var to enable");
    }
    axum::serve(listener, app).await.expect("server error");
}
