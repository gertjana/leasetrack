//! The main dashboard: lease info, odometer recording, projections and charts.

use leasetrack_core::{LeaseData, add_record, compute_report_data, load_user_data, save_user_data};
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::see_other,
        response::{IntoResponse, Response},
        route,
    },
    view::{View, component, view},
};

use super::setup::{ConfigForm, parse_config};
use super::{current_email, layout::document};

// ─── View models ──────────────────────────────────────────────────────────────

/// One bar in the "km per year" chart.
struct YearBar {
    year_num: u32,
    km: u32,
    /// Share of the annual allowance already driven, capped at 125%.
    pct: u32,
    /// For the current year, the projected remainder drawn past `pct`.
    proj_pct: Option<u32>,
    status: &'static str,
}

/// One row of the odometer records table.
struct RecordRow {
    date: String,
    odometer: u32,
    /// Distance since the previous reading; absent for the first record.
    delta: Option<i64>,
}

// ─── Pages ────────────────────────────────────────────────────────────────────

/// `GET /dashboard`
#[route(GET "/dashboard")]
async fn dashboard(cx: &Cx) -> Result<Response> {
    let Some(email) = current_email(cx).await? else {
        return see_other("/login").into_response(cx);
    };
    render_dashboard(cx, &email, None, None).await
}

#[derive(Deserialize)]
struct RecordForm {
    odometer: String,
    date: String,
}

/// `POST /web/record`
#[route(POST "/web/record")]
async fn web_record(cx: &Cx, Form(form): Form<RecordForm>) -> Result<Response> {
    let Some(email) = current_email(cx).await? else {
        return see_other("/login").into_response(cx);
    };

    let odometer = form.odometer.trim().parse::<u32>();
    let date = chrono::NaiveDate::parse_from_str(form.date.trim(), "%Y-%m-%d");

    let (success, error) = match (odometer, date) {
        (Ok(odo), Ok(day)) => match load_user_data(&email) {
            Ok(mut data) => match add_record(&mut data, odo, day) {
                Ok(warnings) => match save_user_data(&email, &data) {
                    Ok(()) => {
                        let message = if warnings.is_empty() {
                            format!("Recorded {odo} km on {day}")
                        } else {
                            format!("Recorded {odo} km on {day} ({})", warnings.join("; "))
                        };
                        (Some(message), None)
                    }
                    // The reading was accepted in memory but never persisted.
                    // Reporting success here would promise a record that is
                    // already gone on the next page load.
                    Err(e) => (None, Some(e)),
                },
                Err(e) => (None, Some(e)),
            },
            Err(e) => (None, Some(e)),
        },
        (Err(_), _) => (
            None,
            Some("Invalid odometer value — must be a number.".to_string()),
        ),
        (_, Err(_)) => (
            None,
            Some("Invalid date — use YYYY-MM-DD format.".to_string()),
        ),
    };

    render_dashboard(cx, &email, error, success).await
}

/// `POST /web/config`
#[route(POST "/web/config")]
async fn web_config(cx: &Cx, Form(form): Form<ConfigForm>) -> Result<Response> {
    let Some(email) = current_email(cx).await? else {
        return see_other("/login").into_response(cx);
    };

    let config = match parse_config(&form) {
        Ok(config) => config,
        Err(message) => return render_dashboard(cx, &email, Some(message), None).await,
    };

    // A user editing their lease before recording anything has no stored data
    // yet, so fall back to a fresh, empty record set.
    let mut data = load_user_data(&email).unwrap_or_else(|_| LeaseData {
        config: config.clone(),
        records: vec![],
    });
    data.config = config;

    if let Err(e) = save_user_data(&email, &data) {
        return render_dashboard(cx, &email, Some(e), None).await;
    }

    render_dashboard(cx, &email, None, Some("Configuration saved.".to_string())).await
}

// ─── Rendering ────────────────────────────────────────────────────────────────

