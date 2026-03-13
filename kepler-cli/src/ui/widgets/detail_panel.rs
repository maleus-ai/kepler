use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::ui::pages::top::app::{AppAction, TopApp};
use crate::ui::widgets::utils::format_bytes;

pub fn render(f: &mut Frame, area: Rect, app: &mut TopApp) {
    let service_name = app.selected_service_name().map(|s| s.to_string());

    let is_all = service_name.is_none();

    // Clear button areas each frame
    app.detail_button_areas.clear();

    if is_all {
        render_all_summary(f, area, app);
    } else {
        render_service_detail(f, area, app, &service_name.unwrap());
    }
}

fn render_all_summary(f: &mut Frame, area: Rect, app: &mut TopApp) {
    let theme = &app.theme;
    let totals = app.compute_totals();

    let running = app
        .service_status
        .values()
        .filter(|i| i.status == "running" || i.status == "healthy")
        .count();
    let stopped = app
        .service_status
        .values()
        .filter(|i| i.status == "stopped" || i.status == "exited")
        .count();
    let failed = app
        .service_status
        .values()
        .filter(|i| i.status == "failed" || i.status == "killed")
        .count();
    let total_services = app.services.len();

    let has_stopped = stopped > 0 || failed > 0;
    let has_running = running > 0;

    let lines = vec![
        Line::from(""),
        detail_row("Services", &format!("{} total", total_services), theme.fg, theme),
        detail_row(
            "Running",
            &format!("{}", running),
            theme.success,
            theme,
        ),
        detail_row(
            "Stopped",
            &format!("{}", stopped),
            theme.fg_dim,
            theme,
        ),
        detail_row(
            "Failed",
            &format!("{}", failed),
            if failed > 0 { theme.error } else { theme.fg_dim },
            theme,
        ),
        Line::from(""),
        detail_row("Total CPU", &format!("{:.2}%", totals.cpu_percent), theme.fg, theme),
        detail_row("Total Memory", &format!("{} (RSS)", format_bytes(totals.memory_rss)), theme.fg, theme),
        detail_row("Processes", &format!("{}", totals.pids.len()), theme.fg, theme),
    ];

    let info_height = lines.len() as u16;
    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(theme.bg_surface));
    f.render_widget(paragraph, area);

    // Action buttons — respect multi-select
    let targets = app.action_target_services();
    let has_toggles = !app.toggled_services.is_empty();
    let count = targets.as_ref().map(|t| t.len()).unwrap_or(0);

    // Recompute enabled state based on actual targets when multi-selecting
    let (has_stopped, has_running) = if has_toggles {
        let stopped = targets.as_ref().map(|svcs| {
            svcs.iter().any(|s| {
                app.service_status.get(s).map_or(false, |i| {
                    matches!(i.status.as_str(), "stopped" | "exited" | "failed" | "killed")
                })
            })
        }).unwrap_or(false);
        let running = targets.as_ref().map(|svcs| {
            svcs.iter().any(|s| {
                app.service_status.get(s).map_or(false, |i| {
                    matches!(i.status.as_str(), "running" | "healthy" | "starting" | "waiting")
                })
            })
        }).unwrap_or(false);
        (stopped, running)
    } else {
        (has_stopped, has_running)
    };

    let (start_label, stop_label, restart_label) = if has_toggles {
        (
            format!("Start {}", count),
            format!("Stop {}", count),
            format!("Restart {}", count),
        )
    } else {
        ("Start All".to_string(), "Stop All".to_string(), "Restart All".to_string())
    };

    let start_action = targets.clone().map(|t| AppAction::StartServices(t));
    let stop_action = targets.clone().map(|t| {
        if t.is_empty() {
            AppAction::StopAll
        } else if t.len() == 1 {
            AppAction::StopService(t[0].clone())
        } else {
            AppAction::StopAll
        }
    });
    let restart_action = targets.map(|t| AppAction::RestartServices(t));

    let button_y = area.y + info_height + 1;
    if button_y + 3 > area.y + area.height {
        return;
    }

    let mut button_x = area.x + 2;
    let gap_bg = app.theme.bg_surface;

    button_x = render_button(
        f, button_x, button_y,
        "s", &start_label,
        has_stopped, app.theme.success, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_stopped { start_action } else { None },
        &mut app.detail_button_areas,
    );

    button_x += 1;

    button_x = render_button(
        f, button_x, button_y,
        "S", &stop_label,
        has_running, app.theme.error, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_running { stop_action } else { None },
        &mut app.detail_button_areas,
    );

    button_x += 1;

    render_button(
        f, button_x, button_y,
        "r", &restart_label,
        has_running, app.theme.warning, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_running { restart_action } else { None },
        &mut app.detail_button_areas,
    );
}

