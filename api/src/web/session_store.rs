//! Server-side session records.
//!
//! Topcoat issues and carries the session token and hands us only its hash;
//! where the records live is up to us. This is the same in-memory model the
//! previous hand-rolled session map used: state resets on restart and is
//! per-instance rather than shared across replicas.
//!
//! The browser only ever holds the opaque token, so signing out (or expiry)
//! revokes access immediately — unlike the API key, which is a long-lived
//! credential that a cookie could otherwise leak forever.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use topcoat::session::{Session, TokenHash};

/// How long a signed-in session stays valid.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

struct Record {
    email: String,
    expires_at: SystemTime,
}

/// Session records keyed by the hash of the token held by the browser.
#[derive(Default)]
pub struct SessionStore {
    sessions: Mutex<HashMap<TokenHash, Record>>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<TokenHash, Record>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record a freshly started session as belonging to `email`.
    pub fn create(&self, session: Session, email: String) {
        let mut sessions = self.lock();
        // Opportunistically drop anything already expired so the map cannot
        // grow without bound in a long-running process.
        let now = SystemTime::now();
        sessions.retain(|_, r| r.expires_at > now);
        sessions.insert(
            session.token_hash,
            Record {
                email,
                expires_at: session.expires_at,
            },
        );
    }

    /// The account a session belongs to, or `None` once it has expired.
    ///
    /// The caller's identity comes from this record only. Nothing the browser
    /// sends other than the opaque token influences which account is loaded,
    /// which is what prevents one user from acting as another.
    pub fn email_for(&self, token_hash: &TokenHash) -> Option<String> {
        let mut sessions = self.lock();
        let record = sessions.get(token_hash)?;
        if record.expires_at <= SystemTime::now() {
            sessions.remove(token_hash);
            return None;
        }
        Some(record.email.clone())
    }

    pub fn delete(&self, token_hash: &TokenHash) {
        self.lock().remove(token_hash);
    }

    /// Invalidate every session belonging to `email`, used when the API key
    /// changes so old sessions cannot outlive a credential reset.
    pub fn delete_for_email(&self, email: &str) {
        self.lock()
            .retain(|_, r| !r.email.eq_ignore_ascii_case(email));
    }

    /// Number of stored sessions, including any that have expired but not yet
    /// been pruned. Used to assert that eviction actually happens.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Token hashes are derived from a seed so a test can look one up without
    /// having to hold on to the `Session` it was stored with.
    fn hash(seed: u8) -> TokenHash {
        TokenHash::new([seed; 32])
    }

    fn session(seed: u8) -> Session {
        Session {
            token_hash: hash(seed),
            expires_at: SystemTime::now() + SESSION_TTL,
        }
    }

    fn expired_session(seed: u8) -> Session {
        Session {
            token_hash: hash(seed),
            expires_at: SystemTime::now() - Duration::from_secs(1),
        }
    }

    #[test]
    fn a_stored_session_resolves_to_its_account() {
        let store = SessionStore::new();
        store.create(session(1), "user@example.com".to_string());

        assert_eq!(store.email_for(&hash(1)).as_deref(), Some("user@example.com"));
    }

    #[test]
    fn an_unknown_token_resolves_to_nobody() {
        let store = SessionStore::new();
        assert_eq!(store.email_for(&hash(9)), None);
    }

    /// The whole point of server-side sessions: signing out revokes access
    /// immediately rather than relying on the browser to forget a cookie.
    #[test]
    fn deleting_a_session_revokes_it_at_once() {
        let store = SessionStore::new();
        store.create(session(1), "user@example.com".to_string());

        store.delete(&hash(1));

        assert_eq!(store.email_for(&hash(1)), None);
    }

    #[test]
    fn an_expired_session_is_refused() {
        let store = SessionStore::new();
        store.create(expired_session(1), "user@example.com".to_string());

        assert_eq!(store.email_for(&hash(1)), None, "expiry is enforced on read");
    }

    #[test]
    fn sessions_are_isolated_from_one_another() {
        let store = SessionStore::new();
        store.create(session(1), "a@example.com".to_string());
        store.create(session(2), "b@example.com".to_string());

        assert_eq!(store.email_for(&hash(1)).as_deref(), Some("a@example.com"));
        assert_eq!(store.email_for(&hash(2)).as_deref(), Some("b@example.com"));

        store.delete(&hash(1));
        assert_eq!(store.email_for(&hash(2)).as_deref(), Some("b@example.com"));
    }

    /// Rotating an API key must not leave older sessions alive, or a reset
    /// would fail to lock out whoever prompted it.
    #[test]
    fn rotating_a_key_drops_every_session_for_that_account() {
        let store = SessionStore::new();
        store.create(session(1), "user@example.com".to_string());
        store.create(session(2), "user@example.com".to_string());
        store.create(session(3), "someone@example.com".to_string());

        store.delete_for_email("user@example.com");

        assert_eq!(store.email_for(&hash(1)), None, "every session for the account goes");
        assert_eq!(store.email_for(&hash(2)), None);
        assert_eq!(
            store.email_for(&hash(3)).as_deref(),
            Some("someone@example.com"),
            "other accounts are untouched"
        );
    }

    #[test]
    fn account_removal_ignores_address_casing() {
        let store = SessionStore::new();
        store.create(session(1), "User@Example.com".to_string());

        store.delete_for_email("user@example.com");

        assert_eq!(store.email_for(&hash(1)), None);
    }

    /// Sessions are held in memory for the life of the process, so expired
    /// entries must not accumulate indefinitely.
    #[test]
    fn expired_sessions_are_evicted_when_new_ones_are_created() {
        let store = SessionStore::new();
        store.create(expired_session(1), "old@example.com".to_string());

        // Creating a session prunes anything already past its expiry.
        store.create(session(2), "new@example.com".to_string());

        assert_eq!(store.len(), 1, "the expired entry should have been dropped");
        assert_eq!(store.email_for(&hash(1)), None);
    }

    #[test]
    fn re_registering_a_token_replaces_the_previous_account() {
        let store = SessionStore::new();
        store.create(session(1), "first@example.com".to_string());
        store.create(session(1), "second@example.com".to_string());

        assert_eq!(store.email_for(&hash(1)).as_deref(), Some("second@example.com"));
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn the_session_lifetime_is_twelve_hours() {
        assert_eq!(SESSION_TTL, Duration::from_secs(12 * 60 * 60));
    }
}
