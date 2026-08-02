use chrono::{Datelike, Duration, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

// ─── Data Structures ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LeaseConfig {
    pub car_name: String,
    pub lease_start: NaiveDate,
    pub lease_years: u32,
    pub allowed_km_per_year: u32,
    pub start_odometer: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct KmRecord {
    pub date: NaiveDate,
    pub odometer: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LeaseData {
    pub config: LeaseConfig,
    pub records: Vec<KmRecord>,
}

// ─── File I/O ─────────────────────────────────────────────────────────────────

/// Returns the data file path. Override with `LEASETRACK_DATA_FILE` env var.
pub fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("LEASETRACK_DATA_FILE") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("leasetrack.json")
}

pub fn load_data() -> Result<LeaseData, String> {
    let path = config_path();
    if !path.exists() {
        return Err(format!(
            "No lease data found. Run 'leasetrack init' to get started.\nConfig file: {}",
            path.display()
        ));
    }
    let content =
        fs::read_to_string(&path).map_err(|e| format!("Failed to read config: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse config: {}", e))
}

pub fn save_data(data: &LeaseData) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }
    if path.exists() {
        let backup = path.with_extension("json.backup");
        fs::copy(&path, &backup).map_err(|e| format!("Failed to write backup: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(data).map_err(|e| format!("Failed to serialize: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("Failed to write config: {}", e))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Add N years to a date, clamping to the last valid day if needed (e.g. Feb 29 → Feb 28).
pub fn add_years(date: NaiveDate, years: u32) -> NaiveDate {
    let new_year = date.year() + years as i32;
    NaiveDate::from_ymd_opt(new_year, date.month(), date.day())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(new_year, date.month(), 28).unwrap())
}

/// Format an integer with thousands separators: 20000 → "20,000"
pub fn fmt_km(n: u32) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut result = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            result.push(',');
        }
        result.push(*c);
    }
    result
}

pub fn fmt_km_f(n: f64) -> String {
    fmt_km(n.round() as u32)
}

/// Return the interpolated odometer value at `date`.
pub fn km_at_date(
    records: &[KmRecord],
    start_odometer: u32,
    lease_start: NaiveDate,
    date: NaiveDate,
) -> Option<f64> {
    let mut pts: Vec<(NaiveDate, f64)> = records
        .iter()
        .map(|r| (r.date, r.odometer as f64))
        .collect();

    if !pts.iter().any(|(d, _)| *d == lease_start) {
        pts.push((lease_start, start_odometer as f64));
    }
    pts.sort_by_key(|(d, _)| *d);

    if records.is_empty() {
        return if date == lease_start {
            Some(start_odometer as f64)
        } else {
            None
        };
    }

    if pts.is_empty() {
        return None;
    }

    let (first_d, first_k) = pts[0];
    let (last_d, last_k) = *pts.last().unwrap();

    if date <= first_d {
        return Some(first_k);
    }
    if date >= last_d {
        return Some(last_k);
    }

    for i in 0..pts.len() - 1 {
        let (d1, k1) = pts[i];
        let (d2, k2) = pts[i + 1];
        if d1 <= date && date < d2 {
            let total = (d2 - d1).num_days() as f64;
            let elapsed = (date - d1).num_days() as f64;
            return Some(k1 + (elapsed / total) * (k2 - k1));
        }
    }

    None
}

// ─── Year / Report Computations ───────────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct YearStats {
    pub year_num: u32,
    pub start: NaiveDate,
    pub end: NaiveDate, // exclusive — equals start of next year
    pub km_driven: Option<f64>,
    pub is_current: bool,
    pub is_future: bool,
}

pub fn compute_year_stats(data: &LeaseData) -> Vec<YearStats> {
    let today = Local::now().date_naive();
    let cfg = &data.config;

    (0..cfg.lease_years)
        .map(|y| {
            let start = add_years(cfg.lease_start, y);
            let end = add_years(cfg.lease_start, y + 1);
            let is_future = today < start;
            let is_current = !is_future && today < end;

            let km_start =
                km_at_date(&data.records, cfg.start_odometer, cfg.lease_start, start);
            let effective_end = if is_current { today } else { end };
            let km_end = if is_future {
                None
            } else {
                km_at_date(
                    &data.records,
                    cfg.start_odometer,
                    cfg.lease_start,
                    effective_end,
                )
            };

            let km_driven = match (km_start, km_end) {
                (Some(s), Some(e)) => Some((e - s).max(0.0)),
                _ => None,
            };

            YearStats {
                year_num: y + 1,
                start,
                end,
                km_driven,
                is_current,
                is_future,
            }
        })
        .collect()
}

#[derive(Debug, Serialize, Clone)]
pub struct CurrentYearProjection {
    pub year_num: u32,
    pub days_elapsed: u32,
    pub days_total: u32,
    pub km_driven: f64,
    pub avg_daily_rate: f64,
    pub projected_year_total: f64,
    /// Positive means over the annual limit.
    pub projected_diff: f64,
}

#[derive(Debug, Serialize)]
pub struct ReportData {
    pub car_name: String,
    pub lease_start: NaiveDate,
    pub lease_end: NaiveDate,
    pub lease_years: u32,
    pub km_allowed_per_year: u32,
    pub km_allowed_total: u32,
    pub last_record: Option<KmRecord>,
    pub years: Vec<YearStats>,
    pub total_driven: f64,
    pub avg_daily_rate: Option<f64>,
    pub projection_intervals: usize,
    pub projected_total: Option<f64>,
    pub current_year: Option<CurrentYearProjection>,
}