fn render_service_detail(f: &mut Frame, area: Rect, app: &mut TopApp, service: &str) {
    let theme = &app.theme;

    let info = app.service_status.get(service);
    let metrics = app.latest.get(service);

    let status_str = info
        .map(|i| i.status.clone())
        .unwrap_or_else(|| "-".to_string());
    let status_color = theme.status_color(&status_str);
    let status_icon = theme.status_icon(&status_str);

    let pid_str = info
        .and_then(|i| i.pid)
        .map(|p| format!("{}", p))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let uptime_str = info
        .and_then(|i| i.started_at)
        .map(|started_at| {
            let now = chrono::Utc::now().timestamp();
            let elapsed_secs = (now - started_at).max(0) as u64;
            format_uptime(elapsed_secs)
        })
        .unwrap_or_else(|| "\u{2014}".to_string());

    let cpu_str = metrics
        .map(|m| format!("{:.2}%", m.cpu_percent))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let mem_str = metrics
        .map(|m| {
            format!(
                "{} (RSS) / {} (VSS)",
                format_bytes(m.memory_rss),
                format_bytes(m.memory_vss)
            )
        })
        .unwrap_or_else(|| "\u{2014}".to_string());

    let procs_str = metrics
        .map(|m| format!("{}", m.pids.len()))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let exit_code_str = info
        .and_then(|i| i.exit_code)
        .map(|c| format!("{}", c))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let signal_str = info
        .and_then(|i| i.signal)
        .map(|s| format!("{}", s))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let health_str = info
        .map(|i| format!("{} failures", i.health_check_failures))
        .unwrap_or_else(|| "\u{2014}".to_string());

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  Status      ", Style::default().fg(theme.fg_dim)),
            Span::styled(
                format!("{} {}", status_icon, status_str),
                Style::default().fg(status_color),
            ),
        ]),
        detail_row("PID", &pid_str, theme.fg, theme),
        detail_row("Uptime", &uptime_str, theme.fg, theme),
        detail_row("CPU", &cpu_str, theme.fg, theme),
        detail_row("Memory", &mem_str, theme.fg, theme),
        detail_row("Processes", &procs_str, theme.fg, theme),
        detail_row("Exit Code", &exit_code_str, theme.fg, theme),
        detail_row("Signal", &signal_str, theme.fg, theme),
        detail_row("Health", &health_str, theme.fg, theme),
    ];

    let info_height = lines.len() as u16;
    let paragraph = Paragraph::new(lines)
        .style(Style::default().bg(theme.bg_surface));
    f.render_widget(paragraph, area);

    // Action buttons — use actual action targets (respects multi-select)
    let targets = app.action_target_services();
    let multi = app.toggled_services.len() > 1
        || (!app.toggled_services.is_empty() && !app.toggled_services.contains(service));
    let count = targets.as_ref().map(|t| t.len()).unwrap_or(0);

    // Enable/disable based on actual targets' statuses
    let (has_stopped, has_running) = if multi {
        let stopped = targets.as_ref().map(|svcs| {
            svcs.iter().any(|s| {
                app.service_status.get(s).map_or(false, |i| {
                    matches!(i.status.as_str(), "stopped" | "exited" | "failed" | "killed")
                })
            })
        }).unwrap_or(false);
        let running = targets.as_ref().map(|svcs| {
            svcs.iter().any(|s| {
                app.service_status.get(s).map_or(false, |i| {
                    matches!(i.status.as_str(), "running" | "healthy" | "starting" | "waiting")
                })
            })
        }).unwrap_or(false);
        (stopped, running)
    } else {
        let is_running = matches!(status_str.as_str(), "running" | "healthy" | "starting" | "waiting");
        let is_stopped = matches!(status_str.as_str(), "stopped" | "exited" | "failed" | "killed");
        (is_stopped, is_running)
    };

    // Labels reflect multi-select
    let (start_label, stop_label, restart_label) = if multi {
        (
            format!("Start {}", count),
            format!("Stop {}", count),
            format!("Restart {}", count),
        )
    } else {
        ("Start".to_string(), "Stop".to_string(), "Restart".to_string())
    };

    // Actions use actual targets
    let start_action = targets.clone().map(|t| AppAction::StartServices(t));
    let stop_action = targets.clone().map(|t| {
        if t.len() == 1 {
            AppAction::StopService(t[0].clone())
        } else {
            AppAction::StopAll
        }
    });
    let restart_action = targets.map(|t| AppAction::RestartServices(t));

    let button_y = area.y + info_height + 1;
    if button_y + 3 > area.y + area.height {
        return;
    }

    let mut button_x = area.x + 2;
    let gap_bg = app.theme.bg_surface;

    button_x = render_button(
        f, button_x, button_y,
        "s", &start_label,
        has_stopped, app.theme.success, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_stopped { start_action } else { None },
        &mut app.detail_button_areas,
    );

    button_x += 1;

    button_x = render_button(
        f, button_x, button_y,
        "S", &stop_label,
        has_running, app.theme.error, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_running { stop_action } else { None },
        &mut app.detail_button_areas,
    );

    button_x += 1;

    render_button(
        f, button_x, button_y,
        "r", &restart_label,
        has_running, app.theme.warning, app.theme.fg_dim, app.theme.bg_surface, gap_bg,
        if has_running { restart_action } else { None },
        &mut app.detail_button_areas,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_button(
    f: &mut Frame,
    x: u16,
    y: u16,
    shortcut: &str,
    label: &str,
    enabled: bool,
    active_color: ratatui::style::Color,
    dim_color: ratatui::style::Color,
    panel_bg: ratatui::style::Color,
    gap_bg: ratatui::style::Color,
    action: Option<AppAction>,
    button_areas: &mut Vec<(AppAction, Rect)>,
) -> u16 {
    // " [s] Start " — shortcut in brackets + label with padding
    let content = format!("[{}] {}", shortcut, label);
    let width = content.len() as u16 + 4; // +2 padding +2 borders
    let rect = Rect::new(x, y, width, 3);

    let (border_fg, text_fg) = if enabled {
        (active_color, active_color)
    } else {
        (panel_bg, dim_color)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_fg).bg(gap_bg))
        .style(Style::default().bg(panel_bg));

    let text_style = if enabled {
        Style::default().fg(text_fg).bg(panel_bg).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(text_fg).bg(panel_bg)
    };

    let paragraph = Paragraph::new(Line::from(content))
        .style(text_style)
        .alignment(Alignment::Center)
        .block(block);

    f.render_widget(paragraph, rect);

    if let Some(action) = action {
        button_areas.push((action, rect));
    }

    x + width
}

fn detail_row<'a>(
    label: &'a str,
    value: &str,
    value_color: ratatui::style::Color,
    theme: &crate::ui::theme::Theme,
) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {:<12} ", label),
            Style::default().fg(theme.fg_dim),
        ),
        Span::styled(value.to_string(), Style::default().fg(value_color)),
    ])
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{}m {}s", m, s)
    } else if secs < 86400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        let s = secs % 60;
        format!("{}h {}m {}s", h, m, s)
    } else {
        let d = secs / 86400;
        let h = (secs % 86400) / 3600;
        let m = (secs % 3600) / 60;
        format!("{}d {}h {}m", d, h, m)
    }
}
