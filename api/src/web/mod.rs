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
        Body, HeaderValue, Next, Router, RouterBuilderDiscoverExt, StatusCode,
        content::{Css, Js},
        header, layer,
        request::{extensions, headers, method},
        response::{IntoResponse, Response},
        route,
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

/// Which quota bucket a POST to `path` is counted against, if any.
///
/// Buckets are deliberately independent: exhausting one must not deny access
/// to another. In particular `/reset` is counted separately from `/login`,
/// because a user recovering an account has usually just spent their sign-in
/// attempts failing, and a shared counter would reject the reset they came for.
fn bucket_for(path: &str) -> Option<(&'static str, ratelimit::Quota)> {
    match path {
        "/login" => Some(("login", ratelimit::LOGIN_PER_IP)),
        "/reset" => Some(("reset", ratelimit::RESET_PER_IP)),
        "/register" | "/forgot" => Some(("email", ratelimit::EMAIL_PER_IP)),
        _ => None,
    }
}

/// Per-IP limits on the unauthenticated endpoints.
///
/// GET requests pass through untouched so that merely loading a page does not
/// consume anyone's quota.
#[layer("/")]
async fn rate_limit(cx: &Cx, body: Body, next: Next<'_>) -> Result<Response> {
    if method(cx) != topcoat::router::Method::POST {
        return next.run(cx, body).await;
    }

    let path = topcoat::router::request::uri(cx).path().to_owned();

    let Some((bucket, quota)) = bucket_for(&path) else {
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::bucket_for;
    use crate::ratelimit;

    #[test]
    fn the_unauthenticated_endpoints_are_bucketed() {
        assert_eq!(bucket_for("/login").unwrap().0, "login");
        assert_eq!(bucket_for("/reset").unwrap().0, "reset");
        assert_eq!(bucket_for("/register").unwrap().0, "email");
        assert_eq!(bucket_for("/forgot").unwrap().0, "email");
    }

    /// The regression this guards: folding `/reset` into the sign-in bucket
    /// means a user who has just failed several sign-ins — the normal
    /// precondition for using a reset link — is refused the reset itself.
    #[test]
    fn sign_in_and_reset_are_counted_separately() {
        let login = bucket_for("/login").expect("bucketed");
        let reset = bucket_for("/reset").expect("bucketed");

        assert_ne!(
            login.0, reset.0,
            "sharing a bucket blocks account recovery after failed sign-ins"
        );
    }

    #[test]
    fn account_creation_and_reset_requests_share_the_mail_bucket() {
        // Both send mail, so one budget covers them jointly.
        assert_eq!(bucket_for("/register").unwrap().0, bucket_for("/forgot").unwrap().0);
    }

    #[test]
    fn each_endpoint_uses_its_documented_quota() {
        assert_eq!(bucket_for("/login").unwrap().1.max, ratelimit::LOGIN_PER_IP.max);
        assert_eq!(bucket_for("/reset").unwrap().1.max, ratelimit::RESET_PER_IP.max);
        assert_eq!(bucket_for("/register").unwrap().1.max, ratelimit::EMAIL_PER_IP.max);
    }

    /// Mail is the scarcer resource, so its budget is tighter than sign-in's.
    /// Both are constants, so this is checked at compile time.
    const _: () = assert!(ratelimit::EMAIL_PER_IP.max < ratelimit::LOGIN_PER_IP.max);

    #[test]
    fn everything_else_is_unlimited() {
        for path in [
            "/",
            "/dashboard",
            "/setup",
            "/logout",
            "/web/record",
            "/web/config",
            "/assets/app.css",
            "/health",
            "/report",
            "/login/",
            "/Login",
            "/login/extra",
        ] {
            assert!(bucket_for(path).is_none(), "{path} should not be limited");
        }
    }
}
