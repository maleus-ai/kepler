use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

use crate::ui::Overlay;
use crate::ui::Tab;
use crate::ui::widgets;

use super::app::{LogFocus, TopApp};

pub fn render(f: &mut Frame, app: &mut TopApp) {
    // Fill entire screen with dark base (crust) — visible as gaps between panels
    let outer_area = f.area();
    let bg_block = Block::default().style(Style::default().bg(app.theme.bg));
    f.render_widget(bg_block, outer_area);

    // Outer frame — uses mantle bg, one step lighter than crust
    let title_line = Line::from(vec![
        Span::styled(
            " \u{25c6} Kepler ",
            Style::default().fg(app.theme.accent),
        ),
    ]);

    let border_color = if app.disconnected {
        app.theme.error
    } else {
        app.theme.bg_mantle // frame border blends with mantle bg
    };

    let outer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(title_line)
        .title_alignment(Alignment::Left)
        .style(Style::default().bg(app.theme.bg_mantle));

    f.render_widget(outer_block, outer_area);

    // Inner area (inside outer border)
    let inner = Rect::new(
        outer_area.x + 1,
        outer_area.y + 1,
        outer_area.width.saturating_sub(2),
        outer_area.height.saturating_sub(2),
    );

    if inner.height < 7 || inner.width < 30 {
        return;
    }

    // Vertical split: main area | help bar
    let footer_height = 1u16;
    let main_height = inner.height.saturating_sub(footer_height);

    let vert_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(main_height),
            Constraint::Length(footer_height),
        ])
        .split(inner);

    let main_area = vert_chunks[0];
    let footer_area = vert_chunks[1];

    // Horizontal split: sidebar | right pane (tabs + content)
    let sidebar_width = {
        let ratio = app.sidebar_width_ratio;
        let w = (main_area.width as f32 * ratio) as u16;
        w.clamp(20, main_area.width.saturating_sub(30).max(20))
    };
    let right_width = main_area.width.saturating_sub(sidebar_width);

    let horiz_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(sidebar_width),
            Constraint::Length(right_width),
        ])
        .split(main_area);

    let sidebar_area = horiz_chunks[0];
    let right_area = horiz_chunks[1];

    // Right pane: tab bar (3 rows) + content
    let tab_height = 3u16;
    let content_height = right_area.height.saturating_sub(tab_height);

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tab_height),
            Constraint::Length(content_height),
        ])
        .split(right_area);

    let tab_area = right_chunks[0];
    let content_area = right_chunks[1];

    // Store areas for mouse hit-testing
    app.service_table_area = Some(sidebar_area);
    app.content_area = Some(content_area);
    app.pane_border_y = Some(sidebar_area.x + sidebar_area.width);

    // Render service sidebar
    widgets::service_table::render(f, sidebar_area, app);

    // Render floating tab bar above content (includes live/paused indicator)
    let (tab_rects, indicator_rect) = widgets::tab_bar::render(f, tab_area, app.tab, app.chart_mode, app.interval.as_secs(), &app.theme);
    app.tab_areas = tab_rects;
    app.live_indicator_area = indicator_rect;

    // Render content container — border matches the active tab style
    let content_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.theme.border_focused).bg(app.theme.bg_mantle))
        .style(Style::default().bg(app.theme.bg_surface));
    f.render_widget(content_block.clone(), content_area);
    let content_inner = content_block.inner(content_area);

    // Render content based on active tab
    match app.tab {
        Tab::Dashboard => {
            render_dashboard(f, content_inner, app);
        }
        Tab::Logs => {
            render_logs_tab(f, content_inner, app);
        }
        Tab::Detail => {
            widgets::detail_panel::render(f, content_inner, app);
        }
    }

    // Render help bar
    widgets::help_bar::render(f, footer_area, app);

    // Render confirm dialog if present
    if app.confirmation.is_some() {
        widgets::confirm_dialog::render(f, app);
    }

    // Render action feedback if present
    if let Some(ref feedback) = app.action_feedback {
        if feedback.expires > std::time::Instant::now() {
            render_feedback(f, feedback, &app.theme);
        }
    }

    // Render tooltip if present
    if let Some(tooltip) = &app.tooltip {
        widgets::tooltip::render(f, tooltip, &app.theme);
    }

    // Render overlay on top
    match &app.overlay {
        Overlay::Help => {
            widgets::popup::render_help(f, &app.theme);
        }
        Overlay::ContextMenu {
            x, y, selected, ..
        } => {
            widgets::context_menu::render(f, &app.theme, *x, *y, *selected);
        }
        Overlay::None => {}
    }
}

