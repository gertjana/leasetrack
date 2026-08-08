use crate::app::{App, AppMode};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Bar, BarChart, BarGroup, Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Root: main area + 1-row status bar at bottom
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(size);

    let main_area = root[0];
    let status_area = root[1];

    // Main: left 40% | right 60%
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(main_area);

    // Left column: car info (fixed 11 rows) | graph (rest, min 6) | projections (fixed 5)
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11), // car info
            Constraint::Min(6),     // graph
            Constraint::Length(5),  // projections
        ])
        .split(columns[0]);

    draw_car_info(f, app, left_rows[0]);
    draw_graph(f, app, left_rows[1]);
    draw_projections(f, app, left_rows[2]);
    draw_records(f, app, columns[1]);
    draw_status_bar(f, app, status_area);

    // Popups rendered on top
    if app.mode == AppMode::RecordPopup {
        draw_record_popup(f, app);
    }
}

fn draw_car_info(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Car Info ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if let Some(report) = &app.report {
        let lines = vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    report.car_name.clone(),
                    Style::default()
                        .fg(Color::Reset)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Lease:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} -> {}", report.lease_start, report.lease_end),
                    Style::default().fg(Color::Reset),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Years:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}", report.lease_years),
                    Style::default().fg(Color::Reset),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Allowed: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} km/yr", fmt_km(report.km_allowed_per_year)),
                    Style::default().fg(Color::Reset),
                ),
            ]),
            Line::from(vec![
                Span::styled("  Total:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} km allowed", fmt_km(report.km_allowed_total)),
                    Style::default().fg(Color::Reset),
                ),
            ]),
            Line::from(""),
            if let Some(last) = &report.last_record {
                Line::from(vec![
                    Span::styled("  Last:    ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{} km ({})", fmt_km(last.odometer), last.date),
                        Style::default().fg(Color::Yellow),
                    ),
                ])
            } else {
                Line::from(Span::styled(
                    "  No records yet",
                    Style::default().fg(Color::DarkGray),
                ))
            },
        ];

        f.render_widget(Paragraph::new(lines).block(block), area);
    } else if app.is_loading {
        f.render_widget(
            Paragraph::new("  Loading...")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    } else {
        let msg = app.error_msg.as_deref().unwrap_or("No data — check API key");
        f.render_widget(
            Paragraph::new(format!("  {}", msg))
                .block(block)
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true }),
            area,
        );
    }
}

fn draw_graph(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Graph ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if let Some(report) = &app.report {
        let inner = block.inner(area);
        f.render_widget(block, area);

        let allowed = report.km_allowed_per_year as u64;
        let yr_width = report.years.len().to_string().len(); // digits in max year number

        // Pre-compute km strings to find max width for right-alignment
        let km_strs: Vec<String> = report.years.iter()
            .map(|y| fmt_km_f(y.km_driven.unwrap_or(0.0)))
            .collect();
        let km_width = km_strs.iter().map(|s| s.len()).max().unwrap_or(6);

        let bars: Vec<Bar> = report
            .years
            .iter()
            .zip(km_strs.iter())
            .map(|(y, km_str)| {
                let km = y.km_driven.unwrap_or(0.0) as u64;
                let color = if y.is_current {
                    Color::Yellow
                } else if y.is_future {
                    Color::DarkGray
                } else if km > allowed {
                    Color::Red
                } else {
                    Color::Green
                };
                // Label: "Yr1  12,779 " — year, km right-aligned, trailing space gap
                let label = format!(
                    "Yr{yr:<yr_w$}  {km:>km_w$} ",
                    yr = y.year_num,
                    yr_w = yr_width,
                    km = km_str,
                    km_w = km_width,
                );
                Bar::default()
                    .label(Line::from(label))
                    .value(km)
                    .text_value(String::new())
                    .style(Style::default().fg(color))
                    .value_style(Style::default().fg(Color::Reset))
            })
            .collect();

        let bar_chart = BarChart::default()
            .data(BarGroup::default().bars(&bars))
            .bar_width(1)
            .bar_gap(0)
            .max(allowed + allowed / 4)
            .direction(Direction::Horizontal);

        f.render_widget(bar_chart, inner);
    } else if app.is_loading {
        f.render_widget(Paragraph::new("  Loading...").block(block), area);
    } else {
        f.render_widget(Paragraph::new("  No data").block(block), area);
    }
}