pub fn compute_report_data(data: &LeaseData) -> ReportData {
    let today = Local::now().date_naive();
    let cfg = &data.config;
    let lease_end = add_years(cfg.lease_start, cfg.lease_years) - Duration::days(1);
    let km_allowed_total = cfg.allowed_km_per_year * cfg.lease_years;

    let years = compute_year_stats(data);
    let total_driven: f64 = years.iter().filter_map(|s| s.km_driven).sum();

    // Average daily rate across all recorded intervals
    let synthetic_start = KmRecord {
        date: cfg.lease_start,
        odometer: cfg.start_odometer,
    };
    let all_recs: Vec<&KmRecord> = std::iter::once(&synthetic_start)
        .chain(data.records.iter())
        .collect();
    let interval_rates: Vec<f64> = all_recs
        .windows(2)
        .filter_map(|w| {
            let days = (w[1].date - w[0].date).num_days() as f64;
            let km_diff = w[1].odometer as f64 - w[0].odometer as f64;
            if days > 0.0 && km_diff >= 0.0 {
                Some(km_diff / days)
            } else {
                None
            }
        })
        .collect();
    let avg_daily_rate = if interval_rates.is_empty() {
        None
    } else {
        Some(interval_rates.iter().sum::<f64>() / interval_rates.len() as f64)
    };

    // Current year projection
    let current_year = years.iter().find(|s| s.is_current).and_then(|s| {
        avg_daily_rate.and_then(|rate| {
            s.km_driven.map(|km| {
                let days_elapsed = (today - s.start).num_days() as f64;
                let days_total = (s.end - s.start).num_days() as f64;
                let days_remaining = days_total - days_elapsed;
                let projected = km + rate * days_remaining;
                CurrentYearProjection {
                    year_num: s.year_num,
                    days_elapsed: days_elapsed as u32,
                    days_total: days_total as u32,
                    km_driven: km,
                    avg_daily_rate: rate,
                    projected_year_total: projected,
                    projected_diff: projected - cfg.allowed_km_per_year as f64,
                }
            })
        })
    });

    // End-of-lease projected total
    let projected_total = avg_daily_rate.map(|rate| {
        years
            .iter()
            .map(|s| {
                if !s.is_current && !s.is_future {
                    s.km_driven.unwrap_or(0.0)
                } else if s.is_current {
                    let km = s.km_driven.unwrap_or(0.0);
                    let days_elapsed = (today - s.start).num_days() as f64;
                    let days_total = (s.end - s.start).num_days() as f64;
                    km + rate * (days_total - days_elapsed)
                } else {
                    let days_total = (s.end - s.start).num_days() as f64;
                    rate * days_total
                }
            })
            .sum()
    });

    ReportData {
        car_name: cfg.car_name.clone(),
        lease_start: cfg.lease_start,
        lease_end,
        lease_years: cfg.lease_years,
        km_allowed_per_year: cfg.allowed_km_per_year,
        km_allowed_total,
        last_record: data.records.last().cloned(),
        years,
        total_driven,
        avg_daily_rate,
        projection_intervals: interval_rates.len(),
        projected_total,
        current_year,
    }
}

// ─── Business Logic ───────────────────────────────────────────────────────────

/// Validate and insert a new odometer record into `data`.
/// Returns `Ok(warnings)` on success, `Err(message)` on hard failure.
/// Does NOT call `save_data`; the caller is responsible for persisting.
pub fn add_record(
    data: &mut LeaseData,
    odometer: u32,
    date: NaiveDate,
) -> Result<Vec<String>, String> {
    let cfg = &data.config;

    if odometer < cfg.start_odometer {
        return Err(format!(
            "Odometer {} km is below the starting odometer {} km.",
            fmt_km(odometer),
            fmt_km(cfg.start_odometer)
        ));
    }

    if let Some(existing) = data.records.iter().find(|r| r.date == date) {
        return Err(format!(
            "A record for {} already exists ({} km). Remove it before updating.",
            date,
            fmt_km(existing.odometer)
        ));
    }

    let mut warnings = Vec::new();

    let lease_end = add_years(cfg.lease_start, cfg.lease_years);
    if date < cfg.lease_start || date > lease_end {
        warnings.push(format!(
            "{} is outside the lease period ({} – {})",
            date,
            cfg.lease_start,
            lease_end - Duration::days(1)
        ));
    }

    data.records.sort_by_key(|r| r.date);
    for r in &data.records {
        if r.date < date && r.odometer > odometer {
            warnings.push(format!(
                "Earlier record on {} shows {} km > {} km (odometer went backwards?)",
                r.date,
                fmt_km(r.odometer),
                fmt_km(odometer)
            ));
        }
        if r.date > date && r.odometer < odometer {
            warnings.push(format!(
                "Later record on {} shows {} km < {} km (odometer went backwards?)",
                r.date,
                fmt_km(r.odometer),
                fmt_km(odometer)
            ));
        }
    }

    data.records.push(KmRecord { date, odometer });
    data.records.sort_by_key(|r| r.date);

    Ok(warnings)
}