fn render_dashboard(f: &mut Frame, area: Rect, app: &mut TopApp) {
    if area.height < 6 {
        return;
    }

    let chart_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    app.chart_area = Some(area);
    app.cpu_chart_area = Some(chart_chunks[0]);
    app.memory_chart_area = Some(chart_chunks[1]);
    widgets::cpu_chart::render(f, chart_chunks[0], app);
    widgets::memory_chart::render(f, chart_chunks[1], app);

    // Render zoom selection overlay if dragging
    if let Some(ref zoom) = app.zoom_state {
        if zoom.is_real_drag {
            let col_min = zoom.start_col.min(zoom.current_col);
            let col_max = zoom.start_col.max(zoom.current_col);
            let chart_rect = match zoom.chart_kind {
                super::app::ChartKind::Cpu => chart_chunks[0],
                super::app::ChartKind::Memory => chart_chunks[1],
                super::app::ChartKind::LogLevel => return, // handled in render_logs_tab
            };
            // Clamp to chart area
            let x = col_min.max(chart_rect.x);
            let x_end = col_max.min(chart_rect.x + chart_rect.width);
            if x < x_end {
                let sel_rect = Rect::new(x, chart_rect.y, x_end - x, chart_rect.height);
                let sel_block = Block::default()
                    .style(Style::default().bg(app.theme.bg_highlight));
                f.render_widget(sel_block, sel_rect);
            }
        }
    }
}

