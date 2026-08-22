//! First-time lease configuration.

use leasetrack_core::{LeaseConfig, LeaseData, load_user_data, save_user_data};
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
    view::view,
};

use super::{current_email, layout::document};

/// The lease fields, submitted by both the setup page and the dashboard's
/// inline edit form.
#[derive(Deserialize)]
pub struct ConfigForm {
    pub car_name: String,
    pub lease_start: String,
    pub lease_years: String,
    pub allowed_km_per_year: String,
    pub start_odometer: String,
}

/// Validate a submitted lease configuration, returning the message to show the
/// user on the first problem found.
pub fn parse_config(form: &ConfigForm) -> std::result::Result<LeaseConfig, String> {
    let lease_start = chrono::NaiveDate::parse_from_str(form.lease_start.trim(), "%Y-%m-%d")
        .map_err(|_| "Invalid lease start date — use YYYY-MM-DD.".to_string())?;

    let lease_years: u32 = match form.lease_years.trim().parse() {
        Ok(n) if (1..=10).contains(&n) => n,
        _ => return Err("Lease years must be between 1 and 10.".to_string()),
    };

    let allowed_km_per_year: u32 = match form.allowed_km_per_year.trim().parse() {
        Ok(n) if n > 0 => n,
        _ => return Err("Allowed km/year must be greater than 0.".to_string()),
    };

    // Parsed strictly rather than defaulting: silently falling back to 0 would
    // overwrite a mistyped reading and skew every "total driven" figure derived
    // from it.
    let start_odometer: u32 = match form.start_odometer.trim().parse() {
        Ok(n) => n,
        Err(_) => return Err("Start odometer must be a whole number of km.".to_string()),
    };

    let car_name = form.car_name.trim().to_string();
    if car_name.is_empty() || car_name.len() > 100 {
        return Err("Car name must be between 1 and 100 characters.".to_string());
    }

    Ok(LeaseConfig {
        car_name,
        lease_start,
        lease_years,
        allowed_km_per_year,
        start_odometer,
    })
}

fn today() -> String {
    chrono::Local::now().date_naive().to_string()
}

async fn setup_view(cx: &Cx, error: &str) -> Result<Response> {
    let today = today();
    view! { cx =>
        document(
            title: "LeaseTrack — Setup",
            body_class: "centered",
            script: Some("/assets/setup.js"),
            <div class="card card-wider">
                <h1>"LeaseTrack"</h1>
                <p class="subtitle">"Let's set up your lease"</p>
                if !error.is_empty() {
                    <div class="error">(error)</div>
                }
                <form method="post" action="/setup" class="setup-form">
                    <label for="car_name">"Car name"</label>
                    <input type="text" id="car_name" name="car_name" placeholder="e.g. Tesla Model 3" maxlength="100" required="" autofocus="">

                    <label for="lease_start">"Lease start date"</label>
                    <input type="date" id="lease_start" name="lease_start" value=(&today) required="" oninput="calcEnd()">

                    <label for="lease_years">"Lease duration (years)"</label>
                    <input type="number" id="lease_years" name="lease_years" value="3" min="1" max="10" required="" oninput="calcEnd()">
                    <p class="hint">"End date: " <span id="end-date">"—"</span></p>

                    <label for="allowed_km_per_year">"Allowed km per year"</label>
                    <input type="number" id="allowed_km_per_year" name="allowed_km_per_year" value="20000" min="1" required="">

                    <label for="start_odometer">"Start odometer (km)"</label>
                    <input type="number" id="start_odometer" name="start_odometer" value="0" min="0" required="">

                    <button type="submit">"Start tracking"</button>
                </form>
            </div>
        )
    }?
    .into_response(cx)
}

/// `GET /setup` — initial lease configuration for new users.
#[route(GET "/setup")]
async fn setup_page(cx: &Cx) -> Result<Response> {
    let Some(email) = current_email(cx).await? else {
        return see_other("/login").into_response(cx);
    };
    // If already set up, go straight to the dashboard.
    if load_user_data(&email).is_ok() {
        return see_other("/dashboard").into_response(cx);
    }
    setup_view(cx, "").await
}

