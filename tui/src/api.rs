#![allow(dead_code)]
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct KmRecord {
    pub date: NaiveDate,
    pub odometer: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseConfig {
    pub car_name: String,
    pub lease_start: NaiveDate,
    pub lease_years: u32,
    pub allowed_km_per_year: u32,
    pub start_odometer: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseData {
    pub config: LeaseConfig,
    pub records: Vec<KmRecord>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct YearStats {
    pub year_num: u32,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub km_driven: Option<f64>,
    pub is_current: bool,
    pub is_future: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CurrentYearProjection {
    pub year_num: u32,
    pub days_elapsed: u32,
    pub days_total: u32,
    pub km_driven: f64,
    pub avg_daily_rate: f64,
    pub projected_year_total: f64,
    pub projected_diff: f64,
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct GraphData {
    pub car_name: String,
    pub allowed_km_per_year: u32,
    pub years: Vec<YearStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordRequest {
    pub odometer: u32,
    pub date: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ApiClient {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl ApiClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            base_url,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let base_url = std::env::var("LEASETRACK_API_URL")
            .unwrap_or_else(|_| "https://leasetrack.apps.gertjanassies.dev".to_string());
        let api_key = std::env::var("LEASETRACK_API_KEY")
            .map_err(|_| "LEASETRACK_API_KEY environment variable is not set".to_string())?;
        Ok(Self::new(base_url, api_key))
    }

    pub async fn get_report(&self) -> Result<ReportData, String> {
        self.client
            .get(format!("{}/report", self.base_url))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<ReportData>()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_list(&self) -> Result<LeaseData, String> {
        self.client
            .get(format!("{}/list", self.base_url))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<LeaseData>()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn get_graph(&self) -> Result<GraphData, String> {
        self.client
            .get(format!("{}/graph", self.base_url))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<GraphData>()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn post_record(&self, odometer: u32, date: String) -> Result<(), String> {
        let req = RecordRequest {
            odometer,
            date: Some(date),
        };
        let resp = self
            .client
            .post(format!("{}/record", self.base_url))
            .header("X-Api-Key", &self.api_key)
            .json(&req)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(body)
        }
    }
}
