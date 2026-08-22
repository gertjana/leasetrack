//! Persistence, user records and the reset-token lifecycle.
//!
//! These exercises need the filesystem, and the paths involved are chosen by
//! process-wide environment variables. Cargo gives each integration test file
//! its own binary, so nothing here can leak into the other suites; within this
//! one, `with_env` serialises access so tests cannot race each other.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Local, NaiveDate};
use leasetrack_core::{
    KmRecord, LeaseConfig, LeaseData, RESET_TOKEN_TTL_SECS, User, UsersData, authenticate_user,
    find_user_by_key, hash_key, issue_reset_token, load_user_data, load_users,
    migrate_users_to_hashed_keys, redeem_reset_token, save_user_data, save_users,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid test date")
}

/// Point the crate's data and users paths at a private temporary directory for
/// the duration of `f`, then clean up.
fn with_env<T>(f: impl FnOnce(&Path) -> T) -> T {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let unique = format!(
        "leasetrack-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let dir: PathBuf = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // SAFETY: all environment access in this binary is serialised by ENV_LOCK,
    // and no other thread reads these variables while the guard is held.
    unsafe {
        std::env::set_var("LEASETRACK_DATA_DIR", &dir);
        std::env::set_var("LEASETRACK_USERS_FILE", dir.join("users.json"));
    }

    let result = f(&dir);

    let _ = std::fs::remove_dir_all(&dir);
    drop(guard);
    result
}

fn sample_data() -> LeaseData {
    LeaseData {
        config: LeaseConfig {
            car_name: "Test Car".to_string(),
            lease_start: date("2025-01-01"),
            lease_years: 3,
            allowed_km_per_year: 20_000,
            start_odometer: 1_000,
        },
        records: vec![
            KmRecord { date: date("2025-03-01"), odometer: 5_000 },
            KmRecord { date: date("2025-06-01"), odometer: 12_000 },
        ],
    }
}

// ─── Lease data ───────────────────────────────────────────────────────────────

#[test]
fn lease_data_survives_a_save_and_load() {
    with_env(|_| {
        let original = sample_data();
        save_user_data("user@example.com", &original).expect("saved");

        let loaded = load_user_data("user@example.com").expect("loaded");

        assert_eq!(loaded.config.car_name, "Test Car");
        assert_eq!(loaded.config.lease_start, date("2025-01-01"));
        assert_eq!(loaded.config.lease_years, 3);
        assert_eq!(loaded.config.allowed_km_per_year, 20_000);
        assert_eq!(loaded.config.start_odometer, 1_000);
        assert_eq!(loaded.records.len(), 2);
        assert_eq!(loaded.records[1].odometer, 12_000);
    });
}

#[test]
fn loading_an_unknown_account_explains_what_to_do() {
    with_env(|_| {
        let error = load_user_data("nobody@example.com").expect_err("no data yet");
        assert!(error.contains("No lease data found"), "got: {error}");
    });
}

#[test]
fn accounts_are_stored_in_separate_files() {
    with_env(|_| {
        let mut a = sample_data();
        a.config.car_name = "Car A".to_string();
        let mut b = sample_data();
        b.config.car_name = "Car B".to_string();

        save_user_data("a@example.com", &a).expect("saved");
        save_user_data("b@example.com", &b).expect("saved");

        assert_eq!(load_user_data("a@example.com").unwrap().config.car_name, "Car A");
        assert_eq!(load_user_data("b@example.com").unwrap().config.car_name, "Car B");
    });
}

/// Addresses differing only in case are one account, so they must not end up
/// with two separate data files.
#[test]
fn an_account_resolves_regardless_of_address_casing() {
    with_env(|_| {
        save_user_data("User@Example.com", &sample_data()).expect("saved");
        assert!(load_user_data("user@example.com").is_ok());
        assert!(load_user_data("USER@EXAMPLE.COM").is_ok());
    });
}

#[test]
fn overwriting_lease_data_leaves_a_backup_of_the_previous_version() {
    with_env(|dir| {
        save_user_data("user@example.com", &sample_data()).expect("saved");

        let mut updated = sample_data();
        updated.config.car_name = "Renamed".to_string();
        save_user_data("user@example.com", &updated).expect("saved again");

        assert_eq!(
            load_user_data("user@example.com").unwrap().config.car_name,
            "Renamed"
        );

        let backup = dir.join("leasetrack-user_at_example_com.json.backup");
        assert!(backup.exists(), "the prior version should be kept");
        let backed_up: LeaseData =
            serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(backed_up.config.car_name, "Test Car");
    });
}

#[test]
fn corrupt_lease_data_reports_a_parse_error() {
    with_env(|dir| {
        std::fs::write(dir.join("leasetrack-user_at_example_com.json"), "{ not json")
            .expect("write");

        let error = load_user_data("user@example.com").expect_err("unparseable");
        assert!(error.contains("Failed to parse"), "got: {error}");
    });
}

// ─── Users file ───────────────────────────────────────────────────────────────

#[test]
fn an_absent_users_file_reads_as_empty_rather_than_failing() {
    with_env(|_| {
        let users = load_users().expect("an empty set");
        assert!(users.users.is_empty());
    });
}

#[test]
fn users_survive_a_save_and_load() {
    with_env(|_| {
        let data = UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        };
        save_users(&data).expect("saved");

        let loaded = load_users().expect("loaded");

        assert_eq!(loaded.users.len(), 1);
        assert_eq!(loaded.users[0].email, "a@example.com");
        assert!(loaded.users[0].matches_key("key-a"));
    });
}