/// `POST /setup`
#[route(POST "/setup")]
async fn setup_post(cx: &Cx, Form(form): Form<ConfigForm>) -> Result<Response> {
    let Some(email) = current_email(cx).await? else {
        return see_other("/login").into_response(cx);
    };

    let config = match parse_config(&form) {
        Ok(config) => config,
        Err(message) => return setup_view(cx, &message).await,
    };

    let data = LeaseData {
        config,
        records: vec![],
    };

    if let Err(e) = save_user_data(&email, &data) {
        return setup_view(cx, &e).await;
    }

    see_other("/dashboard").into_response(cx)
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{ConfigForm, parse_config};

    /// A form that passes validation; individual tests override one field.
    fn form() -> ConfigForm {
        ConfigForm {
            car_name: "Tesla Model 3".to_string(),
            lease_start: "2025-01-01".to_string(),
            lease_years: "3".to_string(),
            allowed_km_per_year: "20000".to_string(),
            start_odometer: "100".to_string(),
        }
    }

    #[test]
    fn a_valid_form_is_accepted() {
        let config = parse_config(&form()).expect("accepted");

        assert_eq!(config.car_name, "Tesla Model 3");
        assert_eq!(config.lease_years, 3);
        assert_eq!(config.allowed_km_per_year, 20_000);
        assert_eq!(config.start_odometer, 100);
        assert_eq!(config.lease_start.to_string(), "2025-01-01");
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let mut f = form();
        f.car_name = "  Tesla Model 3  ".to_string();
        f.lease_start = " 2025-01-01 ".to_string();
        f.lease_years = " 3 ".to_string();
        f.allowed_km_per_year = " 20000 ".to_string();
        f.start_odometer = " 100 ".to_string();

        let config = parse_config(&f).expect("accepted");
        assert_eq!(config.car_name, "Tesla Model 3");
        assert_eq!(config.lease_years, 3);
        assert_eq!(config.start_odometer, 100);
    }

    // ─── Dates ────────────────────────────────────────────────────────────────

    #[test]
    fn the_start_date_must_be_iso_formatted() {
        for bad in ["01-01-2025", "2025/01/01", "tomorrow", "", "2025-13-01", "2025-02-30"] {
            let mut f = form();
            f.lease_start = bad.to_string();

            let error = parse_config(&f).expect_err("rejected");
            assert!(error.contains("lease start date"), "{bad} gave: {error}");
        }
    }

    #[test]
    fn a_leap_day_start_is_accepted() {
        let mut f = form();
        f.lease_start = "2024-02-29".to_string();

        assert!(parse_config(&f).is_ok());
    }

    // ─── Duration ─────────────────────────────────────────────────────────────

    #[test]
    fn the_lease_runs_between_one_and_ten_years() {
        for good in ["1", "5", "10"] {
            let mut f = form();
            f.lease_years = good.to_string();
            assert!(parse_config(&f).is_ok(), "{good} years should be allowed");
        }

        for bad in ["0", "11", "-1", "abc", "", "3.5"] {
            let mut f = form();
            f.lease_years = bad.to_string();

            let error = parse_config(&f).expect_err("rejected");
            assert!(error.contains("between 1 and 10"), "{bad} gave: {error}");
        }
    }

    // ─── Allowance ────────────────────────────────────────────────────────────

    #[test]
    fn the_annual_allowance_must_be_positive() {
        for bad in ["0", "-100", "abc", "", "20,000"] {
            let mut f = form();
            f.allowed_km_per_year = bad.to_string();

            let error = parse_config(&f).expect_err("rejected");
            assert!(error.contains("greater than 0"), "{bad} gave: {error}");
        }
    }

    // ─── Start odometer ───────────────────────────────────────────────────────

    #[test]
    fn a_start_odometer_of_zero_is_valid() {
        let mut f = form();
        f.start_odometer = "0".to_string();

        assert_eq!(parse_config(&f).expect("accepted").start_odometer, 0);
    }

    /// This previously fell back to 0 on any parse error, silently discarding a
    /// mistyped reading and skewing every total derived from it.
    #[test]
    fn a_non_numeric_start_odometer_is_rejected_rather_than_defaulted() {
        for bad in ["abc", "", "-5", "12.5", "1 000"] {
            let mut f = form();
            f.start_odometer = bad.to_string();

            let error = parse_config(&f).expect_err("rejected");
            assert!(error.contains("Start odometer"), "{bad} gave: {error}");
        }
    }

    // ─── Car name ─────────────────────────────────────────────────────────────

    #[test]
    fn the_car_name_must_not_be_empty() {
        for bad in ["", "   "] {
            let mut f = form();
            f.car_name = bad.to_string();

            let error = parse_config(&f).expect_err("rejected");
            assert!(error.contains("Car name"), "got: {error}");
        }
    }

    #[test]
    fn the_car_name_is_capped_at_100_characters() {
        let mut f = form();
        f.car_name = "x".repeat(100);
        assert!(parse_config(&f).is_ok(), "100 characters is the limit, inclusive");

        f.car_name = "x".repeat(101);
        let error = parse_config(&f).expect_err("rejected");
        assert!(error.contains("Car name"), "got: {error}");
    }

    // ─── Ordering ─────────────────────────────────────────────────────────────

    /// Fields are checked in a fixed order, so a form with several problems
    /// reports the first one rather than a confusing mixture.
    #[test]
    fn the_first_problem_is_reported() {
        let mut f = form();
        f.lease_start = "nonsense".to_string();
        f.lease_years = "99".to_string();
        f.car_name = String::new();

        let error = parse_config(&f).expect_err("rejected");
        assert!(error.contains("lease start date"), "got: {error}");
    }
}
