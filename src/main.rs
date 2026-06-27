use chrono::{Datelike, Duration, Local, NaiveDate};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

// ─── Data Structures ──────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct LeaseConfig {
    car_name: String,
    lease_start: NaiveDate,
    lease_years: u32,
    allowed_km_per_year: u32,
    start_odometer: u32,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct KmRecord {
    date: NaiveDate,
    odometer: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct LeaseData {
    config: LeaseConfig,
    records: Vec<KmRecord>,
}

// ─── File I/O ─────────────────────────────────────────────────────────────────

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("leasetrack.json")
}

fn load_data() -> Result<LeaseData, String> {
    let path = config_path();
    if !path.exists() {
        return Err(format!(
            "No lease data found. Run 'leasetrack init' to get started.\nConfig file: {}",
            path.display()
        ));
    }
    let content = fs::read_to_string(&path).map_err(|e| format!("Failed to read config: {}", e))?;
    serde_json::from_str(&content).map_err(|e| format!("Failed to parse config: {}", e))
}

fn save_data(data: &LeaseData) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }
    // Back up the existing file before overwriting
    if path.exists() {
        let backup = path.with_extension("json.backup");
        fs::copy(&path, &backup).map_err(|e| format!("Failed to write backup: {}", e))?;
    }
    let content =
        serde_json::to_string_pretty(data).map_err(|e| format!("Failed to serialize: {}", e))?;
    fs::write(&path, content).map_err(|e| format!("Failed to write config: {}", e))
}

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "leasetrack")]
#[command(version = "0.1.0")]
#[command(about = "Track kilometer usage for your lease car")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize lease car tracking (interactive)
    Init,

    /// Record an odometer reading
    Record {
        /// Total odometer reading in km
        odometer: u32,
        /// Date of reading (YYYY-MM-DD, defaults to today)
        #[arg(short, long, value_name = "DATE")]
        date: Option<String>,
    },

    /// Show km driven per year report
    Report,

    /// Show ASCII bar graph of km driven per year
    Graph,

    /// List all recorded odometer readings
    List,
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn prompt(question: &str) -> String {
    print!("{}", question);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
}

/// Add N years to a date, clamping to the last valid day if needed (e.g. Feb 29 → Feb 28).
fn add_years(date: NaiveDate, years: u32) -> NaiveDate {
    let new_year = date.year() + years as i32;
    NaiveDate::from_ymd_opt(new_year, date.month(), date.day())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(new_year, date.month(), 28).unwrap())
}

