//! Lease arithmetic: date handling, odometer interpolation, and the derived
//! year / report figures the CLI, API and dashboard all render.

use chrono::{Duration, Local, NaiveDate};
use leasetrack_core::{
    KmRecord, LeaseConfig, LeaseData, add_years, compute_report_data, compute_year_stats, fmt_km,
    fmt_km_f, km_at_date,
};

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("valid test date")
}

fn lease(start: NaiveDate, years: u32, per_year: u32, start_odometer: u32) -> LeaseData {
    LeaseData {
        config: LeaseConfig {
            car_name: "Test Car".to_string(),
            lease_start: start,
            lease_years: years,
            allowed_km_per_year: per_year,
            start_odometer,
        },
        records: vec![],
    }
}

// ─── add_years ────────────────────────────────────────────────────────────────

#[test]
fn add_years_advances_the_calendar_year() {
    assert_eq!(add_years(date("2025-01-01"), 3), date("2028-01-01"));
    assert_eq!(add_years(date("2025-06-15"), 1), date("2026-06-15"));
}

#[test]
fn add_years_of_zero_is_the_same_day() {
    assert_eq!(add_years(date("2025-06-15"), 0), date("2025-06-15"));
}

/// A lease starting on a leap day has no exact anniversary in common years.
/// Clamping to the 28th keeps the lease a whole number of years rather than
/// silently rolling into March.
#[test]
fn add_years_clamps_a_leap_day_to_february_28() {
    assert_eq!(add_years(date("2024-02-29"), 1), date("2025-02-28"));
    assert_eq!(add_years(date("2024-02-29"), 3), date("2027-02-28"));
}

#[test]
fn add_years_keeps_a_leap_day_when_landing_on_a_leap_year() {
    assert_eq!(add_years(date("2024-02-29"), 4), date("2028-02-29"));
}

// ─── fmt_km ───────────────────────────────────────────────────────────────────

#[test]
fn fmt_km_groups_thousands() {
    assert_eq!(fmt_km(0), "0");
    assert_eq!(fmt_km(7), "7");
    assert_eq!(fmt_km(999), "999");
    assert_eq!(fmt_km(1_000), "1,000");
    assert_eq!(fmt_km(20_000), "20,000");
    assert_eq!(fmt_km(100_000), "100,000");
    assert_eq!(fmt_km(1_234_567), "1,234,567");
}

#[test]
fn fmt_km_f_rounds_to_the_nearest_whole_km() {
    assert_eq!(fmt_km_f(1_499.4), "1,499");
    assert_eq!(fmt_km_f(1_499.5), "1,500");
    assert_eq!(fmt_km_f(0.2), "0");
}

// ─── km_at_date ───────────────────────────────────────────────────────────────

#[test]
fn km_at_date_without_records_is_known_only_at_the_lease_start() {
    let start = date("2025-01-01");
    assert_eq!(km_at_date(&[], 1_000, start, start), Some(1_000.0));
    assert_eq!(km_at_date(&[], 1_000, start, date("2025-06-01")), None);
}

#[test]
fn km_at_date_interpolates_linearly_between_two_readings() {
    let start = date("2025-01-01");
    let records = vec![
        KmRecord { date: date("2025-01-11"), odometer: 1_100 },
        KmRecord { date: date("2025-01-21"), odometer: 1_300 },
    ];

    // Exactly halfway between the two readings: 1100 + 0.5 * 200.
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2025-01-16")),
        Some(1_200.0)
    );
    // Two days into the ten-day interval: 1100 + 0.2 * 200.
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2025-01-13")),
        Some(1_140.0)
    );
    // And within the leading interval, from the synthetic lease-start point:
    // 1000 + 0.5 * 100.
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2025-01-06")),
        Some(1_050.0)
    );
}

#[test]
fn km_at_date_returns_exact_values_on_reading_days() {
    let start = date("2025-01-01");
    let records = vec![
        KmRecord { date: date("2025-01-11"), odometer: 1_100 },
        KmRecord { date: date("2025-01-21"), odometer: 1_300 },
    ];

    assert_eq!(
        km_at_date(&records, 1_000, start, date("2025-01-11")),
        Some(1_100.0)
    );
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2025-01-21")),
        Some(1_300.0)
    );
}

