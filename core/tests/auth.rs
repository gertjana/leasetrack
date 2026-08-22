//! Credential handling: key hashing, constant-time comparison, token
//! generation, and the filename sanitising that keeps one account's data from
//! reaching another's file.

use leasetrack_core::{User, generate_api_key, generate_token, hash_key, secret_eq, user_data_path};

// ─── hash_key ─────────────────────────────────────────────────────────────────

#[test]
fn hash_key_matches_the_known_sha256_vector() {
    // Guards the on-disk format: changing the algorithm would silently
    // invalidate every stored key.
    assert_eq!(
        hash_key("test"),
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
    );
}

#[test]
fn hash_key_is_deterministic_and_lowercase_hex() {
    let hash = hash_key("some-api-key");

    assert_eq!(hash, hash_key("some-api-key"));
    assert_eq!(hash.len(), 64);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn hash_key_distinguishes_different_keys() {
    assert_ne!(hash_key("key-a"), hash_key("key-b"));
    // Keys are case sensitive.
    assert_ne!(hash_key("abc"), hash_key("ABC"));
}

// ─── User key handling ────────────────────────────────────────────────────────

#[test]
fn a_new_user_stores_only_the_hash() {
    let user = User::new("a@example.com".to_string(), "secret-key".to_string());

    assert_eq!(user.key_hash.as_deref(), Some(hash_key("secret-key").as_str()));
    assert_eq!(user.api_key, None, "the cleartext key must never be stored");
}

#[test]
fn a_user_matches_their_own_key_and_nothing_else() {
    let user = User::new("a@example.com".to_string(), "secret-key".to_string());

    assert!(user.matches_key("secret-key"));
    assert!(!user.matches_key("wrong-key"));
    assert!(!user.matches_key("secret-key "), "keys are compared exactly");
    assert!(!user.matches_key("SECRET-KEY"));
}

/// A request arriving with no credentials must never authenticate, even
/// against a malformed record.
#[test]
fn an_empty_key_never_matches() {
    let user = User::new("a@example.com".to_string(), "secret-key".to_string());
    assert!(!user.matches_key(""));

    let empty = User {
        email: "b@example.com".to_string(),
        key_hash: None,
        api_key: None,
        reset_token: None,
        reset_expires: None,
    };
    assert!(!empty.matches_key(""));
    assert!(!empty.matches_key("anything"));
}

#[test]
fn rotating_a_key_replaces_the_hash_and_invalidates_the_old_one() {
    let mut user = User::new("a@example.com".to_string(), "old-key".to_string());
    user.set_api_key("new-key");

    assert!(user.matches_key("new-key"));
    assert!(!user.matches_key("old-key"), "the previous key must stop working");
    assert_eq!(user.api_key, None);
}

/// Records written before key hashing existed still carry a cleartext key.
/// They must keep working, so a rollback or a stale file cannot lock anyone
/// out of their account.
#[test]
fn a_legacy_cleartext_record_still_authenticates() {
    let legacy = User {
        email: "a@example.com".to_string(),
        key_hash: None,
        api_key: Some("legacy-key".to_string()),
        reset_token: None,
        reset_expires: None,
    };

    assert!(legacy.matches_key("legacy-key"));
    assert!(!legacy.matches_key("other-key"));
}

#[test]
fn rotating_a_legacy_record_drops_the_cleartext_key() {
    let mut legacy = User {
        email: "a@example.com".to_string(),
        key_hash: None,
        api_key: Some("legacy-key".to_string()),
        reset_token: None,
        reset_expires: None,
    };

    legacy.set_api_key("new-key");

    assert_eq!(legacy.api_key, None);
    assert!(legacy.matches_key("new-key"));
    assert!(!legacy.matches_key("legacy-key"));
}

/// The hash wins when both fields are populated, so a stale cleartext value
/// cannot be used to bypass a rotated key.
#[test]
fn the_hash_takes_precedence_over_a_leftover_cleartext_key() {
    let user = User {
        email: "a@example.com".to_string(),
        key_hash: Some(hash_key("current-key")),
        api_key: Some("stale-key".to_string()),
        reset_token: None,
        reset_expires: None,
    };

    assert!(user.matches_key("current-key"));
    assert!(!user.matches_key("stale-key"));
}

// ─── secret_eq ────────────────────────────────────────────────────────────────

#[test]
fn secret_eq_compares_by_value() {
    assert!(secret_eq("abc", "abc"));
    assert!(!secret_eq("abc", "abd"));
    assert!(!secret_eq("abc", "abcd"), "different lengths never match");
    assert!(!secret_eq("", "a"));
    assert!(secret_eq("", ""));
}

#[test]
fn secret_eq_handles_long_values() {
    let token = generate_token();
    assert!(secret_eq(&token, &token.clone()));
    assert!(!secret_eq(&token, &generate_token()));
}

// ─── Token generation ─────────────────────────────────────────────────────────

#[test]
fn an_api_key_is_32_hex_characters() {
    let key = generate_api_key();

    assert_eq!(key.len(), 32, "128 bits, hex encoded");
    assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn a_token_is_64_hex_characters() {
    let token = generate_token();

    assert_eq!(token.len(), 64, "256 bits, hex encoded");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Not a randomness test — just a guard against a constant or trivially
/// repeating generator.
#[test]
fn generated_secrets_do_not_repeat() {
    let keys: std::collections::HashSet<_> = (0..64).map(|_| generate_api_key()).collect();
    assert_eq!(keys.len(), 64);

    let tokens: std::collections::HashSet<_> = (0..64).map(|_| generate_token()).collect();
    assert_eq!(tokens.len(), 64);
}

// ─── user_data_path ───────────────────────────────────────────────────────────
//
// Only the filename is asserted: the base directory depends on environment
// variables, which the storage suite covers in its own process.

fn filename_for(email: &str) -> String {
    user_data_path(email)
        .file_name()
        .expect("a filename")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn an_email_becomes_a_predictable_filename() {
    assert_eq!(
        filename_for("user@example.com"),
        "leasetrack-user_at_example_com.json"
    );
}

/// Addresses differing only in case must resolve to one file, or the same
/// account would see different data depending on how it was typed.
#[test]
fn filenames_are_case_insensitive_and_trimmed() {
    let expected = "leasetrack-user_at_example_com.json";

    assert_eq!(filename_for("User@Example.com"), expected);
    assert_eq!(filename_for("USER@EXAMPLE.COM"), expected);
    assert_eq!(filename_for("  user@example.com  "), expected);
}

/// The critical one: an address is untrusted input, so nothing in it may
/// escape the base directory.
#[test]
fn path_traversal_cannot_escape_the_base_directory() {
    for hostile in [
        "../../etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "a/../../b@example.com",
        "/etc/passwd",
        "..\\..\\windows\\system32",
    ] {
        let path = user_data_path(hostile);
        let name = filename_for(hostile);

        assert!(!name.contains('/'), "{hostile} produced {name}");
        assert!(!name.contains('\\'), "{hostile} produced {name}");
        assert!(!name.contains(".."), "{hostile} produced {name}");
        assert!(name.starts_with("leasetrack-"), "{hostile} produced {name}");
        assert!(name.ends_with(".json"), "{hostile} produced {name}");

        // The file must sit directly in the base directory, one level down.
        let base = user_data_path("anchor@example.com");
        assert_eq!(
            path.parent(),
            base.parent(),
            "{hostile} escaped to {}",
            path.display()
        );
    }
}

#[test]
fn unusual_but_legal_addresses_stay_distinct() {
    assert_ne!(filename_for("a.b@example.com"), filename_for("ab@example.com"));
    assert_ne!(filename_for("a+tag@example.com"), filename_for("a@example.com"));
}

#[test]
fn permitted_characters_are_preserved() {
    // Hyphens and underscores survive; everything else outside [a-z0-9] folds
    // to '_'.
    assert_eq!(
        filename_for("first-last_x@example.com"),
        "leasetrack-first-last_x_at_example_com.json"
    );
}