async fn render_dashboard(
    cx: &Cx,
    email: &str,
    record_error: Option<String>,
    record_success: Option<String>,
) -> Result<Response> {
    let Ok(data) = load_user_data(email) else {
        return see_other("/setup").into_response(cx);
    };

    let report = compute_report_data(&data);
    let today = chrono::Local::now().date_naive().to_string();

    // Records newest-first, each with the distance since the previous reading.
    let records: Vec<RecordRow> = data
        .records
        .iter()
        .enumerate()
        .rev()
        .map(|(i, rec)| RecordRow {
            date: rec.date.to_string(),
            odometer: rec.odometer,
            delta: (i > 0).then(|| rec.odometer as i64 - data.records[i - 1].odometer as i64),
        })
        .collect();

    let allowed = report.km_allowed_per_year as f64;
    let projected_year_total = report.current_year.as_ref().map(|p| p.projected_year_total);

    let years: Vec<YearBar> = report
        .years
        .iter()
        .map(|y| {
            let km = y.km_driven.unwrap_or(0.0);
            let pct = ((km / allowed) * 100.0).min(125.0) as u32;
            let status = if y.is_future {
                "future"
            } else if y.is_current {
                "current"
            } else if km > allowed {
                "over"
            } else {
                "ok"
            };
            // For the current year, add a projected-remainder segment.
            let proj_pct = y.is_current.then(|| {
                projected_year_total.map(|total| {
                    let full = (total / allowed * 100.0).min(125.0) as u32;
                    full.saturating_sub(pct)
                })
            });
            YearBar {
                year_num: y.year_num,
                km: km as u32,
                pct,
                proj_pct: proj_pct.flatten(),
                status,
            }
        })
        .collect();

    let current = report.current_year.as_ref();
    let proj_year_diff = current.map(|p| p.projected_diff as i64);
    let proj_year_total = current.map(|p| p.projected_year_total as u32);
    let proj_total_diff = report
        .projected_total
        .map(|total| total as i64 - report.km_allowed_total as i64);

    let car_name = report.car_name.clone();
    let lease_start = report.lease_start.to_string();
    let lease_end = report.lease_end.to_string();
    let record_error = record_error.unwrap_or_default();
    let record_success = record_success.unwrap_or_default();

    view! { cx =>
        document(
            title: format!("LeaseTrack — {car_name}"),
            script: Some("/assets/dashboard.js"),
            <header>
                <a class="brand" href="/dashboard"><h1>"LeaseTrack — " (&car_name)</h1></a>
                <div class="header-actions">
                    <span class="header-email">(email)</span>
                    <form method="post" action="/logout" class="signout-form">
                        <button type="submit" class="signout">"Sign out"</button>
                    </form>
                </div>
            </header>

            <main>
                <div class="panel">
                    <h2>
                        "Lease Info"
                        <button type="button" class="edit-toggle" id="cfg-edit-btn" onclick="toggleLeaseEdit()">"Edit"</button>
                    </h2>

                    <div id="cfg-view">
                        <div class="info-row"><span>"Car"</span><span>(&car_name)</span></div>
                        <div class="info-row"><span>"Start date"</span><span>(&lease_start)</span></div>
                        <div class="info-row"><span>"Years"</span><span>(report.lease_years)</span></div>
                        <div class="info-row"><span>"End date"</span><span>(&lease_end)</span></div>
                        <div class="info-row"><span>"Allowed / year"</span><span>(report.km_allowed_per_year) " km"</span></div>
                        <div class="info-row"><span>"Start odometer"</span><span>(data.config.start_odometer) " km"</span></div>
                        <div class="info-row"><span>"Allowed total"</span><span>(report.km_allowed_total) " km"</span></div>
                        <div class="info-row"><span>"Total driven"</span><span>(report.total_driven as u32) " km"</span></div>
                        lease_extras(report: &report)
                    </div>

                    <form method="post" action="/web/config" id="cfg-form" style="display:none">
                        <div class="info-row">
                            <span>"Car"</span>
                            <input type="text" name="car_name" value=(&car_name) maxlength="100" required="">
                        </div>
                        <div class="info-row">
                            <span>"Start date"</span>
                            <input type="date" id="cfg-start" name="lease_start" value=(&lease_start) required="" oninput="calcEnd()">
                        </div>
                        <div class="info-row">
                            <span>"Years"</span>
                            <input type="number" id="cfg-years" name="lease_years" value=(report.lease_years) min="1" max="10" required="" oninput="calcEnd()">
                        </div>
                        <div class="info-row">
                            <span>"End date"</span>
                            <span class="computed" id="cfg-end">(&lease_end)</span>
                        </div>
                        <div class="info-row">
                            <span>"Allowed / year"</span>
                            <input type="number" name="allowed_km_per_year" value=(report.km_allowed_per_year) min="1" required="">
                        </div>
                        <div class="info-row">
                            <span>"Start odometer"</span>
                            <input type="number" name="start_odometer" value=(data.config.start_odometer) min="0" required="">
                        </div>
                        <div class="info-row"><span>"Allowed total"</span><span>(report.km_allowed_total) " km"</span></div>
                        <div class="info-row"><span>"Total driven"</span><span>(report.total_driven as u32) " km"</span></div>
                        lease_extras(report: &report)
                        <button type="submit" class="config-save">"Save"</button>
                    </form>
                </div>

                <div class="panel">
                    <h2>"Record Odometer"</h2>
                    if !record_success.is_empty() {
                        <div class="success-box">(&record_success)</div>
                    }
                    if !record_error.is_empty() {
                        <div class="error-box inline">(&record_error)</div>
                    }
                    <form method="post" action="/web/record" class="record-form">
                        <label for="odometer">"Odometer (km)"</label>
                        <input type="number" id="odometer" name="odometer" min="0" placeholder="e.g. 25000" required="">
                        <label for="date">"Date"</label>
                        <input type="date" id="date" name="date" value=(&today) required="">
                        <button type="submit">"Record"</button>
                    </form>
                </div>

                <div class="panel span-full">
                    <h2>"Projections"</h2>
                    if let Some(year_diff) = proj_year_diff {
                        <div class="proj-grid">
                            projection_card(
                                label: "End of current year vs annual limit",
                                diff: year_diff,
                                sub: format!(
                                    "projected {} km / {} km allowed",
                                    proj_year_total.unwrap_or(0),
                                    report.km_allowed_per_year,
                                ),
                            )
                            if let Some(total_diff) = proj_total_diff {
                                projection_card(
                                    label: "End of lease vs total allowed",
                                    diff: total_diff,
                                    sub: format!(
                                        "projected vs {} km total allowed",
                                        report.km_allowed_total,
                                    ),
                                )
                            }
                        </div>
                    } else {
                        <p class="proj-empty">"No projection data yet."</p>
                    }
                </div>

                <div class="panel span-full">
                    <h2>"Km per year"</h2>
                    for year in &years {
                        <div class="bar-row">
                            <div class="bar-label">"Year " (year.year_num)</div>
                            <div class="bar-track">
                                <div
                                    class=(format!("bar-fill {}", year.status))
                                    style=(format!("width:{}%", year.pct))
                                ></div>
                                if let Some(proj) = year.proj_pct.filter(|p| *p > 0) {
                                    <div class="bar-fill proj" style=(format!("width:{proj}%"))></div>
                                }
                            </div>
                            <div class="bar-km">
                                if year.status != "future" {
                                    (year.km) " km"
                                } else {
                                    "—"
                                }
                            </div>
                        </div>
                    }
                </div>

                <div class="panel span-full">
                    <h2>"Odometer records"</h2>
                    <table class="records-table">
                        <thead>
                            <tr><th>"Date"</th><th>"Odometer"</th><th>"Delta"</th></tr>
                        </thead>
                        <tbody>
                            for row in &records {
                                <tr>
                                    <td>(&row.date)</td>
                                    <td>(row.odometer) " km"</td>
                                    <td>
                                        if let Some(delta) = row.delta {
                                            <span class=(if delta >= 0 { "delta pos" } else { "delta neg" })>
                                                if delta >= 0 { "+" }
                                                (delta) " km"
                                            </span>
                                        } else {
                                            "—"
                                        }
                                    </td>
                                </tr>
                            }
                        </tbody>
                    </table>
                </div>
            </main>
        )
    }?
    .into_response(cx)
}

/// The optional last-reading and average-rate rows, shown in both the read-only
/// panel and the edit form.
#[component]
async fn lease_extras(report: &leasetrack_core::ReportData) -> Result {
    view! {
        if let Some(last) = report.last_record.as_ref() {
            <div class="info-row">
                <span>"Last reading"</span>
                <span>(last.odometer) " km (" (last.date.to_string()) ")"</span>
            </div>
        }
        if let Some(rate) = report.avg_daily_rate {
            <div class="info-row">
                <span>"Avg daily rate"</span>
                <span>(rate as u32) " km/day"</span>
            </div>
        }
    }
}

/// A single projection tile, coloured green when under the limit and red when
/// over.
#[component]
async fn projection_card(label: &str, diff: i64, #[into] sub: String) -> Result {
    view! {
        <div class="proj-card">
            <div class="proj-label">(label)</div>
            <div class=(if diff > 0 { "proj-val red" } else { "proj-val green" })>
                if diff > 0 { "+" }
                (diff) " km"
            </div>
            <div class="proj-sub">(sub)</div>
        </div>
    }
}

// Keeps `View` in scope for the component macro's generated signatures.
const _: Option<View> = None;