/// Format an integer with thousands separators: 20000 → "20,000"
fn fmt_km(n: u32) -> String {
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

fn fmt_km_f(n: f64) -> String {
    fmt_km(n.round() as u32)
}

/// Return the interpolated odometer value at `date`.
/// Uses a synthetic record at lease_start with start_odometer as baseline.
/// Returns None if there are no actual records yet (except for the start date itself).
fn km_at_date(
    records: &[KmRecord],
    start_odometer: u32,
    lease_start: NaiveDate,
    date: NaiveDate,
) -> Option<f64> {
    // Build working point set: synthetic start + actual records
    let mut pts: Vec<(NaiveDate, f64)> = records
        .iter()
        .map(|r| (r.date, r.odometer as f64))
        .collect();

    // Add synthetic start only if no actual record exists at lease_start
    if !pts.iter().any(|(d, _)| *d == lease_start) {
        pts.push((lease_start, start_odometer as f64));
    }
    pts.sort_by_key(|(d, _)| *d);

    // Without any actual records we can only return data at the start
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

    // Linear interpolation between surrounding points
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

struct YearStats {
    year_num: u32,
    start: NaiveDate,
    end: NaiveDate, // exclusive — equals start of next year
    km_driven: Option<f64>,
    is_current: bool,
    is_future: bool,
}

fn compute_year_stats(data: &LeaseData) -> Vec<YearStats> {
    let today = Local::now().date_naive();
    let cfg = &data.config;

    (0..cfg.lease_years)
        .map(|y| {
            let start = add_years(cfg.lease_start, y);
            let end = add_years(cfg.lease_start, y + 1);
            let is_future = today < start;
            let is_current = !is_future && today < end;

            let km_start = km_at_date(&data.records, cfg.start_odometer, cfg.lease_start, start);
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

// ─── Commands ─────────────────────────────────────────────────────────────────

fn cmd_init() -> Result<(), String> {
    let path = config_path();
    if path.exists() {
        let ans = prompt("Lease data already exists. Overwrite? [y/N] ");
        if ans.to_lowercase() != "y" {
            println!("Aborted.");
            return Ok(());
        }
    }

    println!("Setting up lease car tracking");
    println!("{}", "─".repeat(40));

    let car_name = loop {
        let v = prompt("Car name: ");
        if !v.is_empty() {
            break v;
        }
        println!("Car name cannot be empty.");
    };

    let lease_start = loop {
        let v = prompt("Lease start date (YYYY-MM-DD): ");
        match NaiveDate::parse_from_str(&v, "%Y-%m-%d") {
            Ok(d) => break d,
            Err(_) => println!("Invalid date. Use YYYY-MM-DD format (e.g. 2024-01-15)."),
        }
    };

    let lease_years: u32 = loop {
        let v = prompt("Number of lease years (1–10): ");
        match v.parse::<u32>() {
            Ok(n) if (1..=10).contains(&n) => break n,
            _ => println!("Please enter a number between 1 and 10."),
        }
    };

    let allowed_km: u32 = loop {
        let v = prompt("Allowed km per year: ");
        match v.parse::<u32>() {
            Ok(n) if n > 0 => break n,
            _ => println!("Please enter a positive number."),
        }
    };

    let start_odometer: u32 = loop {
        let v = prompt("Starting odometer in km (press Enter for 0): ");
        if v.is_empty() {
            break 0;
        }
        match v.parse::<u32>() {
            Ok(n) => break n,
            _ => println!("Please enter a non-negative number."),
        }
    };

    let lease_end = add_years(lease_start, lease_years) - Duration::days(1);
    let total_km = allowed_km * lease_years;

    let data = LeaseData {
        config: LeaseConfig {
            car_name: car_name.clone(),
            lease_start,
            lease_years,
            allowed_km_per_year: allowed_km,
            start_odometer,
        },
        records: Vec::new(),
    };

    save_data(&data)?;

    println!();
    println!("Lease car '{}' configured!", car_name);
    println!("Period:  {} → {}", lease_start, lease_end);
    println!(
        "Allowed: {} km/year  ({} km total over {} years)",
        fmt_km(allowed_km),
        fmt_km(total_km),
        lease_years
    );
    println!("Saved:   {}", config_path().display());

    Ok(())
}

fn cmd_record(odometer: u32, date_str: Option<String>) -> Result<(), String> {
    let mut data = load_data()?;
    let cfg = &data.config;

    let date = match date_str {
        Some(ref s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| format!("Invalid date '{}'. Use YYYY-MM-DD.", s))?,
        None => Local::now().date_naive(),
    };

    // Basic validation
    if odometer < cfg.start_odometer {
        return Err(format!(
            "Odometer {} km is below the starting odometer {} km.",
            fmt_km(odometer),
            fmt_km(cfg.start_odometer)
        ));
    }

    let lease_end = add_years(cfg.lease_start, cfg.lease_years);
    if date < cfg.lease_start || date > lease_end {
        println!(
            "Warning: {} is outside the lease period ({} – {})",
            date,
            cfg.lease_start,
            lease_end - Duration::days(1)
        );
    }

    // Duplicate date check
    if let Some(existing) = data.records.iter().find(|r| r.date == date) {
        return Err(format!(
            "A record for {} already exists ({} km). Remove it before updating.",
            date,
            fmt_km(existing.odometer)
        ));
    }

    // Monotonicity warnings
    data.records.sort_by_key(|r| r.date);
    for r in &data.records {
        if r.date < date && r.odometer > odometer {
            println!(
                "Warning: earlier record on {} shows {} km > {} km (odometer went backwards?)",
                r.date,
                fmt_km(r.odometer),
                fmt_km(odometer)
            );
        }
        if r.date > date && r.odometer < odometer {
            println!(
                "Warning: later record on {} shows {} km < {} km (odometer went backwards?)",
                r.date,
                fmt_km(r.odometer),
                fmt_km(odometer)
            );
        }
    }

    // Build km-since-last-record label
    let km_since = data
        .records
        .iter()
        .filter(|r| r.date < date)
        .last()
        .map(|prev| {
            format!(
                " (+{} km since {})",
                fmt_km(odometer - prev.odometer),
                prev.date
            )
        })
        .unwrap_or_default();

    data.records.push(KmRecord { date, odometer });
    data.records.sort_by_key(|r| r.date);
    save_data(&data)?;

    println!("Recorded: {} km on {}{}", fmt_km(odometer), date, km_since);
    Ok(())
}

fn cmd_report(data: &LeaseData) -> Result<(), String> {
    let today = Local::now().date_naive();
    let cfg = &data.config;
    let lease_end = add_years(cfg.lease_start, cfg.lease_years) - Duration::days(1);
    let total_allowed = cfg.allowed_km_per_year * cfg.lease_years;

    // Header
    println!("Lease car:  {}", cfg.car_name);
    println!(
        "Period:     {} → {} ({} years)",
        cfg.lease_start, lease_end, cfg.lease_years
    );
    println!(
        "Allowed:    {} km/year  |  {} km total",
        fmt_km(cfg.allowed_km_per_year),
        fmt_km(total_allowed)
    );
    if let Some(last) = data.records.last() {
        println!("Last entry: {} km on {}", fmt_km(last.odometer), last.date);
    }
    println!();

    let line = "─".repeat(76);
    println!("{}", line);
    println!(
        " {:>4}  {:<24}  {:>9}  {:>9}  {:>10}  {}",
        "Year", "Period", "Driven", "Allowed", "Difference", ""
    );
    println!("{}", line);

    let stats = compute_year_stats(data);
    let mut total_driven = 0.0f64;

    for s in &stats {
        let period = format!("{} – {}", s.start, s.end - Duration::days(1));

        let (driven_s, diff_s, icon): (String, String, &str) = if s.is_future {
            ("—".into(), "—".into(), "")
        } else if let Some(km) = s.km_driven {
            let allowed = cfg.allowed_km_per_year as f64;
            let diff = km - allowed;

            let driven_label = if s.is_current {
                format!("{}*", fmt_km_f(km))
            } else {
                fmt_km_f(km)
            };

            let diff_label = if diff.abs() < 1.0 {
                "=".into()
            } else if diff > 0.0 {
                format!("+{}", fmt_km_f(diff))
            } else {
                format!("-{}", fmt_km_f(diff.abs()))
            };

            let icon = if s.is_current {
                "▶"
            } else if diff > 0.0 {
                "⚠"
            } else {
                "✓"
            };

            total_driven += km;
            (driven_label, diff_label, icon)
        } else {
            ("—".into(), "—".into(), "")
        };

        println!(
            " {:>4}  {:<24}  {:>9}  {:>9}  {:>10}  {}",
            s.year_num,
            period,
            driven_s,
            fmt_km(cfg.allowed_km_per_year),
            diff_s,
            icon
        );
    }

    println!("{}", line);

    let total_diff = total_driven - total_allowed as f64;
    let diff_label: String = if total_diff.abs() < 1.0 {
        "=".into()
    } else if total_diff > 0.0 {
        format!("+{}", fmt_km_f(total_diff))
    } else {
        format!("-{}", fmt_km_f(total_diff.abs()))
    };

    println!(
        " {:>4}  {:<24}  {:>9}  {:>9}  {:>10}",
        "Total",
        "",
        fmt_km_f(total_driven),
        fmt_km(total_allowed),
        diff_label
    );

    // Compute average daily rate across all recorded intervals (used for projections)
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
    let avg_rate_opt: Option<f64> = if interval_rates.is_empty() {
        None
    } else {
        Some(interval_rates.iter().sum::<f64>() / interval_rates.len() as f64)
    };

    // Current year note
    for s in &stats {
        if s.is_current {
            if let Some(km) = s.km_driven {
                let days_elapsed = (today - s.start).num_days() as f64;
                let days_total = (s.end - s.start).num_days() as f64;
                if days_elapsed > 0.0 {
                    let avg_rate = avg_rate_opt.unwrap_or(km / days_elapsed);
                    let days_remaining = days_total - days_elapsed;
                    let projected = km + avg_rate * days_remaining;
                    let proj_diff = projected - cfg.allowed_km_per_year as f64;

                    println!();
                    println!(
                        "* Year {} in progress — {} of {} days elapsed",
                        s.year_num, days_elapsed as u32, days_total as u32
                    );
                    println!("  Current:   {} km driven", fmt_km_f(km));
                    println!(
                        "  Avg rate:  {:.1} km/day (averaged over {} intervals)",
                        avg_rate,
                        interval_rates.len()
                    );
                    println!("  Projected: {} km by end of year", fmt_km_f(projected));
                    if proj_diff.abs() > 1.0 {
                        let (icon, word) = if proj_diff > 0.0 {
                            ("⚠", "over")
                        } else {
                            ("✓", "under")
                        };
                        println!(
                            "  Outlook:   {} km {} the annual limit {}",
                            fmt_km_f(proj_diff.abs()),
                            word,
                            icon
                        );
                    }
                }
            }
        }
    }

    // End-of-lease total projection
    if let Some(avg_rate) = avg_rate_opt {
        let projected_total: f64 = stats
            .iter()
            .map(|s| {
                if !s.is_current && !s.is_future {
                    // Completed year — use actual km
                    s.km_driven.unwrap_or(0.0)
                } else if s.is_current {
                    // Current year — actual so far + rate × remaining days
                    let km = s.km_driven.unwrap_or(0.0);
                    let days_elapsed = (today - s.start).num_days() as f64;
                    let days_total = (s.end - s.start).num_days() as f64;
                    km + avg_rate * (days_total - days_elapsed)
                } else {
                    // Future year — rate × full year
                    let days_total = (s.end - s.start).num_days() as f64;
                    avg_rate * days_total
                }
            })
            .sum();

        let proj_diff = projected_total - total_allowed as f64;
        let (diff_label, icon) = if proj_diff.abs() < 1.0 {
            ("on the limit".into(), "✓")
        } else if proj_diff > 0.0 {
            (format!("{} km over the limit", fmt_km_f(proj_diff)), "⚠")
        } else {
            (
                format!("{} km under the limit", fmt_km_f(proj_diff.abs())),
                "✓",
            )
        };

        println!();
        println!(
            "End-of-lease projection ({})",
            add_years(cfg.lease_start, cfg.lease_years) - Duration::days(1)
        );
        println!(
            "  Projected total: {} km  |  allowed: {} km",
            fmt_km_f(projected_total),
            fmt_km(total_allowed)
        );
        println!("  Outlook:         {} {}", diff_label, icon);
        println!(
            "  Based on avg rate of {:.1} km/day over {} intervals",
            avg_rate,
            interval_rates.len()
        );
    }

    Ok(())
}

fn cmd_graph(data: &LeaseData) -> Result<(), String> {
    let cfg = &data.config;
    let stats = compute_year_stats(data);
    let allowed = cfg.allowed_km_per_year as f64;
    const BAR_WIDTH: usize = 50;

    // Scale to the maximum of allowed and any recorded km, plus 5% headroom
    let max_km = stats
        .iter()
        .filter_map(|s| s.km_driven)
        .fold(allowed, f64::max);
    let scale = max_km * 1.05;

    println!("Km driven per year — {}", cfg.car_name);
    println!("Allowed: {} km/year", fmt_km(cfg.allowed_km_per_year));
    let sep = "─".repeat(BAR_WIDTH + 24);
    println!("{}", sep);

    for s in &stats {
        let label = format!("Year {}", s.year_num);
        let bar = match s.km_driven {
            Some(km) => render_bar(km, allowed, scale, BAR_WIDTH),
            None => " ".repeat(BAR_WIDTH),
        };
        let value = if s.is_future {
            format!("{:>9} km", "—")
        } else if let Some(km) = s.km_driven {
            let icon = if s.is_current {
                "▶"
            } else if km > allowed {
                "⚠"
            } else {
                "✓"
            };
            format!("{:>9} km {}", fmt_km_f(km), icon)
        } else {
            format!("{:>9} km", "—")
        };
        println!("{:<7} │{}│ {}", label, bar, value);
    }

    println!("{}", sep);

    // Ruler: show km values at 0%, 25%, 50%, 75%, 100% of scale
    let ruler_offset = 9usize; // "Year X  │" width
    let marks: Vec<(usize, String)> = [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|&frac| {
            let km = scale * frac;
            let pos = (frac * BAR_WIDTH as f64).round() as usize;
            (pos, fmt_km(km.round() as u32))
        })
        .collect();

    // Build ruler line
    let mut ruler = vec![' '; BAR_WIDTH + ruler_offset + 2];
    for (pos, label) in &marks {
        let start = ruler_offset + pos;
        let start = if start + label.len() > ruler.len() {
            ruler.len().saturating_sub(label.len())
        } else {
            start
        };
        for (j, c) in label.chars().enumerate() {
            if start + j < ruler.len() {
                ruler[start + j] = c;
            }
        }
    }
    println!("{}", ruler.iter().collect::<String>());

    // Show where the allowed mark falls
    let allowed_pos = ruler_offset + ((allowed / scale) * BAR_WIDTH as f64).round() as usize;
    let mut marker = vec![' '; BAR_WIDTH + ruler_offset + 2];
    if allowed_pos < marker.len() {
        marker[allowed_pos] = '^';
        let label = "allowed";
        let lstart = allowed_pos.saturating_sub(label.len() / 2);
        let lstart = lstart.min(marker.len().saturating_sub(label.len()));
        // Only print if it doesn't overlap the ^ itself by much
        for (j, c) in label.chars().enumerate() {
            if lstart + j < marker.len() && lstart + j != allowed_pos {
                marker[lstart + j] = c;
            }
        }
    }

    println!();
    println!("Legend: █ driven (within limit)   ░ unused allowance   ▓ over limit");

    Ok(())
}

fn render_bar(km: f64, allowed: f64, scale: f64, width: usize) -> String {
    let driven_w = ((km / scale) * width as f64).round() as usize;
    let allowed_w = ((allowed / scale) * width as f64).round() as usize;
    let driven_w = driven_w.min(width);
    let allowed_w = allowed_w.min(width);

    let mut bar: Vec<char> = vec![' '; width];

    if km <= allowed {
        for i in 0..driven_w {
            bar[i] = '█';
        }
        for i in driven_w..allowed_w {
            bar[i] = '░';
        }
    } else {
        for i in 0..allowed_w {
            bar[i] = '█';
        }
        for i in allowed_w..driven_w {
            bar[i] = '▓';
        }
    }

    bar.into_iter().collect()
}

fn cmd_list(data: &LeaseData) -> Result<(), String> {
    let cfg = &data.config;
    println!("Odometer records — {}", cfg.car_name);

    if data.records.is_empty() {
        println!("No records yet.");
        println!("Use 'leasetrack record <km>' to add an odometer reading.");
        return Ok(());
    }

    let sep = "─".repeat(50);
    println!("{}", sep);
    println!(" {:<12}  {:>12}  {:>14}", "Date", "Odometer", "Since prev");
    println!("{}", sep);

    // Synthetic start record for display
    let start_rec = KmRecord {
        date: cfg.lease_start,
        odometer: cfg.start_odometer,
    };
    let all: Vec<&KmRecord> = std::iter::once(&start_rec)
        .chain(data.records.iter())
        .collect();

    for (i, r) in all.iter().enumerate() {
        let since = if i == 0 {
            "—".to_string()
        } else {
            let prev = all[i - 1];
            let delta = r.odometer as i64 - prev.odometer as i64;
            if delta >= 0 {
                format!("+{} km", fmt_km(delta as u32))
            } else {
                format!("-{} km", fmt_km((-delta) as u32))
            }
        };
        let note = if i == 0 { "  (lease start)" } else { "" };
        println!(
            " {:<12}  {:>9} km  {:>14}{}",
            r.date,
            fmt_km(r.odometer),
            since,
            note
        );
    }

    println!("{}", sep);
    println!("Total: {} records", data.records.len());
    Ok(())
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    let result: Result<(), String> = match cli.command {
        Commands::Init => cmd_init(),
        Commands::Record { odometer, date } => cmd_record(odometer, date),
        Commands::Report => load_data().and_then(|d| cmd_report(&d)),
        Commands::Graph => load_data().and_then(|d| cmd_graph(&d)),
        Commands::List => load_data().and_then(|d| cmd_list(&d)),
    };

    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}
