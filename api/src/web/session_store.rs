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
}