/// The users file holds long-lived credentials, so other local accounts must
/// not be able to read it.
#[cfg(unix)]
#[test]
fn the_users_file_is_only_readable_by_its_owner() {
    use std::os::unix::fs::PermissionsExt;

    with_env(|dir| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        })
        .expect("saved");

        let mode = std::fs::metadata(dir.join("users.json"))
            .expect("metadata")
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o600, "expected owner-only rw, got {:o}", mode & 0o777);
    });
}

#[test]
fn a_stored_key_is_never_written_in_cleartext() {
    with_env(|dir| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "super-secret".to_string())],
        })
        .expect("saved");

        let raw = std::fs::read_to_string(dir.join("users.json")).expect("read");

        assert!(!raw.contains("super-secret"), "the key leaked into the file");
        assert!(raw.contains(&hash_key("super-secret")));
    });
}

// ─── Authentication ───────────────────────────────────────────────────────────

#[test]
fn authentication_requires_a_matching_address_and_key() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        })
        .expect("saved");

        assert!(authenticate_user("a@example.com", "key-a").is_some());
        assert!(authenticate_user("a@example.com", "wrong").is_none());
        assert!(authenticate_user("b@example.com", "key-a").is_none());
        assert!(authenticate_user("a@example.com", "").is_none());
    });
}

#[test]
fn authentication_accepts_any_casing_of_the_address() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        })
        .expect("saved");

        assert!(authenticate_user("A@Example.com", "key-a").is_some());
    });
}

/// One account's key must never resolve to another account.
#[test]
fn a_key_only_ever_identifies_its_own_account() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![
                User::new("a@example.com".to_string(), "key-a".to_string()),
                User::new("b@example.com".to_string(), "key-b".to_string()),
            ],
        })
        .expect("saved");

        assert_eq!(find_user_by_key("key-a").unwrap().email, "a@example.com");
        assert_eq!(find_user_by_key("key-b").unwrap().email, "b@example.com");
        assert!(find_user_by_key("key-c").is_none());
        assert!(authenticate_user("a@example.com", "key-b").is_none());
    });
}

#[test]
fn an_empty_key_authenticates_nobody() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        })
        .expect("saved");

        assert!(find_user_by_key("").is_none());
    });
}

// ─── Legacy key migration ─────────────────────────────────────────────────────

fn legacy_user(email: &str, key: &str) -> User {
    User {
        email: email.to_string(),
        key_hash: None,
        api_key: Some(key.to_string()),
        reset_token: None,
        reset_expires: None,
    }
}

#[test]
fn migration_rewrites_cleartext_keys_as_hashes() {
    with_env(|dir| {
        // Written directly, since `save_users` only ever emits hashed records.
        let raw = serde_json::to_string_pretty(&UsersData {
            users: vec![legacy_user("a@example.com", "legacy-key")],
        })
        .unwrap();
        std::fs::write(dir.join("users.json"), raw).expect("write");

        let migrated = migrate_users_to_hashed_keys().expect("migrated");
        assert_eq!(migrated, 1);

        let after = std::fs::read_to_string(dir.join("users.json")).expect("read");
        assert!(!after.contains("legacy-key"), "cleartext key survived migration");
        assert!(after.contains(&hash_key("legacy-key")));
    });
}

#[test]
fn a_migrated_user_keeps_the_key_they_already_had() {
    with_env(|dir| {
        let raw = serde_json::to_string_pretty(&UsersData {
            users: vec![legacy_user("a@example.com", "legacy-key")],
        })
        .unwrap();
        std::fs::write(dir.join("users.json"), raw).expect("write");

        migrate_users_to_hashed_keys().expect("migrated");

        assert!(
            authenticate_user("a@example.com", "legacy-key").is_some(),
            "migration must be invisible to the user"
        );
    });
}

#[test]
fn migration_is_a_no_op_once_everything_is_hashed() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![User::new("a@example.com".to_string(), "key-a".to_string())],
        })
        .expect("saved");

        assert_eq!(migrate_users_to_hashed_keys().expect("ran"), 0);
        // Safe to call repeatedly, as it is on every startup.
        assert_eq!(migrate_users_to_hashed_keys().expect("ran"), 0);
    });
}

