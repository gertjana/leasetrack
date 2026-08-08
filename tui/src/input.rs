use crate::app::{App, AppMode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub async fn handle_key(app: &mut App, key: KeyEvent) {
    match app.mode {
        AppMode::Normal => handle_normal(app, key).await,
        AppMode::RecordPopup => handle_record_popup(app, key).await,
    }
}

async fn handle_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            app.status_msg = Some("__QUIT__".to_string());
        }
        KeyCode::Char('r') => {
            app.open_record_popup();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.scroll_up();
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.scroll_down();
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.status_msg = Some("__QUIT__".to_string());
        }
        _ => {}
    }
}

async fn handle_record_popup(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.close_popup();
        }
        KeyCode::Tab => {
            app.record_input.focused_field = (app.record_input.focused_field + 1) % 2;
        }
        KeyCode::BackTab => {
            app.record_input.focused_field = if app.record_input.focused_field == 0 { 1 } else { 0 };
        }
        KeyCode::Enter => {
            app.submit_record().await;
        }
        KeyCode::Backspace => {
            if app.record_input.focused_field == 0 {
                app.record_input.odometer.pop();
            } else {
                app.record_input.date.pop();
            }
            app.error_msg = None;
        }
        KeyCode::Char(c) => {
            if app.record_input.focused_field == 0 {
                if c.is_ascii_digit() {
                    app.record_input.odometer.push(c);
                }
            } else if c.is_ascii_digit() || c == '-' {
                app.record_input.date.push(c);
            }
            app.error_msg = None;
        }
        _ => {}
    }
}
