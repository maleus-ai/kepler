use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Chart, Dataset, GraphType};

use crate::ui::pages::top::app::{ChartMode, TopApp};
use crate::ui::widgets::utils::{chart_data_points, format_duration_label, format_timestamp_label, nice_ceil_f64};

pub fn render(f: &mut Frame, area: Rect, app: &TopApp, focused: bool) {
    let theme = &app.theme;
    let right_edge_ms = app.current_right_edge_ms();
    let window = app.chart_window;
    let window_secs = window.as_secs_f64();

    let visible = app.visible_services();

    let mut all_datasets: Vec<(String, Vec<(f64, f64)>, ratatui::style::Color)> = Vec::new();
    let mut max_y: f64 = 0.0;

    for svc in &visible {
        if let Some(history) = app.history.get(*svc) {
            let points = chart_data_points(history, window, right_edge_ms, |e| e.cpu_percent as f64);
            for &(_, y) in &points {
                if y > max_y {
                    max_y = y;
                }
            }
            let color = app.service_chart_color(svc);
            all_datasets.push(((*svc).clone(), points, color));
        }
    }

    if app.is_all_view() && !app.services.is_empty() {
        let points = chart_data_points(&app.total_history, window, right_edge_ms, |e| e.cpu_percent as f64);
        for &(_, y) in &points {
            if y > max_y {
                max_y = y;
            }
        }
        all_datasets.push(("Total".to_string(), points, theme.fg_bright));
    }

    // Round up to a nice value with ~4 y-axis labels
    max_y = nice_ceil_f64(max_y * 1.2, 1.0);

    let dataset_refs: Vec<Dataset> = all_datasets
        .iter()
        .map(|(name, points, color)| {
            let mut ds = Dataset::default()
                .name(name.as_str())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(*color))
                .data(points);
            if name == "Total" {
                ds = ds.style(
                    Style::default()
                        .fg(*color)
                        .add_modifier(Modifier::DIM),
                );
            }
            ds
        })
        .collect();

    let x_labels = match app.chart_mode {
        ChartMode::Live => vec![
            format!("-{}", format_duration_label(window_secs as u64)),
            "now".to_string(),
        ],
        ChartMode::Paused { right_edge_ms: edge } => {
            let left_ms = edge - (window_secs * 1000.0) as i64;
            vec![
                format_timestamp_label(left_ms),
                format_timestamp_label(edge),
            ]
        }
    };
    let y_step = max_y / 4.0;
    let y_labels: Vec<String> = (0..=4)
        .map(|i| {
            let v = i as f64 * y_step;
            if v >= 100.0 { format!("{}", v as u64) }
            else if v >= 10.0 { format!("{:.0}", v) }
            else if v >= 1.0 { format!("{:.1}", v) }
            else if v > 0.0 { format!("{:.2}", v) }
            else { "0".to_string() }
        })
        .collect();

    let title = if let Some(svc) = &app.focused_service {
        format!(" CPU Usage (%) \u{2014} {} ", svc)
    } else if !app.toggled_services.is_empty() {
        let count = app.toggled_services.len();
        format!(" CPU Usage (%) \u{2014} {} selected ", count)
    } else {
        " CPU Usage (%) \u{2014} All ".to_string()
    };
    let block = if focused { theme.block_focused(&title) } else { theme.block(&title) };

    let chart = Chart::new(dataset_refs)
        .block(block)
        .style(Style::default().bg(theme.bg_surface))
        .x_axis(
            Axis::default()
                .bounds([0.0, window_secs])
                .labels(x_labels)
                .style(Style::default().fg(theme.fg_dim)),
        )
        .y_axis(
            Axis::default()
                .bounds([0.0, max_y])
                .labels(y_labels)
                .style(Style::default().fg(theme.fg_dim)),
        );

    f.render_widget(chart, area);
}