/// Outside the recorded range the value is held flat rather than extrapolated:
/// inventing mileage beyond what was actually observed would inflate every
/// figure derived from it.
#[test]
fn km_at_date_clamps_outside_the_recorded_range() {
    let start = date("2025-01-01");
    let records = vec![KmRecord { date: date("2025-01-11"), odometer: 1_100 }];

    // Before the lease start, the synthetic start point wins.
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2024-12-01")),
        Some(1_000.0)
    );
    // After the last reading, the last value is held.
    assert_eq!(
        km_at_date(&records, 1_000, start, date("2030-01-01")),
        Some(1_100.0)
    );
}

#[test]
fn km_at_date_uses_a_real_record_on_the_lease_start_over_the_synthetic_one() {
    let start = date("2025-01-01");
    let records = vec![KmRecord { date: start, odometer: 5_000 }];

    // The stored reading takes precedence over `start_odometer`.
    assert_eq!(km_at_date(&records, 1_000, start, start), Some(5_000.0));
}

// ─── compute_year_stats ───────────────────────────────────────────────────────

#[test]
fn year_stats_split_the_lease_into_one_entry_per_year() {
    let mut data = lease(date("2025-01-01"), 3, 20_000, 0);
    data.records.push(KmRecord { date: date("2025-06-01"), odometer: 10_000 });

    let years = compute_year_stats(&data);

    assert_eq!(years.len(), 3);
    assert_eq!(years[0].year_num, 1);
    assert_eq!(years[2].year_num, 3);

    // Each year runs start..end with end exclusive, meeting the next year's start.
    assert_eq!(years[0].start, date("2025-01-01"));
    assert_eq!(years[0].end, date("2026-01-01"));
    assert_eq!(years[1].start, years[0].end);
    assert_eq!(years[2].end, date("2028-01-01"));
}

/// Built relative to today so the assertion holds whenever the suite runs:
/// 400 days is comfortably more than one year and less than two.
#[test]
fn year_stats_mark_exactly_one_year_current_and_the_rest_past_or_future() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 3, 20_000, 1_000);
    data.records.push(KmRecord {
        date: today - Duration::days(200),
        odometer: 11_000,
    });

    let years = compute_year_stats(&data);

    assert_eq!(years.iter().filter(|y| y.is_current).count(), 1);
    assert!(!years[0].is_current && !years[0].is_future, "year 1 is past");
    assert!(years[1].is_current, "year 2 contains today");
    assert!(years[2].is_future, "year 3 has not started");

    // A year cannot be both current and future.
    assert!(years.iter().all(|y| !(y.is_current && y.is_future)));
}

#[test]
fn year_stats_report_no_distance_for_future_years() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 3, 20_000, 1_000);
    data.records.push(KmRecord {
        date: today - Duration::days(200),
        odometer: 11_000,
    });

    let years = compute_year_stats(&data);

    assert!(years[2].is_future);
    assert_eq!(years[2].km_driven, None, "a future year has driven nothing yet");
    assert!(years[0].km_driven.is_some(), "a completed year has a distance");
}

#[test]
fn year_stats_never_report_negative_distance() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 2, 20_000, 50_000);
    // An odometer reading below the configured start would otherwise produce a
    // negative delta for the year.
    data.records.push(KmRecord {
        date: today - Duration::days(100),
        odometer: 10_000,
    });

    for year in compute_year_stats(&data) {
        if let Some(km) = year.km_driven {
            assert!(km >= 0.0, "year {} reported {km} km", year.year_num);
        }
    }
}

// ─── compute_report_data ──────────────────────────────────────────────────────

#[test]
fn report_carries_the_configuration_through_unchanged() {
    let data = lease(date("2025-01-01"), 3, 20_000, 500);
    let report = compute_report_data(&data);

    assert_eq!(report.car_name, "Test Car");
    assert_eq!(report.lease_start, date("2025-01-01"));
    assert_eq!(report.lease_years, 3);
    assert_eq!(report.km_allowed_per_year, 20_000);
}

#[test]
fn report_totals_the_allowance_across_the_whole_lease() {
    let report = compute_report_data(&lease(date("2025-01-01"), 3, 20_000, 0));
    assert_eq!(report.km_allowed_total, 60_000);
}

