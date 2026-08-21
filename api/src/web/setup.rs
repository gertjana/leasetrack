//! First-time lease configuration.

use leasetrack_core::{LeaseConfig, LeaseData, load_user_data, save_user_data};
use serde::Deserialize;
use topcoat::{
    Result,
    context::Cx,
    router::{
        content::Form,
        error::see_other,
        route,
        response::{IntoResponse, Response},
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

    let start_odometer: u32 = form.start_odometer.trim().parse().unwrap_or(0);

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
