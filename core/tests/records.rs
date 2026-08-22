//! Recording odometer readings: what is rejected outright, what is accepted
//! with a warning, and how records are ordered.

use chrono::NaiveDate;
use leasetrack_core::{LeaseConfig, LeaseData, add_record};

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid test date")
}

/// A 3 year lease from 2025-01-01 starting at 1,000 km.
fn data() -> LeaseData {
    LeaseData {
        config: LeaseConfig {
            car_name: "Test Car".to_string(),
            lease_start: date("2025-01-01"),
            lease_years: 3,
            allowed_km_per_year: 20_000,
            start_odometer: 1_000,
        },
        records: vec![],
    }
}

// ─── Accepted ─────────────────────────────────────────────────────────────────

#[test]
fn a_normal_reading_is_accepted_without_warnings() {
    let mut d = data();
    let warnings = add_record(&mut d, 5_000, date("2025-03-01")).expect("accepted");

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    assert_eq!(d.records.len(), 1);
    assert_eq!(d.records[0].odometer, 5_000);
    assert_eq!(d.records[0].date, date("2025-03-01"));
}

#[test]
fn a_reading_equal_to_the_start_odometer_is_accepted() {
    let mut d = data();
    let warnings = add_record(&mut d, 1_000, date("2025-01-01")).expect("accepted");

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn records_are_kept_in_date_order_however_they_arrive() {
    let mut d = data();
    add_record(&mut d, 9_000, date("2025-09-01")).expect("accepted");
    add_record(&mut d, 3_000, date("2025-03-01")).expect("accepted");
    add_record(&mut d, 6_000, date("2025-06-01")).expect("accepted");

    let dates: Vec<_> = d.records.iter().map(|r| r.date).collect();
    assert_eq!(
        dates,
        vec![date("2025-03-01"), date("2025-06-01"), date("2025-09-01")]
    );
}

// ─── Rejected ─────────────────────────────────────────────────────────────────

#[test]
fn a_reading_below_the_start_odometer_is_rejected() {
    let mut d = data();
    let error = add_record(&mut d, 500, date("2025-03-01")).expect_err("rejected");

    assert!(error.contains("below the starting odometer"), "got: {error}");
    assert!(d.records.is_empty(), "a rejected reading must not be stored");
}

#[test]
fn a_second_reading_on_the_same_date_is_rejected() {
    let mut d = data();
    add_record(&mut d, 5_000, date("2025-03-01")).expect("accepted");

    let error = add_record(&mut d, 6_000, date("2025-03-01")).expect_err("rejected");

    assert!(error.contains("already exists"), "got: {error}");
    assert_eq!(d.records.len(), 1, "the original reading is untouched");
    assert_eq!(d.records[0].odometer, 5_000);
}

/// The error names the existing value so the user can tell whether their new
/// reading is the correct one.
#[test]
fn the_duplicate_error_reports_the_existing_reading() {
    let mut d = data();
    add_record(&mut d, 25_000, date("2025-03-01")).expect("accepted");

    let error = add_record(&mut d, 26_000, date("2025-03-01")).expect_err("rejected");

    assert!(error.contains("25,000"), "expected a formatted value in: {error}");
}

// ─── Accepted with warnings ───────────────────────────────────────────────────

#[test]
fn a_reading_before_the_lease_started_warns_but_is_stored() {
    let mut d = data();
    let warnings = add_record(&mut d, 2_000, date("2024-06-01")).expect("accepted");

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("outside the lease period"), "got: {warnings:?}");
    assert_eq!(d.records.len(), 1, "the reading is still recorded");
}

#[test]
fn a_reading_after_the_lease_ended_warns_but_is_stored() {
    let mut d = data();
    let warnings = add_record(&mut d, 70_000, date("2029-01-01")).expect("accepted");

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("outside the lease period"), "got: {warnings:?}");
    assert_eq!(d.records.len(), 1);
}

#[test]
fn the_final_day_of_the_lease_is_inside_the_period() {
    let mut d = data();
    // The lease runs 2025-01-01 through 2027-12-31.
    let warnings = add_record(&mut d, 55_000, date("2027-12-31")).expect("accepted");

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn a_reading_lower_than_an_earlier_one_warns_about_going_backwards() {
    let mut d = data();
    add_record(&mut d, 10_000, date("2025-03-01")).expect("accepted");

    let warnings = add_record(&mut d, 8_000, date("2025-06-01")).expect("accepted");

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("odometer went backwards"), "got: {warnings:?}");
    assert!(warnings[0].contains("Earlier record"), "got: {warnings:?}");
}

#[test]
fn a_reading_higher_than_a_later_one_warns_about_going_backwards() {
    let mut d = data();
    add_record(&mut d, 8_000, date("2025-06-01")).expect("accepted");

    // Backfilling an earlier date with a larger value is equally suspicious.
    let warnings = add_record(&mut d, 10_000, date("2025-03-01")).expect("accepted");

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("odometer went backwards"), "got: {warnings:?}");
    assert!(warnings[0].contains("Later record"), "got: {warnings:?}");
}

#[test]
fn one_warning_is_raised_per_inconsistent_neighbour() {
    let mut d = data();
    add_record(&mut d, 10_000, date("2025-03-01")).expect("accepted");
    add_record(&mut d, 20_000, date("2025-04-01")).expect("accepted");

    // Lower than both existing readings, which both predate it.
    let warnings = add_record(&mut d, 5_000, date("2025-06-01")).expect("accepted");

    assert_eq!(warnings.len(), 2, "got: {warnings:?}");
}

#[test]
fn a_reading_can_be_both_out_of_period_and_backwards() {
    let mut d = data();
    add_record(&mut d, 10_000, date("2025-03-01")).expect("accepted");

    let warnings = add_record(&mut d, 5_000, date("2029-06-01")).expect("accepted");

    assert_eq!(warnings.len(), 2, "got: {warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("outside the lease period")));
    assert!(warnings.iter().any(|w| w.contains("odometer went backwards")));
}

#[test]
fn an_equal_reading_on_a_later_date_is_not_treated_as_backwards() {
    let mut d = data();
    add_record(&mut d, 10_000, date("2025-03-01")).expect("accepted");

    // Standing still is normal for a parked car; only a decrease is suspicious.
    let warnings = add_record(&mut d, 10_000, date("2025-06-01")).expect("accepted");

    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn existing_records_survive_a_rejected_insert() {
    let mut d = data();
    add_record(&mut d, 5_000, date("2025-03-01")).expect("accepted");
    add_record(&mut d, 9_000, date("2025-06-01")).expect("accepted");

    let _ = add_record(&mut d, 100, date("2025-09-01")).expect_err("rejected");

    assert_eq!(d.records.len(), 2);
    assert_eq!(
        d.records.iter().map(|r| r.odometer).collect::<Vec<_>>(),
        vec![5_000, 9_000]
    );
}

#[test]
fn add_record_does_not_persist_anything_itself() {
    // The caller owns persistence; `add_record` only mutates the value it is
    // given. Nothing here touches the filesystem.
    let mut d = data();
    add_record(&mut d, 5_000, date("2025-03-01")).expect("accepted");

    assert_eq!(d.records.len(), 1);
    assert_eq!(d.records[0].date, date("2025-03-01"));
    assert_eq!(d.records[0].odometer, 5_000);
}