#[test]
fn loading_users_presents_legacy_records_as_hashed() {
    with_env(|dir| {
        let raw = serde_json::to_string_pretty(&UsersData {
            users: vec![legacy_user("a@example.com", "legacy-key")],
        })
        .unwrap();
        std::fs::write(dir.join("users.json"), raw).expect("write");

        let loaded = load_users().expect("loaded");

        assert_eq!(loaded.users[0].api_key, None, "callers never see cleartext");
        assert_eq!(
            loaded.users[0].key_hash.as_deref(),
            Some(hash_key("legacy-key").as_str())
        );
    });
}

// ─── Reset tokens ─────────────────────────────────────────────────────────────

fn one_user() {
    save_users(&UsersData {
        users: vec![User::new("a@example.com".to_string(), "original-key".to_string())],
    })
    .expect("saved");
}

#[test]
fn issuing_a_token_for_an_unknown_address_returns_nothing() {
    with_env(|_| {
        one_user();
        assert_eq!(issue_reset_token("nobody@example.com").expect("ran"), None);
    });
}

/// Issuing must not rotate the key, or an unauthenticated request could lock
/// someone out of their own account.
#[test]
fn issuing_a_token_leaves_the_existing_key_working() {
    with_env(|_| {
        one_user();
        issue_reset_token("a@example.com").expect("issued").expect("a token");

        assert!(
            authenticate_user("a@example.com", "original-key").is_some(),
            "the key must survive until the link is followed"
        );
    });
}

#[test]
fn redeeming_a_token_rotates_the_key() {
    with_env(|_| {
        one_user();
        let token = issue_reset_token("a@example.com").unwrap().unwrap();

        let (email, new_key) = redeem_reset_token(&token).expect("redeemed");

        assert_eq!(email, "a@example.com");
        assert_ne!(new_key, "original-key");
        assert!(authenticate_user("a@example.com", &new_key).is_some());
        assert!(
            authenticate_user("a@example.com", "original-key").is_none(),
            "the previous key must stop working"
        );
    });
}

#[test]
fn a_token_can_only_be_redeemed_once() {
    with_env(|_| {
        one_user();
        let token = issue_reset_token("a@example.com").unwrap().unwrap();

        redeem_reset_token(&token).expect("first use succeeds");
        let error = redeem_reset_token(&token).expect_err("second use fails");

        assert!(error.contains("Invalid or expired"), "got: {error}");
    });
}

#[test]
fn an_unknown_or_empty_token_is_refused() {
    with_env(|_| {
        one_user();

        assert!(redeem_reset_token("").is_err());
        assert!(redeem_reset_token("not-a-real-token").is_err());
        assert!(redeem_reset_token(&"0".repeat(64)).is_err());
    });
}

#[test]
fn issuing_a_second_token_invalidates_the_first() {
    with_env(|_| {
        one_user();
        let first = issue_reset_token("a@example.com").unwrap().unwrap();
        let second = issue_reset_token("a@example.com").unwrap().unwrap();

        assert_ne!(first, second);
        assert!(redeem_reset_token(&first).is_err(), "the superseded link is dead");
        assert!(redeem_reset_token(&second).is_ok());
    });
}

#[test]
fn an_expired_token_is_refused_and_consumed() {
    with_env(|_| {
        one_user();
        let token = issue_reset_token("a@example.com").unwrap().unwrap();

        // Backdate the expiry past the TTL, as if the link had gone stale.
        let mut users = load_users().expect("loaded");
        users.users[0].reset_expires = Some(Local::now().timestamp() - RESET_TOKEN_TTL_SECS - 1);
        save_users(&users).expect("saved");

        let error = redeem_reset_token(&token).expect_err("expired");
        assert!(error.contains("Invalid or expired"), "got: {error}");

        // The stale token is cleared, so it cannot be retried.
        let after = load_users().expect("loaded");
        assert_eq!(after.users[0].reset_token, None);
        assert!(
            authenticate_user("a@example.com", "original-key").is_some(),
            "an expired link must not rotate the key"
        );
    });
}

#[test]
fn redeeming_clears_the_token_from_storage() {
    with_env(|_| {
        one_user();
        let token = issue_reset_token("a@example.com").unwrap().unwrap();
        redeem_reset_token(&token).expect("redeemed");

        let after = load_users().expect("loaded");
        assert_eq!(after.users[0].reset_token, None);
        assert_eq!(after.users[0].reset_expires, None);
    });
}

/// A reset for one account must not disturb another.
#[test]
fn a_reset_only_affects_the_requesting_account() {
    with_env(|_| {
        save_users(&UsersData {
            users: vec![
                User::new("a@example.com".to_string(), "key-a".to_string()),
                User::new("b@example.com".to_string(), "key-b".to_string()),
            ],
        })
        .expect("saved");

        let token = issue_reset_token("a@example.com").unwrap().unwrap();
        redeem_reset_token(&token).expect("redeemed");

        assert!(
            authenticate_user("b@example.com", "key-b").is_some(),
            "the other account is untouched"
        );
    });
}