/// The lease ends the day before the anniversary: a 3 year lease from
/// 2025-01-01 runs through 2027-12-31, not into 2028.
#[test]
fn report_lease_end_is_the_day_before_the_final_anniversary() {
    let report = compute_report_data(&lease(date("2025-01-01"), 3, 20_000, 0));
    assert_eq!(report.lease_end, date("2027-12-31"));
}

#[test]
fn report_exposes_the_most_recent_reading() {
    let mut data = lease(date("2025-01-01"), 3, 20_000, 0);
    data.records.push(KmRecord { date: date("2025-03-01"), odometer: 5_000 });
    data.records.push(KmRecord { date: date("2025-06-01"), odometer: 12_000 });

    let last = compute_report_data(&data).last_record.expect("a last record");
    assert_eq!(last.odometer, 12_000);
    assert_eq!(last.date, date("2025-06-01"));
}

#[test]
fn report_without_records_has_no_rate_or_projection() {
    let report = compute_report_data(&lease(date("2025-01-01"), 3, 20_000, 0));

    assert_eq!(report.avg_daily_rate, None);
    assert_eq!(report.projection_intervals, 0);
    assert_eq!(report.projected_total, None);
    assert!(report.current_year.is_none(), "no rate means nothing to project from");
}

#[test]
fn report_averages_the_daily_rate_across_intervals() {
    let mut data = lease(date("2025-01-01"), 3, 20_000, 0);
    // 100 km over 10 days, then 300 km over 10 days: rates of 10 and 30.
    data.records.push(KmRecord { date: date("2025-01-11"), odometer: 100 });
    data.records.push(KmRecord { date: date("2025-01-21"), odometer: 400 });

    let report = compute_report_data(&data);

    assert_eq!(report.projection_intervals, 2);
    assert_eq!(report.avg_daily_rate, Some(20.0), "mean of 10 and 30");
}

/// A backwards odometer reading yields a negative interval, which would drag
/// the average rate down and understate every projection.
#[test]
fn report_ignores_intervals_where_the_odometer_went_backwards() {
    let mut data = lease(date("2025-01-01"), 3, 20_000, 0);
    data.records.push(KmRecord { date: date("2025-01-11"), odometer: 1_000 });
    data.records.push(KmRecord { date: date("2025-01-21"), odometer: 500 });

    let report = compute_report_data(&data);

    // Only the first interval counts: 1000 km over 10 days.
    assert_eq!(report.projection_intervals, 1);
    assert_eq!(report.avg_daily_rate, Some(100.0));
}

#[test]
fn report_total_driven_is_the_sum_of_the_per_year_distances() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 3, 20_000, 1_000);
    data.records.push(KmRecord { date: today - Duration::days(300), odometer: 6_000 });
    data.records.push(KmRecord { date: today - Duration::days(100), odometer: 16_000 });

    let report = compute_report_data(&data);
    let summed: f64 = report.years.iter().filter_map(|y| y.km_driven).sum();

    assert!((report.total_driven - summed).abs() < 1e-9);
}

#[test]
fn report_projects_the_current_year_from_the_average_rate() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 3, 20_000, 0);
    data.records.push(KmRecord { date: today - Duration::days(300), odometer: 5_000 });
    data.records.push(KmRecord { date: today - Duration::days(100), odometer: 15_000 });

    let report = compute_report_data(&data);
    let projection = report.current_year.expect("a current year projection");

    assert_eq!(projection.year_num, 2);
    assert!(projection.days_elapsed < projection.days_total);
    assert!(
        projection.projected_year_total >= projection.km_driven,
        "projecting forward cannot reduce the distance already driven"
    );
    // `projected_diff` is the overshoot against the annual allowance.
    let expected = projection.projected_year_total - report.km_allowed_per_year as f64;
    assert!((projection.projected_diff - expected).abs() < 1e-9);
}

#[test]
fn report_projects_a_lease_total_covering_every_year() {
    let today = Local::now().date_naive();
    let mut data = lease(today - Duration::days(400), 3, 20_000, 0);
    data.records.push(KmRecord { date: today - Duration::days(300), odometer: 5_000 });
    data.records.push(KmRecord { date: today - Duration::days(100), odometer: 15_000 });

    let report = compute_report_data(&data);
    let projected = report.projected_total.expect("a projected total");

    assert!(
        projected >= report.total_driven,
        "the projection includes what has already been driven"
    );
}