fn draw_projections(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Projections ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = vec![];

    if let Some(report) = &app.report {
        // Current year projection
        if let Some(proj) = &report.current_year {
            let diff_color = if proj.projected_diff > 0.0 { Color::Red } else { Color::Green };
            let sign = if proj.projected_diff > 0.0 { "+" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(" End yr ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}", proj.year_num), Style::default().fg(Color::Yellow)),
                Span::styled(":  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} km", fmt_km_f(proj.projected_year_total)),
                    Style::default().fg(Color::Reset),
                ),
                Span::styled("  (", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}{} vs limit", sign, fmt_km_f(proj.projected_diff)),
                    Style::default().fg(diff_color),
                ),
                Span::styled(")", Style::default().fg(Color::DarkGray)),
            ]));
        }

        // End of contract projection
        if let Some(proj_total) = report.projected_total {
            let over = proj_total - report.km_allowed_total as f64;
            let color = if over > 0.0 { Color::Red } else { Color::Green };
            let sign = if over > 0.0 { "+" } else { "" };
            lines.push(Line::from(vec![
                Span::styled(" End lease: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{} km", fmt_km_f(proj_total)),
                    Style::default().fg(Color::Reset),
                ),
                Span::styled("  (", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("{}{} vs total", sign, fmt_km_f(over)),
                    Style::default().fg(color),
                ),
                Span::styled(")", Style::default().fg(Color::DarkGray)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_records(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Records ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if let Some(list) = &app.list {
        let mut records = list.records.clone();
        records.sort_by(|a, b| b.date.cmp(&a.date)); // newest first

        let items: Vec<ListItem> = records
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let delta = if i + 1 < records.len() {
                    let prev = records[i + 1].odometer;
                    format!("+{}", fmt_km(r.odometer.saturating_sub(prev)))
                } else {
                    "-".to_string()
                };

                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("  {}  ", r.date),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{:>9}  ", fmt_km(r.odometer)),
                        Style::default().fg(Color::Reset),
                    ),
                    Span::styled(
                        format!("{:>8}", delta),
                        Style::default().fg(Color::Green),
                    ),
                ]))
            })
            .collect();

        let inner = block.inner(area);
        f.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(inner);

        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Date          Odometer      Delta",
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ))),
            rows[0],
        );

        let visible_height = rows[1].height as usize;
        let scroll = app.records_scroll.min(items.len().saturating_sub(1));
        let end = (scroll + visible_height).min(items.len());
        let visible_items: Vec<ListItem> = items[scroll..end].to_vec();

        f.render_widget(List::new(visible_items), rows[1]);

        // Scroll indicator in title area
        if items.len() > visible_height {
            let hint = format!(" ({}/{}) ", scroll + 1, items.len());
            let hint_area = Rect {
                x: area.x + area.width.saturating_sub(hint.len() as u16 + 1),
                y: area.y,
                width: hint.len() as u16,
                height: 1,
            };
            f.render_widget(
                Paragraph::new(Span::styled(hint, Style::default().fg(Color::DarkGray))),
                hint_area,
            );
        }
    } else if app.is_loading {
        f.render_widget(Paragraph::new("  Loading...").block(block), area);
    } else {
        f.render_widget(Paragraph::new("  No data").block(block), area);
    }
}

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let (status_text, status_style) = if let Some(err) = &app.error_msg {
        (
            format!("ERROR: {}", err),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )
    } else if app.is_loading {
        ("Loading...".to_string(), Style::default().fg(Color::Yellow))
    } else if let Some(msg) = &app.status_msg {
        (msg.clone(), Style::default().fg(Color::Green))
    } else {
        ("".to_string(), Style::default())
    };

    let hints = Line::from(vec![
        Span::styled(" leasetrack-tui  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[r]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::styled(" record  ", Style::default().fg(Color::DarkGray)),
        Span::styled("[q]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::styled(" quit", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if status_text.is_empty() { "".to_string() } else { format!("   {}", status_text) },
            status_style,
        ),
    ]);

    f.render_widget(Paragraph::new(hints), area);
}

fn draw_record_popup(f: &mut Frame, app: &App) {
    let area = fixed_rect(44, 10, f.area());
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Record Odometer ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // spacer
            Constraint::Length(1), // odometer
            Constraint::Length(1), // spacer
            Constraint::Length(1), // date
            Constraint::Length(1), // spacer
            Constraint::Min(0),    // hints / error
        ])
        .split(inner);

    let input = &app.record_input;

    // Odometer field
    let odo_focused = input.focused_field == 0;
    let odo_style = if odo_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Reset)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Odometer: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{}{}]", input.odometer, if odo_focused { "█" } else { " " }),
                odo_style,
            ),
        ])),
        rows[1],
    );

    // Date field
    let date_focused = input.focused_field == 1;
    let date_style = if date_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Reset)
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Date:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("[{}{}]", input.date, if date_focused { "█" } else { " " }),
                date_style,
            ),
        ])),
        rows[3],
    );

    // Hints or error
    if let Some(err) = &app.error_msg {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  Error: {}", err),
                Style::default().fg(Color::Red),
            )))
            .wrap(Wrap { trim: true }),
            rows[5],
        );
    } else {
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "  Tab: next field",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    "  Enter: save   Esc: cancel",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            rows[5],
        );
    }
}

/// Returns a Rect of exact width/height centered in r
fn fixed_rect(width: u16, height: u16, r: Rect) -> Rect {
    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(r.width),
        height: height.min(r.height),
    }
}

fn fmt_km(n: u32) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

fn fmt_km_f(n: f64) -> String {
    fmt_km(n.abs().round() as u32)
}
