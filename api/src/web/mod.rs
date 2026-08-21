//! The LeaseTrack web interface, rendered server-side with Topcoat.
//!
//! The JSON API stays on axum; this router is mounted as that router's
//! fallback, so it serves every path the API does not claim. See
//! [`crate::main`] for the wiring.

pub mod auth;
pub mod dashboard;
pub mod layout;
pub mod session_store;
pub mod setup;

use topcoat::{
    Result,
    context::{Cx, app_context},
    cookie::RouterBuilderCookieExt,
    router::{
        Body, HeaderValue, Next, Router, RouterBuilderDiscoverExt, StatusCode, header,
        content::{Css, Js},
        response::{IntoResponse, Response},
        layer, route,
        request::{extensions, headers, method},
    },
    session::{self, RouterBuilderSessionExt, SessionConfig},
    view::view,
};

use crate::ratelimit::{self, RateLimiter};
use session_store::{SESSION_TTL, SessionStore};

// ─── Application state ────────────────────────────────────────────────────────

/// Which deployment this is. Anything other than `production` gets a banner.
pub struct AppEnv(String);

impl AppEnv {
    pub fn from_env() -> Self {
        AppEnv(std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string()))
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    pub fn is_production(&self) -> bool {
        self.0 == "production"
    }
}

pub fn app_env(cx: &Cx) -> &AppEnv {
    app_context(cx)
}

pub fn store(cx: &Cx) -> &SessionStore {
    app_context(cx)
}

pub fn limiter(cx: &Cx) -> &RateLimiter {
    app_context(cx)
}

/// The signed-in account, or `None` when there is no live session.
pub async fn current_email(cx: &Cx) -> Result<Option<String>> {
    let Some(token_hash) = session::token_hash(cx).await? else {
        return Ok(None);
    };
    Ok(store(cx).email_for(&token_hash))
}

/// Public base URL used to build links in outgoing email.
pub fn base_url() -> String {
    std::env::var("APP_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string())
        .trim_end_matches('/')
        .to_string()
}

// ─── Router ───────────────────────────────────────────────────────────────────

/// Build the web router. `limiter` is shared with the rest of the process so
/// the per-address email quota is counted once, not once per router.
pub fn router(limiter: RateLimiter) -> Router {
    Router::builder()
        .cookies()
        .sessions(SessionConfig::builder().lifetime(SESSION_TTL).build())
        .app_context(AppEnv::from_env())
        .app_context(SessionStore::new())
        .app_context(limiter)
        .discover()
        .build()
}

// ─── Static assets ────────────────────────────────────────────────────────────

const APP_CSS: &str = include_str!("app.css");
const SETUP_JS: &str = include_str!("setup.js");
const DASHBOARD_JS: &str = include_str!("dashboard.js");

/// Assets are served from the binary rather than declared with `asset!`, which
/// would require the `topcoat` CLI at build time. They carry no content hash,
/// so the cache window is kept short enough that a deploy is picked up quickly.
fn cached(body: impl IntoResponse, cx: &Cx) -> Result<Response> {
    let mut response = body.into_response(cx)?;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    Ok(response)
}

#[route(GET "/assets/app.css")]
async fn app_css(cx: &Cx) -> Result<Response> {
    cached(Css(APP_CSS), cx)
}

#[route(GET "/assets/setup.js")]
async fn setup_js(cx: &Cx) -> Result<Response> {
    cached(Js(SETUP_JS), cx)
}

#[route(GET "/assets/dashboard.js")]
async fn dashboard_js(cx: &Cx) -> Result<Response> {
    cached(Js(DASHBOARD_JS), cx)
}

// ─── Rate limiting ────────────────────────────────────────────────────────────

/// Per-IP limits on the unauthenticated endpoints.
///
/// GET requests pass through untouched so that merely loading a page does not
/// consume anyone's quota. `/login` is bucketed separately from the
/// mail-sending endpoints so that a burst of sign-in attempts cannot exhaust
/// someone's ability to request a reset.
#[layer("/")]
async fn rate_limit(cx: &Cx, body: Body, next: Next<'_>) -> Result<Response> {
    if method(cx) != topcoat::router::Method::POST {
        return next.run(cx, body).await;
    }

    let path = topcoat::router::request::uri(cx).path().to_owned();
    let bucketed = match path.as_str() {
        // `/reset` shares the sign-in quota: it is unauthenticated and, now that
        // redeeming happens on POST, it is the other endpoint that trades a
        // secret for account access.
        "/login" | "/reset" => Some(("login", ratelimit::LOGIN_PER_IP)),
        "/register" | "/forgot" => Some(("email", ratelimit::EMAIL_PER_IP)),
        _ => None,
    };

    let Some((bucket, quota)) = bucketed else {
        return next.run(cx, body).await;
    };

    let client = ratelimit::client_key_from(headers(cx), extensions(cx));
    match limiter(cx).check(&format!("{bucket}:ip:{client}"), quota) {
        Ok(()) => next.run(cx, body).await,
        Err(retry) => {
            tracing::warn!("rate limited {bucket} for {client}");
            too_many_requests(cx, retry).await
        }
    }
}

async fn too_many_requests(cx: &Cx, retry_after: u64) -> Result<Response> {
    let minutes = retry_after.div_ceil(60);
    let retry_header = HeaderValue::from_str(&retry_after.to_string())
        .unwrap_or_else(|_| HeaderValue::from_static("60"));

    view! { cx =>
        (StatusCode::TOO_MANY_REQUESTS)
        ((header::RETRY_AFTER, retry_header))
        layout::document(
            title: "LeaseTrack — Too many requests",
            body_class: "centered",
            <div class="card">
                <h1>"Too many requests"</h1>
                <p class="subtitle">
                    "Please wait about " (minutes) " minute(s) and try again."
                </p>
                <a class="back" href="/login">"Back to sign in"</a>
            </div>
        )
    }?
    .into_response(cx)
}
