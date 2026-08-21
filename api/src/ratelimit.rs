//! Fixed-window rate limiting for the unauthenticated endpoints.
//!
//! `/login`, `/register` and `/forgot` are the endpoints an attacker can hit
//! without credentials. They are limited on two independent axes:
//!
//! * **per client IP** — caps brute forcing and general abuse from one source;
//! * **per target email** — caps how much mail one address can be made to
//!   receive. An IP limit alone does not do this: a botnet spread across many
//!   addresses could still flood a single victim's inbox.
//!
//! State is in-memory, like the session store, so it resets on restart and is
//! per-instance rather than shared across replicas.

use axum::extract::ConnectInfo;
use axum::http::{Extensions, HeaderMap};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A quota: at most `max` hits per `window`.
#[derive(Clone, Copy)]
pub struct Quota {
    pub max: u32,
    pub window: Duration,
}

impl Quota {
    pub const fn new(max: u32, window_secs: u64) -> Self {
        Quota { max, window: Duration::from_secs(window_secs) }
    }
}

/// Sign-in attempts per IP. Generous enough for a fat-fingered human, far too
/// tight to work through a keyspace.
pub const LOGIN_PER_IP: Quota = Quota::new(10, 15 * 60);
/// Reset-link redemptions per IP.
///
/// Deliberately its own bucket rather than sharing the sign-in one: someone who
/// has forgotten their key burns sign-in attempts first and only then follows a
/// reset link, so a shared counter would lock them out of recovery at exactly
/// the moment they need it.
pub const RESET_PER_IP: Quota = Quota::new(10, 15 * 60);
/// Account creation / reset requests per IP.
pub const EMAIL_PER_IP: Quota = Quota::new(5, 60 * 60);
/// Messages any single address can be made to receive, regardless of source.
pub const EMAIL_PER_ADDRESS: Quota = Quota::new(5, 60 * 60);

struct Window {
    hits: u32,
    started: Instant,
    window: Duration,
}

/// Fixed-window counters keyed by an arbitrary string.
#[derive(Clone, Default)]
pub struct RateLimiter {
    windows: Arc<Mutex<HashMap<String, Window>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        RateLimiter { windows: Arc::new(Mutex::new(HashMap::new())) }
    }

    /// Record a hit against `key`. Returns `Err(retry_after_secs)` when the
    /// quota for the current window is already spent.
    pub fn check(&self, key: &str, quota: Quota) -> Result<(), u64> {
        let now = Instant::now();
        let mut windows = self.windows.lock().expect("rate limiter lock poisoned");

        // Drop windows that have rolled over so the map cannot grow unbounded.
        windows.retain(|_, w| now.duration_since(w.started) < w.window);

        match windows.get_mut(key) {
            Some(window) if now.duration_since(window.started) < quota.window => {
                if window.hits >= quota.max {
                    let elapsed = now.duration_since(window.started);
                    let retry = quota.window.saturating_sub(elapsed).as_secs().max(1);
                    return Err(retry);
                }
                window.hits += 1;
                Ok(())
            }
            _ => {
                windows.insert(
                    key.to_string(),
                    Window { hits: 1, started: now, window: quota.window },
                );
                Ok(())
            }
        }
    }
}

/// Client address for limiting purposes.
///
/// Behind a reverse proxy the socket address is the proxy's, so every client
/// would share one bucket — which turns the limiter into a self-inflicted
/// denial of service. `TRUST_PROXY=1` switches to the rightmost
/// `X-Forwarded-For` entry, which is the address the *closest* proxy observed
/// and the only hop a client cannot forge. It defaults to off, because
/// trusting that header on a directly-reachable server would let anyone
/// sidestep the limit with a fabricated value.
///
/// Takes the parts rather than a whole request so it can be called from a
/// Topcoat handler, which exposes headers and extensions but not a `Request`.
/// `ConnectInfo` is inserted by the surrounding axum server and survives into
/// Topcoat because the tower bridge preserves request extensions.
pub fn client_key_from(headers: &HeaderMap, extensions: &Extensions) -> String {
    let trust_proxy = std::env::var("TRUST_PROXY")
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);

    if trust_proxy {
        if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(ip) = forwarded.rsplit(',').next().map(str::trim) {
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }

    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Check the per-address quota. Called from the handlers, which have already
/// parsed the form and therefore know the target address.
pub fn check_email_quota(limiter: &RateLimiter, email: &str) -> Result<(), u64> {
    limiter.check(&format!("email:{}", email.trim().to_lowercase()), EMAIL_PER_ADDRESS)
}
