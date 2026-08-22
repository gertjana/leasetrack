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
/// Deliberately its own bucket rather than sharing the sign-in one. Account
/// recovery runs in a fixed order: someone who has forgotten their key fails
/// sign-in several times *first*, and only then requests and follows a reset
/// link. A shared counter is therefore already spent by the time the confirm
/// button is pressed, and returns 429 to the one person it should let through.
///
/// Brute force is not the threat model here: reset tokens are 256-bit random
/// values, so this quota exists to bound abuse, not to protect the token.
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

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Mutex;

    /// `client_key_from` reads `TRUST_PROXY`, which is process-wide. Unit tests
    /// share one process, so every test touching it takes this lock.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_trust_proxy<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: all access to this variable in this binary is serialised by
        // ENV_LOCK, and nothing else reads it while the guard is held.
        unsafe {
            match value {
                Some(v) => std::env::set_var("TRUST_PROXY", v),
                None => std::env::remove_var("TRUST_PROXY"),
            }
        }
        let out = f();
        unsafe { std::env::remove_var("TRUST_PROXY") };
        drop(guard);
        out
    }

    fn headers_with(forwarded: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = forwarded {
            headers.insert("x-forwarded-for", value.parse().expect("valid header"));
        }
        headers
    }

    fn extensions_with(ip: [u8; 4]) -> Extensions {
        let mut ext = Extensions::new();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 4242);
        ext.insert(ConnectInfo(addr));
        ext
    }

    // ─── Quota accounting ─────────────────────────────────────────────────────

    #[test]
    fn requests_are_allowed_up_to_the_quota() {
        let limiter = RateLimiter::new();
        let quota = Quota::new(3, 3600);

        assert!(limiter.check("k", quota).is_ok());
        assert!(limiter.check("k", quota).is_ok());
        assert!(limiter.check("k", quota).is_ok());
    }

    #[test]
    fn the_request_after_the_quota_is_refused() {
        let limiter = RateLimiter::new();
        let quota = Quota::new(2, 3600);

        limiter.check("k", quota).expect("first");
        limiter.check("k", quota).expect("second");

        assert!(limiter.check("k", quota).is_err(), "the third exceeds a quota of 2");
    }

    #[test]
    fn a_refusal_reports_how_long_to_wait() {
        let limiter = RateLimiter::new();
        let quota = Quota::new(1, 900);

        limiter.check("k", quota).expect("first");
        let retry = limiter.check("k", quota).expect_err("refused");

        assert!(retry > 0, "a retry hint of 0 would invite an immediate retry");
        assert!(retry <= 900, "cannot exceed the window, got {retry}");
    }

    /// The bug that shipped once already, at the layer below the routing table:
    /// separate keys must not draw down a shared counter.
    #[test]
    fn separate_keys_are_counted_independently() {
        let limiter = RateLimiter::new();
        let quota = Quota::new(1, 3600);

        limiter.check("login:ip:1.2.3.4", quota).expect("first key");
        assert!(
            limiter.check("reset:ip:1.2.3.4", quota).is_ok(),
            "exhausting one bucket must not block another"
        );
        assert!(
            limiter.check("login:ip:5.6.7.8", quota).is_ok(),
            "one client must not consume another client's quota"
        );

        // Each key is independently exhausted.
        assert!(limiter.check("login:ip:1.2.3.4", quota).is_err());
        assert!(limiter.check("reset:ip:1.2.3.4", quota).is_err());
    }

    #[test]
    fn a_cloned_limiter_shares_its_counters() {
        // The limiter is cloned into the router; the clones must not each get
        // their own budget.
        let limiter = RateLimiter::new();
        let clone = limiter.clone();
        let quota = Quota::new(1, 3600);

        limiter.check("k", quota).expect("first");
        assert!(clone.check("k", quota).is_err(), "clones share state");
    }

    #[test]
    fn the_window_rolls_over_and_restores_the_quota() {
        let limiter = RateLimiter::new();
        let quota = Quota::new(1, 1);

        limiter.check("k", quota).expect("first");
        assert!(limiter.check("k", quota).is_err(), "still inside the window");

        std::thread::sleep(std::time::Duration::from_millis(1_300));

        assert!(
            limiter.check("k", quota).is_ok(),
            "the quota should be available again in the next window"
        );
    }

    #[test]
    fn the_email_quota_is_keyed_by_address_not_by_client() {
        let limiter = RateLimiter::new();

        for _ in 0..EMAIL_PER_ADDRESS.max {
            check_email_quota(&limiter, "victim@example.com").expect("within quota");
        }

        assert!(
            check_email_quota(&limiter, "victim@example.com").is_err(),
            "one address cannot be mailed without limit"
        );
        assert!(
            check_email_quota(&limiter, "other@example.com").is_ok(),
            "a different address has its own budget"
        );
    }

    #[test]
    fn the_email_quota_ignores_address_casing_and_padding() {
        let limiter = RateLimiter::new();

        check_email_quota(&limiter, "User@Example.com").expect("first");
        check_email_quota(&limiter, "  user@example.com  ").expect("second");

        // Both spellings drew down the same budget.
        for _ in 2..EMAIL_PER_ADDRESS.max {
            check_email_quota(&limiter, "user@example.com").expect("within quota");
        }
        assert!(check_email_quota(&limiter, "USER@EXAMPLE.COM").is_err());
    }

    // ─── Client identification ────────────────────────────────────────────────

    #[test]
    fn the_socket_address_identifies_the_client_by_default() {
        with_trust_proxy(None, || {
            let key = client_key_from(&headers_with(None), &extensions_with([203, 0, 113, 5]));
            assert_eq!(key, "203.0.113.5");
        });
    }

    /// Without `TRUST_PROXY` the header is attacker-controlled: honouring it on
    /// a directly reachable server would let anyone reset their own limit by
    /// sending a fresh value each request.
    #[test]
    fn a_forwarded_header_is_ignored_unless_the_proxy_is_trusted() {
        with_trust_proxy(None, || {
            let key = client_key_from(
                &headers_with(Some("198.51.100.9")),
                &extensions_with([203, 0, 113, 5]),
            );
            assert_eq!(key, "203.0.113.5", "the spoofable header must not win");
        });
    }

    #[test]
    fn a_trusted_proxy_supplies_the_client_address() {
        with_trust_proxy(Some("1"), || {
            let key = client_key_from(
                &headers_with(Some("198.51.100.9")),
                &extensions_with([203, 0, 113, 5]),
            );
            assert_eq!(key, "198.51.100.9");
        });
    }

    /// Only the rightmost entry was observed by our own proxy; everything to
    /// its left was supplied by the client and can be fabricated.
    #[test]
    fn the_rightmost_forwarded_entry_wins() {
        with_trust_proxy(Some("1"), || {
            let key = client_key_from(
                &headers_with(Some("1.1.1.1, 2.2.2.2, 198.51.100.9")),
                &extensions_with([203, 0, 113, 5]),
            );
            assert_eq!(key, "198.51.100.9", "a client-supplied prefix must not win");
        });
    }

    #[test]
    fn trust_proxy_accepts_its_documented_spellings() {
        for value in ["1", "true", "yes"] {
            with_trust_proxy(Some(value), || {
                let key = client_key_from(
                    &headers_with(Some("198.51.100.9")),
                    &extensions_with([203, 0, 113, 5]),
                );
                assert_eq!(key, "198.51.100.9", "TRUST_PROXY={value} should be on");
            });
        }

        for value in ["0", "false", "no", ""] {
            with_trust_proxy(Some(value), || {
                let key = client_key_from(
                    &headers_with(Some("198.51.100.9")),
                    &extensions_with([203, 0, 113, 5]),
                );
                assert_eq!(key, "203.0.113.5", "TRUST_PROXY={value} should be off");
            });
        }
    }

    #[test]
    fn a_trusted_proxy_falls_back_when_the_header_is_absent_or_blank() {
        with_trust_proxy(Some("1"), || {
            let absent = client_key_from(&headers_with(None), &extensions_with([203, 0, 113, 5]));
            assert_eq!(absent, "203.0.113.5");

            let blank = client_key_from(
                &headers_with(Some("1.1.1.1,   ")),
                &extensions_with([203, 0, 113, 5]),
            );
            assert_eq!(blank, "203.0.113.5", "an empty entry is not an address");
        });
    }

    /// Better to collapse to one shared bucket than to fail open and drop the
    /// limit entirely.
    #[test]
    fn an_unidentifiable_client_still_gets_a_bucket() {
        with_trust_proxy(None, || {
            let key = client_key_from(&headers_with(None), &Extensions::new());
            assert_eq!(key, "unknown");
        });
    }
}
