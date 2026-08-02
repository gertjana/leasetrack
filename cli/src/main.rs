use chrono::{Duration, Local, NaiveDate};
use clap::{Parser, Subcommand};
use leasetrack_core::{
    add_record, add_years, compute_report_data, compute_year_stats, config_path, fmt_km, fmt_km_f,
    load_data, save_data, LeaseConfig, LeaseData,
};
use std::io::{self, Write};

// ─── CLI Definition ───────────────────────────────────────────────────────────

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

// ─── Prompt Helper ────────────────────────────────────────────────────────────

fn prompt(question: &str) -> String {
    print!("{}", question);
    io::stdout().flush().unwrap();
    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    input.trim().to_string()
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

    let date = match date_str {
        Some(ref s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .map_err(|_| format!("Invalid date '{}'. Use YYYY-MM-DD.", s))?,
        None => Local::now().date_naive(),
    };

    // Capture km-since-last before inserting the new record
    let km_since = data
        .records
        .iter()
        .filter(|r| r.date < date)
        .last()
        .map(|prev| {
            format!(
                " (+{} km since {})",
                fmt_km(odometer.saturating_sub(prev.odometer)),
                prev.date
            )
        })
        .unwrap_or_default();

    let warnings = add_record(&mut data, odometer, date)?;
    save_data(&data)?;

    for w in &warnings {
        println!("Warning: {}", w);
    }
    println!("Recorded: {} km on {}{}", fmt_km(odometer), date, km_since);
    Ok(())
}

fn cmd_report(data: &leasetrack_core::LeaseData) -> Result<(), String> {
    let report = compute_report_data(data);

    println!("Lease car:  {}", report.car_name);
    println!(
        "Period:     {} → {} ({} years)",
        report.lease_start, report.lease_end, report.lease_years
    );
    println!(
        "Allowed:    {} km/year  |  {} km total",
        fmt_km(report.km_allowed_per_year),
        fmt_km(report.km_allowed_total)
    );
    if let Some(last) = &report.last_record {
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

    for s in &report.years {
        let period = format!("{} – {}", s.start, s.end - Duration::days(1));

        let (driven_s, diff_s, icon): (String, String, &str) = if s.is_future {
            ("—".into(), "—".into(), "")
        } else if let Some(km) = s.km_driven {
            let allowed = report.km_allowed_per_year as f64;
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

            (driven_label, diff_label, icon)
        } else {
            ("—".into(), "—".into(), "")
        };

        println!(
            " {:>4}  {:<24}  {:>9}  {:>9}  {:>10}  {}",
            s.year_num,
            period,
            driven_s,
            fmt_km(report.km_allowed_per_year),
            diff_s,
            icon
        );
    }

    println!("{}", line);

    let total_diff = report.total_driven - report.km_allowed_total as f64;
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
        fmt_km_f(report.total_driven),
        fmt_km(report.km_allowed_total),
        diff_label
    );

    // Current year projection block
    if let Some(cy) = &report.current_year {
        println!();
        println!(
            "* Year {} in progress — {} of {} days elapsed",
            cy.year_num, cy.days_elapsed, cy.days_total
        );
        println!("  Current:   {} km driven", fmt_km_f(cy.km_driven));
        println!(
            "  Avg rate:  {:.1} km/day (averaged over {} intervals)",
            cy.avg_daily_rate, report.projection_intervals
        );
        println!(
            "  Projected: {} km by end of year",
            fmt_km_f(cy.projected_year_total)
        );
        if cy.projected_diff.abs() > 1.0 {
            let (icon, word) = if cy.projected_diff > 0.0 {
                ("⚠", "over")
            } else {
                ("✓", "under")
            };
            println!(
                "  Outlook:   {} km {} the annual limit {}",
                fmt_km_f(cy.projected_diff.abs()),
                word,
                icon
            );
        }
    }

    // End-of-lease total projection
    if let (Some(avg_rate), Some(projected_total)) =
        (report.avg_daily_rate, report.projected_total)
    {
        let proj_diff = projected_total - report.km_allowed_total as f64;
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
        println!("End-of-lease projection ({})", report.lease_end);
        println!(
            "  Projected total: {} km  |  allowed: {} km",
            fmt_km_f(projected_total),
            fmt_km(report.km_allowed_total)
        );
        println!("  Outlook:         {} {}", diff_label, icon);
        println!(
            "  Based on avg rate of {:.1} km/day over {} intervals",
            avg_rate, report.projection_intervals
        );
    }

    Ok(())
}

fn cmd_graph(data: &leasetrack_core::LeaseData) -> Result<(), String> {
    let cfg = &data.config;
    let stats = compute_year_stats(data);
    let allowed = cfg.allowed_km_per_year as f64;
    const BAR_WIDTH: usize = 50;

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

    let ruler_offset = 9usize;
    let marks: Vec<(usize, String)> = [0.0, 0.25, 0.5, 0.75, 1.0]
        .iter()
        .map(|&frac| {
            let km = scale * frac;
            let pos = (frac * BAR_WIDTH as f64).round() as usize;
            (pos, fmt_km(km.round() as u32))
        })
        .collect();

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

fn cmd_list(data: &leasetrack_core::LeaseData) -> Result<(), String> {
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

    let start_rec = leasetrack_core::KmRecord {
        date: cfg.lease_start,
        odometer: cfg.start_odometer,
    };
    let all: Vec<&leasetrack_core::KmRecord> = std::iter::once(&start_rec)
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
