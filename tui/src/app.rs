use crate::api::{ApiClient, LeaseData, ReportData};
use chrono::Local;

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Normal,
    RecordPopup,
}

#[derive(Debug, Clone)]
pub struct RecordInput {
    pub odometer: String,
    pub date: String,
    pub focused_field: u8, // 0 = odometer, 1 = date
}

impl RecordInput {
    pub fn new() -> Self {
        Self {
            odometer: String::new(),
            date: Local::now().format("%Y-%m-%d").to_string(),
            focused_field: 0,
        }
    }
}

#[derive(Debug)]
pub struct App {
    pub mode: AppMode,
    pub report: Option<ReportData>,
    pub list: Option<LeaseData>,
    pub records_scroll: usize,
    pub record_input: RecordInput,
    pub status_msg: Option<String>,
    pub error_msg: Option<String>,
    pub is_loading: bool,
    pub api: ApiClient,
}

impl App {
    pub fn new(api: ApiClient) -> Self {
        Self {
            mode: AppMode::Normal,
            report: None,
            list: None,
            records_scroll: 0,
            record_input: RecordInput::new(),
            status_msg: None,
            error_msg: None,
            is_loading: false,
            api,
        }
    }

    pub async fn refresh(&mut self) {
        self.is_loading = true;
        self.error_msg = None;

        let report = self.api.get_report().await;
        let list = self.api.get_list().await;

        match report {
            Ok(r) => self.report = Some(r),
            Err(e) => self.error_msg = Some(format!("Report: {}", e)),
        }
        match list {
            Ok(l) => self.list = Some(l),
            Err(e) => {
                if self.error_msg.is_none() {
                    self.error_msg = Some(format!("List: {}", e));
                }
            }
        }

        self.is_loading = false;
    }

    pub async fn submit_record(&mut self) {
        let odometer_str = self.record_input.odometer.trim().to_string();
        let date_str = self.record_input.date.trim().to_string();

        let odometer: u32 = match odometer_str.parse() {
            Ok(v) => v,
            Err(_) => {
                self.error_msg = Some("Invalid odometer value".to_string());
                return;
            }
        };

        self.is_loading = true;
        match self.api.post_record(odometer, date_str).await {
            Ok(_) => {
                self.status_msg = Some(format!("Recorded {} km", odometer));
                self.mode = AppMode::Normal;
                self.record_input = RecordInput::new();
                self.refresh().await;
            }
            Err(e) => {
                self.error_msg = Some(e);
            }
        }
        self.is_loading = false;
    }

    pub fn open_record_popup(&mut self) {
        self.record_input = RecordInput::new();
        self.error_msg = None;
        self.mode = AppMode::RecordPopup;
    }

    pub fn close_popup(&mut self) {
        self.mode = AppMode::Normal;
        self.error_msg = None;
    }

    pub fn scroll_up(&mut self) {
        if self.records_scroll > 0 {
            self.records_scroll -= 1;
        }
    }

    pub fn scroll_down(&mut self) {
        if let Some(list) = &self.list {
            let max = list.records.len().saturating_sub(1);
            if self.records_scroll < max {
                self.records_scroll += 1;
            }
        }
    }
}