fn render_logs_tab(f: &mut Frame, area: Rect, app: &mut TopApp) {
    // When detail panel is open, split horizontally: logs (left) | detail (right)
    let (logs_area, detail_area) = if app.log_detail_open {
        let detail_width = ((area.width as f32 * 0.40) as u16).clamp(30, 60).min(area.width.saturating_sub(30));
        let logs_width = area.width.saturating_sub(detail_width);
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(logs_width),
                Constraint::Length(detail_width),
            ])
            .split(area);
        (h_chunks[0], Some(h_chunks[1]))
    } else {
        (area, None)
    };

    if logs_area.height < 12 {
        // Too small for chart — just render log viewer
        app.log_chart_area = None;
        app.log_filter_area = None;
        app.log_viewer_area = Some(logs_area);
        widgets::log_viewer::render(f, logs_area, app);
        // Still render detail panel if open
        if let Some(detail_rect) = detail_area {
            app.log_detail_panel_area = Some(detail_rect);
            widgets::log_detail_panel::render(f, detail_rect, app);
        }
        return;
    }

    // Compute dynamic filter height: borders (2) + visual content lines, capped
    let filter_inner_width = logs_area.width.saturating_sub(2); // left + right border
    let visual_lines = app.log_filter_input.visual_height(filter_inner_width);
    let filter_height = (visual_lines + 2).min(10); // borders + cap at 10 rows

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Length(filter_height),
            Constraint::Min(0),
        ])
        .split(logs_area);

    let chart_area = chunks[0];
    let filter_area = chunks[1];
    let viewer_area = chunks[2];

    app.log_chart_area = Some(chart_area);
    app.log_filter_area = Some(filter_area);

    let chart_focused = app.log_focus == LogFocus::Chart;
    widgets::log_level_chart::render(f, chart_area, app, chart_focused);

    let filter_focused = app.log_focus == LogFocus::Filter;
    widgets::filter_input::render(f, filter_area, app, filter_focused);

    // Log viewer with bordered container showing focus state
    let viewer_focused = app.log_focus == LogFocus::Viewer;
    let viewer_border_color = if viewer_focused {
        app.theme.border_focused
    } else {
        app.theme.border
    };
    let viewer_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(viewer_border_color))
        .style(Style::default().bg(app.theme.bg_surface));
    let viewer_inner = viewer_block.inner(viewer_area);
    f.render_widget(viewer_block, viewer_area);
    app.log_viewer_area = Some(viewer_inner);
    widgets::log_viewer::render(f, viewer_inner, app);

    // Helper: highlight a column range on the log chart, only painting cells
    // that don't already have a bar background (so bars remain visible).
    let highlight_log_columns = |f: &mut Frame, x_start: u16, x_end: u16, y_start: u16, y_end: u16, theme: &crate::ui::theme::Theme| {
        let buf = f.buffer_mut();
        for x in x_start..x_end {
            for y in y_start..y_end {
                if let Some(cell) = buf.cell_mut(ratatui::layout::Position::new(x, y)) {
                    if cell.bg == theme.bg_surface || cell.bg == ratatui::style::Color::Reset {
                        cell.set_bg(theme.bg_highlight);
                    }
                }
            }
        }
    };

    // Render zoom selection overlay on log chart if dragging
    if let Some(ref zoom) = app.zoom_state {
        if zoom.is_real_drag && zoom.chart_kind == super::app::ChartKind::LogLevel {
            let col_min = zoom.start_col.min(zoom.current_col);
            let col_max = zoom.start_col.max(zoom.current_col);
            let x_start = col_min.max(chart_area.x);
            let x_end = col_max.min(chart_area.x + chart_area.width);
            if x_start < x_end {
                highlight_log_columns(f, x_start, x_end, chart_area.y, chart_area.y + chart_area.height, &app.theme);
            }
        }
    }

    // Render keyboard cursor on log chart (behind bars)
    if let Some(data_area) = app.log_chart_data_area() {
        if let Some(cursor_col) = app.log_chart_cursor_col(data_area.width) {
            let x = data_area.x + cursor_col;
            if x < data_area.x + data_area.width {
                highlight_log_columns(f, x, x + 1, data_area.y, data_area.y + data_area.height, &app.theme);
            }
        }

        // Render keyboard selection on log chart (behind bars)
        if let Some((start, end)) = app.log_chart_selection_cols(data_area.width) {
            let col_min = start.min(end);
            let col_max = start.max(end);
            let x_start = (data_area.x + col_min).max(data_area.x);
            let x_end = (data_area.x + col_max + 1).min(data_area.x + data_area.width);
            if x_start < x_end {
                highlight_log_columns(f, x_start, x_end, data_area.y, data_area.y + data_area.height, &app.theme);
            }
        }
    }

    // Render detail panel if open
    if let Some(detail_rect) = detail_area {
        app.log_detail_panel_area = Some(detail_rect);
        widgets::log_detail_panel::render(f, detail_rect, app);
    } else {
        app.log_detail_panel_area = None;
    }
}

fn render_feedback(
    f: &mut Frame,
    feedback: &super::app::ActionFeedback,
    theme: &crate::ui::theme::Theme,
) {
    use ratatui::widgets::Paragraph;

    let style = match feedback.kind {
        super::app::FeedbackKind::Success => Style::default().fg(theme.success).bg(theme.bg_overlay),
        super::app::FeedbackKind::Error => Style::default().fg(theme.error).bg(theme.bg_overlay),
        super::app::FeedbackKind::Info => Style::default().fg(theme.accent).bg(theme.bg_overlay),
    };

    let area = f.area();
    let width = (feedback.message.len() as u16 + 4).min(area.width.saturating_sub(4));
    let x = area.x + area.width.saturating_sub(width) - 2;
    let y = area.y + area.height.saturating_sub(3);
    let rect = Rect::new(x, y, width, 1);

    let paragraph = Paragraph::new(format!(" {} ", feedback.message)).style(style);
    f.render_widget(paragraph, rect);
}
