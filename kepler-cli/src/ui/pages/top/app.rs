use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::ui::theme::Theme;
use crate::ui::widgets::context_menu::MENU_ITEMS;
use crate::ui::widgets::text_input::{TextInputAction, TextInputConfig, TextInputState};
use crate::ui::widgets::utils::service_color;
use crate::ui::{AppEvent, Overlay, Tab};
use kepler_protocol::protocol::{MonitorMetricEntry, ServiceInfo};

const MAX_HISTORY_POINTS: usize = 300;
const DEFAULT_MAX_LOG_ENTRIES: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChartMode {
    Live,
    Paused { right_edge_ms: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Cpu,
    Memory,
    LogLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFocus {
    Chart,
    Filter,
    Viewer,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    Dsl,
    Sql,
}

#[derive(Debug, Clone, Copy)]
pub struct DragState {
    pub chart_kind: ChartKind,
    pub start_col: u16,
    pub initial_right_edge_ms: i64,
    pub is_real_drag: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ZoomState {
    pub chart_kind: ChartKind,
    pub start_col: u16,
    pub current_col: u16,
    pub is_real_drag: bool,
}

#[derive(Debug, Clone)]
pub struct ChartTooltip {
    pub chart_kind: ChartKind,
    pub screen_col: u16,
    pub screen_row: u16,
    pub timestamp_ms: i64,
    pub entries: Vec<(String, f64, ratatui::style::Color)>,
}

// --- Service management types ---

#[derive(Debug, Clone)]
pub enum AppAction {
    StartServices(Vec<String>),
    StopService(String),
    StopAll,
    RestartServices(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct ActionFeedback {
    pub message: String,
    pub kind: FeedbackKind,
    pub expires: std::time::Instant,
}

#[derive(Debug, Clone, Copy)]
pub enum FeedbackKind {
    Success,
    Error,
    Info,
}

#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub action: AppAction,
    pub selected: usize, // 0=Confirm, 1=Cancel
}

// --- Log types ---

#[derive(Debug, Clone)]
pub struct DisplayLogEntry {
    pub id: i64,
    pub service: String,
    pub hook: Option<String>,
    pub line: String,
    pub timestamp: i64,
    pub level: String,
    pub attributes: Option<String>,
}

pub struct TopApp {
    pub theme: Theme,
    pub services: Vec<String>,
    pub selected: usize,
    pub tab: Tab,
    pub overlay: Overlay,
    pub latest: HashMap<String, MonitorMetricEntry>,
    pub history: HashMap<String, VecDeque<MonitorMetricEntry>>,
    pub focused_service: Option<String>,
    pub should_quit: bool,
    pub interval: Duration,
    pub history_scroll: usize,
    pub chart_window: Duration,

    // Layout — sidebar + content
    pub sidebar_width_ratio: f32,
    pub dragging_border: bool,
    pub service_table_area: Option<Rect>,
    pub service_table_offset: usize,
    pub content_area: Option<Rect>,
    pub chart_area: Option<Rect>,
    #[allow(dead_code)]
    pub history_table_area: Option<Rect>,
    pub pane_border_y: Option<u16>,

    // Multi-select
    pub toggled_services: HashSet<String>,
    // Connection state
    pub disconnected: bool,
    pub consecutive_failures: u32,
    // Chart interaction
    pub cpu_chart_area: Option<Rect>,
    pub memory_chart_area: Option<Rect>,
    pub chart_mode: ChartMode,
    pub drag_state: Option<DragState>,
    pub zoom_state: Option<ZoomState>,
    pub tooltip: Option<ChartTooltip>,
    pub live_indicator_area: Option<Rect>,
    pub tab_areas: Vec<(Tab, Rect)>,
    pub detail_button_areas: Vec<(AppAction, Rect)>,

    // --- Service management ---
    pub service_status: HashMap<String, ServiceInfo>,
    pub pending_actions: Vec<AppAction>,
    pub action_feedback: Option<ActionFeedback>,
    pub confirmation: Option<ConfirmDialog>,
    pub confirm_button_areas: Vec<(usize, Rect)>, // (button_index, area) for click handling

    // --- Logs ---
    pub log_entries: VecDeque<DisplayLogEntry>,
    pub log_scroll: usize,
    pub log_last_id: Option<i64>,
    pub max_log_entries: usize,
    /// Debounced log refetch: when set, refetch logs after this instant
    pub log_refetch_at: Option<std::time::Instant>,

    // --- Log filter ---
    pub log_filter_input: TextInputState,
    pub log_filter_mode: FilterMode,
    pub log_filter_error: Option<String>,
    pub log_filter_applied: Option<(String, bool)>, // (query, is_sql)
    pub log_filter_dirty: bool,
    pub log_filter_area: Option<Rect>,

    // --- Log chart ---
    pub log_chart_area: Option<Rect>,
    pub log_viewer_area: Option<Rect>,
    pub log_focus: LogFocus,
    /// Cursor position as a timestamp (ms) anchored to a bucket.
    pub log_chart_cursor: Option<i64>,
    /// Selection range as timestamps (ms) anchored to buckets.
    pub log_chart_selection: Option<(i64, i64)>,

    // --- Log viewer cursor (stores log entry ID for stable identity) ---
    pub log_viewer_cursor: Option<i64>,

    // --- Log detail side panel ---
    pub log_detail_open: bool,
    pub log_detail_entry: Option<DisplayLogEntry>,
    pub log_detail_scroll: usize,
    pub log_detail_selected: usize,
    pub log_detail_panel_area: Option<Rect>,
    pub log_detail_copyable_count: usize,
    /// Clickable areas in the detail panel: (area, copyable_index)
    pub log_detail_click_areas: Vec<(Rect, usize)>,

    // --- Double-click tracking ---
    last_click: Option<(u16, u16, std::time::Instant)>,
}

impl TopApp {
    pub fn new(interval: Duration, initial_service: Option<String>) -> Self {
        Self {
            theme: Theme::default_theme(),
            services: Vec::new(),
            selected: 0,
            tab: Tab::Dashboard,
            overlay: Overlay::None,
            latest: HashMap::new(),
            history: HashMap::new(),
            focused_service: initial_service,
            should_quit: false,
            interval,
            history_scroll: 0,
            chart_window: Duration::from_secs(300),
            sidebar_width_ratio: 0.25,
            dragging_border: false,
            service_table_area: None,
            service_table_offset: 0,
            content_area: None,
            chart_area: None,
            history_table_area: None,
            pane_border_y: None,
            toggled_services: HashSet::new(),
            disconnected: false,
            consecutive_failures: 0,
            cpu_chart_area: None,
            memory_chart_area: None,
            chart_mode: ChartMode::Live,
            drag_state: None,
            zoom_state: None,
            tooltip: None,
            live_indicator_area: None,
            tab_areas: Vec::new(),
            detail_button_areas: Vec::new(),
            service_status: HashMap::new(),
            pending_actions: Vec::new(),
            action_feedback: None,
            confirmation: None,
            confirm_button_areas: Vec::new(),
            log_entries: VecDeque::new(),
            log_scroll: 0,
            log_last_id: None,
            max_log_entries: DEFAULT_MAX_LOG_ENTRIES,
            log_refetch_at: None,
            log_filter_input: TextInputState::new(TextInputConfig {
                wrapping: true,
                multiline: true,
                placeholder: "Type filter query...".into(),
            }),
            log_filter_mode: FilterMode::Dsl,
            log_filter_error: None,
            log_filter_applied: None,
            log_filter_dirty: false,
            log_filter_area: None,
            log_chart_area: None,
            log_viewer_area: None,
            log_focus: LogFocus::Chart,
            log_chart_cursor: None,
            log_chart_selection: None,
            log_viewer_cursor: None,
            log_detail_open: false,
            log_detail_entry: None,
            log_detail_scroll: 0,
            log_detail_selected: 0,
            log_detail_panel_area: None,
            log_detail_copyable_count: 1,
            log_detail_click_areas: Vec::new(),
            last_click: None,
        }
    }

    /// Returns the list of services that should be visible in charts/tables.
    /// Uses focused > toggled > cursor-selected > all.
    pub fn visible_services(&self) -> Vec<&String> {
        if let Some(svc) = &self.focused_service {
            self.services.iter().filter(|s| *s == svc).collect()
        } else if !self.toggled_services.is_empty() {
            self.services
                .iter()
                .filter(|s| self.toggled_services.contains(s.as_str()))
                .collect()
        } else if self.selected > 0 {
            if let Some(svc) = self.services.get(self.selected - 1) {
                vec![svc]
            } else {
                self.services.iter().collect()
            }
        } else {
            self.services.iter().collect()
        }
    }

    /// Whether we're in the "all services" view.
    pub fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.tooltip = None;
        self.zoom_state = None;
        self.drag_state = None;
        self.log_chart_cursor = None;
        self.log_chart_selection = None;
        // Clear stale chart areas from the tab we're leaving
        // so hit_test_chart() doesn't match regions that belong to another tab
        self.cpu_chart_area = None;
        self.memory_chart_area = None;
        self.chart_area = None;
        self.log_chart_area = None;
        self.log_viewer_area = None;
        self.log_filter_area = None;
        // Reset detail panel
        self.log_detail_open = false;
        self.log_detail_entry = None;
        self.log_viewer_cursor = None;
        self.log_detail_scroll = 0;
        self.log_detail_panel_area = None;

        // When switching to Logs in Live mode, reset cursor to re-sync from start
        if tab == Tab::Logs && matches!(self.chart_mode, ChartMode::Live) {
            self.log_last_id = None;
            self.log_entries.clear();
            self.log_scroll = 0;
        }
    }

    pub fn is_all_view(&self) -> bool {
        self.focused_service.is_none() && self.toggled_services.is_empty() && self.selected == 0
    }

    /// Get the chart color for a service by its global index.
    pub fn service_chart_color(&self, service: &str) -> ratatui::style::Color {
        let idx = self.services.iter().position(|s| s == service).unwrap_or(0);
        service_color(idx, &self.theme)
    }

    /// Whether a service is toggled on in multi-select mode.
    pub fn is_toggled(&self, service: &str) -> bool {
        self.toggled_services.contains(service)
    }

    /// Check if a screen position is inside a chart area.
    pub fn hit_test_chart(&self, col: u16, row: u16) -> Option<ChartKind> {
        if let Some(area) = self.cpu_chart_area {
            if col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height {
                return Some(ChartKind::Cpu);
            }
        }
        if let Some(area) = self.memory_chart_area {
            if col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height {
                return Some(ChartKind::Memory);
            }
        }
        if let Some(area) = self.log_chart_area {
            if col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height {
                return Some(ChartKind::LogLevel);
            }
        }
        None
    }

    /// Returns the right edge timestamp in ms.
    pub fn current_right_edge_ms(&self) -> i64 {
        match self.chart_mode {
            ChartMode::Live => chrono::Utc::now().timestamp_millis(),
            ChartMode::Paused { right_edge_ms } => right_edge_ms,
        }
    }

    /// Build the filtered log entries list (matching time window + visible services).
    pub fn log_filtered_entries(&self) -> Vec<&DisplayLogEntry> {
        let right_edge_ms = self.current_right_edge_ms();
        let window_ms = self.chart_window.as_millis() as i64;
        let left_edge_ms = right_edge_ms - window_ms;
        let visible_svcs: Vec<&String> = self.visible_services();
        let show_all = self.is_all_view();
        self.log_entries
            .iter()
            .filter(|e| e.timestamp >= left_edge_ms && e.timestamp <= right_edge_ms)
            .filter(|e| show_all || visible_svcs.iter().any(|s| s.as_str() == e.service))
            .collect()
    }

    /// Count of currently filtered log entries.
    pub fn log_filtered_count(&self) -> usize {
        self.log_filtered_entries().len()
    }

    /// Resolve cursor ID to index in the filtered list. Returns None if not found.
    pub fn cursor_index(&self) -> Option<usize> {
        let cursor_id = self.log_viewer_cursor?;
        let filtered = self.log_filtered_entries();
        filtered.iter().position(|e| e.id == cursor_id)
    }

    /// Move cursor to the previous (older) entry in the filtered list.
    pub fn move_cursor_up(&mut self) {
        let filtered = self.log_filtered_entries();
        let total = filtered.len();
        if total == 0 { return; }
        match self.log_viewer_cursor {
            None => {
                // Initialize at the last (newest) entry
                self.log_viewer_cursor = Some(filtered[total - 1].id);
            }
            Some(cursor_id) => {
                if let Some(pos) = filtered.iter().position(|e| e.id == cursor_id) {
                    if pos > 0 {
                        self.log_viewer_cursor = Some(filtered[pos - 1].id);
                    }
                } else {
                    // Cursor ID no longer in filtered list — snap to last
                    self.log_viewer_cursor = Some(filtered[total - 1].id);
                }
            }
        }
    }

    /// Move cursor to the next (newer) entry in the filtered list.
    pub fn move_cursor_down(&mut self) {
        let filtered = self.log_filtered_entries();
        let total = filtered.len();
        if total == 0 { return; }
        match self.log_viewer_cursor {
            None => {
                self.log_viewer_cursor = Some(filtered[total - 1].id);
            }
            Some(cursor_id) => {
                if let Some(pos) = filtered.iter().position(|e| e.id == cursor_id) {
                    if pos + 1 < total {
                        self.log_viewer_cursor = Some(filtered[pos + 1].id);
                    }
                } else {
                    self.log_viewer_cursor = Some(filtered[total - 1].id);
                }
            }
        }
    }

    /// Move cursor up by `count` entries.
    pub fn move_cursor_up_by(&mut self, count: usize) {
        let filtered = self.log_filtered_entries();
        let total = filtered.len();
        if total == 0 { return; }
        let pos = match self.log_viewer_cursor {
            Some(cursor_id) => filtered.iter().position(|e| e.id == cursor_id).unwrap_or(total - 1),
            None => total - 1,
        };
        let new_pos = pos.saturating_sub(count);
        self.log_viewer_cursor = Some(filtered[new_pos].id);
    }

    /// Move cursor down by `count` entries.
    pub fn move_cursor_down_by(&mut self, count: usize) {
        let filtered = self.log_filtered_entries();
        let total = filtered.len();
        if total == 0 { return; }
        let pos = match self.log_viewer_cursor {
            Some(cursor_id) => filtered.iter().position(|e| e.id == cursor_id).unwrap_or(total - 1),
            None => total - 1,
        };
        let new_pos = (pos + count).min(total - 1);
        self.log_viewer_cursor = Some(filtered[new_pos].id);
    }

    /// Adjust log_scroll so the cursor is within the visible viewport.
    pub fn ensure_cursor_visible(&mut self) {
        let cursor_idx = match self.cursor_index() {
            Some(idx) => idx,
            None => return,
        };
        let inner_height = self.log_viewer_area.map(|a| a.height as usize).unwrap_or(20);
        let total = self.log_filtered_count();
        if total == 0 || inner_height == 0 {
            return;
        }
        let max_scroll = total.saturating_sub(inner_height);
        // The viewer shows entries from index `skip` to `skip + inner_height`
        // where skip = total - inner_height - scroll (bottom-aligned)
        // We need: skip <= cursor_idx < skip + inner_height
        let min_scroll_for_cursor = total.saturating_sub(cursor_idx + inner_height);
        let max_scroll_for_cursor = if cursor_idx < total {
            total - cursor_idx - 1
        } else {
            0
        };
        self.log_scroll = self.log_scroll
            .max(min_scroll_for_cursor)
            .min(max_scroll_for_cursor.min(max_scroll));
    }

    /// Update the detail panel entry from the current cursor.
    pub fn update_log_detail_from_cursor(&mut self) {
        if let Some(cursor_id) = self.log_viewer_cursor {
            let filtered = self.log_filtered_entries();
            if let Some(entry) = filtered.iter().find(|e| e.id == cursor_id) {
                self.log_detail_entry = Some((*entry).clone());
                self.log_detail_scroll = 0;
                self.log_detail_selected = 0;
            }
        }
    }

    /// Open the log detail panel for the entry at the given filtered index.
    pub fn open_log_detail(&mut self, idx: usize) {
        let filtered = self.log_filtered_entries();
        if let Some(entry) = filtered.get(idx) {
            let entry_id = entry.id;
            let entry_clone = (*entry).clone();
            drop(filtered);
            self.log_viewer_cursor = Some(entry_id);
            self.log_detail_entry = Some(entry_clone);
            self.log_detail_open = true;
            self.log_detail_scroll = 0;
            self.log_detail_selected = 0;
            self.log_focus = LogFocus::Detail;
        }
    }

    /// Get the currently selected/focused service name.
    pub fn selected_service_name(&self) -> Option<&str> {
        if let Some(svc) = &self.focused_service {
            Some(svc.as_str())
        } else if self.selected == 0 {
            None
        } else {
            self.services.get(self.selected - 1).map(|s| s.as_str())
        }
    }

    /// Get service names for actions (focused > toggled > selected > all).
    /// Returns `None` only when there are no services at all.
    /// Returns `Some(vec![])` when ALL is selected (meaning "all services").
    pub fn action_target_services(&self) -> Option<Vec<String>> {
        if self.services.is_empty() {
            return None;
        }
        if let Some(svc) = &self.focused_service {
            Some(vec![svc.clone()])
        } else if !self.toggled_services.is_empty() {
            Some(self.toggled_services.iter().cloned().collect())
        } else if self.selected > 0 {
            self.services.get(self.selected - 1).map(|svc| vec![svc.clone()])
        } else {
            // ALL row — empty vec means all services
            Some(vec![])
        }
    }

    /// Whether the current action target is "all services" (ALL row, no focus/toggles).
    fn is_all_action(&self) -> bool {
        self.focused_service.is_none() && self.toggled_services.is_empty() && self.selected == 0
    }

    /// Set feedback message that auto-expires.
    /// Schedule a debounced log refetch (300ms from now).
    /// Each call resets the timer so rapid panning only triggers one fetch.
    pub fn schedule_log_refetch(&mut self) {
        self.log_refetch_at = Some(std::time::Instant::now() + Duration::from_millis(300));
    }

    pub fn set_feedback(&mut self, message: String, kind: FeedbackKind) {
        self.action_feedback = Some(ActionFeedback {
            message,
            kind,
            expires: std::time::Instant::now() + Duration::from_secs(3),
        });
    }

    /// Copy text to the system clipboard.
    /// Tries arboard first, falls back to shell commands on failure.
    pub fn copy_to_clipboard(&mut self, text: &str) {
        // Try arboard first
        if let Ok(mut cb) = arboard::Clipboard::new() {
            if cb.set_text(text).is_ok() {
                self.set_feedback("Copied to clipboard".into(), FeedbackKind::Success);
                return;
            }
        }
        // Fallback to shell commands
        let result = if cfg!(target_os = "macos") {
            Self::clipboard_via_cmd("pbcopy", text)
        } else {
            // Try wl-copy first (Wayland), then xclip (X11)
            Self::clipboard_via_cmd("wl-copy", text)
                .or_else(|_| Self::clipboard_via_cmd("xclip", text))
        };
        match result {
            Ok(()) => self.set_feedback("Copied to clipboard".into(), FeedbackKind::Success),
            Err(_) => self.set_feedback("Clipboard not available".into(), FeedbackKind::Error),
        }
    }

    fn clipboard_via_cmd(cmd: &str, text: &str) -> Result<(), std::io::Error> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut args: Vec<&str> = Vec::new();
        if cmd == "xclip" {
            args.push("-selection");
            args.push("clipboard");
        }
        let mut child = Command::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        child.wait()?;
        Ok(())
    }

    /// Copy the currently selected item in the detail panel to clipboard.
    pub fn copy_detail_selected(&mut self) {
        let entry = match &self.log_detail_entry {
            Some(e) => e.clone(),
            None => return,
        };
        if self.log_detail_selected == 0 {
            // Copy raw text
            self.copy_to_clipboard(&entry.line);
        } else {
            // Copy attribute by index
            let attr_idx = self.log_detail_selected - 1;
            if let Some(ref attrs_str) = entry.attributes {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(attrs_str) {
                    if let Some(obj) = parsed.as_object() {
                        if let Some((_key, value)) = obj.iter().nth(attr_idx) {
                            let text = match value.as_str() {
                                Some(s) => s.to_string(),
                                None => value.to_string(),
                            };
                            self.copy_to_clipboard(&text);
                        }
                    }
                }
            }
        }
    }

    /// View the selected log entry in context — centers a 30s window and clears filter.
    pub fn view_log_in_context(&mut self) {
        let entry = match &self.log_detail_entry {
            Some(e) => e.clone(),
            None => return,
        };
        let ts = entry.timestamp;
        let context_window_ms = 30_000i64;
        self.chart_window = Duration::from_millis(context_window_ms as u64);
        self.chart_mode = ChartMode::Paused {
            right_edge_ms: ts + context_window_ms / 2,
        };
        // Clear filter
        self.log_filter_input.clear();
        self.log_filter_applied = None;
        self.log_filter_error = None;
        self.log_filter_dirty = false;
        // Re-fetch logs
        self.log_last_id = None;
        self.log_entries.clear();
        self.log_scroll = 0;
        // Close detail panel
        self.log_detail_open = false;
        self.log_detail_entry = None;
        self.log_viewer_cursor = None;
        self.log_focus = LogFocus::Viewer;
    }

    /// Compute tooltip data at the given screen position.
    pub fn show_tooltip_at(&mut self, col: u16, row: u16, chart_kind: ChartKind) {
        if chart_kind == ChartKind::LogLevel {
            self.show_log_tooltip_at(col, row);
            return;
        }

        use crate::ui::widgets::utils;

        let chart_area = match chart_kind {
            ChartKind::Cpu => self.cpu_chart_area,
            ChartKind::Memory => self.memory_chart_area,
            ChartKind::LogLevel => unreachable!(),
        };
        let chart_area = match chart_area {
            Some(a) => a,
            None => return,
        };

        let data_area = utils::estimate_chart_data_area(chart_area, 8);
        if data_area.width == 0 || data_area.height == 0 {
            return;
        }

        let right_edge_ms = self.current_right_edge_ms();
        let window_secs = self.chart_window.as_secs_f64();
        let x_bounds = [0.0, window_secs];

        let query_x = match utils::pixel_to_data_x(col, data_area, x_bounds) {
            Some(x) => x,
            None => return,
        };

        let timestamp_ms = right_edge_ms - ((window_secs - query_x) * 1000.0) as i64;
        let visible = self.visible_services();
        let extractor: fn(&MonitorMetricEntry) -> f64 = match chart_kind {
            ChartKind::Cpu => |e| e.cpu_percent as f64,
            ChartKind::Memory => |e| e.memory_rss as f64,
            ChartKind::LogLevel => unreachable!(),
        };

        let mut datasets: Vec<(String, Vec<(f64, f64)>, ratatui::style::Color)> = Vec::new();
        for svc in &visible {
            if let Some(history) = self.history.get(*svc) {
                let points = utils::chart_data_points(history, self.chart_window, right_edge_ms, extractor);
                let color = self.service_chart_color(svc);
                datasets.push(((*svc).clone(), points, color));
            }
        }

        if self.is_all_view() && !self.services.is_empty() {
            let total_history = self.compute_total_history();
            let points = utils::chart_data_points(&total_history, self.chart_window, right_edge_ms, extractor);
            datasets.push(("Total".to_string(), points, self.theme.fg_bright));
        }

        let nearest = utils::find_nearest_values(&datasets, query_x);
        let tooltip_entries: Vec<_> = nearest
            .into_iter()
            .map(|(name, value, color)| (name, value, color))
            .collect();

        if !tooltip_entries.is_empty() {
            self.tooltip = Some(ChartTooltip {
                chart_kind,
                screen_col: col,
                screen_row: row,
                timestamp_ms,
                entries: tooltip_entries,
            });
        }
    }

    /// Compute log level tooltip at a screen position.
    fn show_log_tooltip_at(&mut self, col: u16, row: u16) {
        let chart_area = match self.log_chart_area {
            Some(a) => a,
            None => return,
        };

        // Estimate data area: 1 border + 4 y-label + inner
        let y_label_width: u16 = 4;
        let data_area = Rect::new(
            chart_area.x + 1 + y_label_width,
            chart_area.y + 1,
            chart_area.width.saturating_sub(2 + y_label_width),
            chart_area.height.saturating_sub(3), // border top + border bottom + x-axis
        );

        if data_area.width == 0 || col < data_area.x || col >= data_area.x + data_area.width {
            return;
        }

        // Use snapped bucket boundaries matching the chart renderer
        let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_area.width);

        // Determine which bucket this column represents
        let col_in_data = (col - data_area.x) as i64;
        let bucket_left = snapped_left + col_in_data * bucket_ms;
        let bucket_right = bucket_left + bucket_ms;
        let timestamp_ms = (bucket_left + bucket_right) / 2;

        let visible_svcs: Vec<&String> = self.visible_services();
        let show_all = self.is_all_view();

        let mut err_count: u32 = 0;
        let mut warn_count: u32 = 0;
        let mut info_count: u32 = 0;

        for entry in &self.log_entries {
            if entry.timestamp < bucket_left || entry.timestamp >= bucket_right {
                continue;
            }
            if !show_all && !visible_svcs.iter().any(|s| s.as_str() == entry.service) {
                continue;
            }
            match entry.level.as_str() {
                "err" | "error" | "fatal" => err_count += 1,
                "warn" | "warning" => warn_count += 1,
                _ => info_count += 1,
            }
        }

        let mut entries = Vec::new();
        if err_count > 0 {
            entries.push(("err".to_string(), err_count as f64, self.theme.error));
        }
        if warn_count > 0 {
            entries.push(("warn".to_string(), warn_count as f64, self.theme.warning));
        }
        if info_count > 0 {
            entries.push(("info".to_string(), info_count as f64, self.theme.accent));
        }

        if !entries.is_empty() {
            self.tooltip = Some(ChartTooltip {
                chart_kind: ChartKind::LogLevel,
                screen_col: col,
                screen_row: row,
                timestamp_ms,
                entries,
            });
        }
    }

    /// Compute the log chart data area (for cursor/selection positioning).
    pub fn log_chart_data_area(&self) -> Option<Rect> {
        let chart_area = self.log_chart_area?;
        let y_label_width: u16 = 4;
        let data_area = Rect::new(
            chart_area.x + 1 + y_label_width,
            chart_area.y + 1,
            chart_area.width.saturating_sub(2 + y_label_width),
            chart_area.height.saturating_sub(3),
        );
        if data_area.width == 0 || data_area.height == 0 {
            None
        } else {
            Some(data_area)
        }
    }

    /// Compute snapped bucket boundaries for the log chart.
    /// Returns (snapped_left, bucket_ms) matching the chart renderer's snapping.
    fn log_chart_snapped_bounds(&self, data_width: u16) -> (i64, i64) {
        let right_edge_ms = self.current_right_edge_ms();
        let window_ms = self.chart_window.as_millis() as i64;
        let width = data_width as i64;
        let bucket_ms = window_ms / width;
        let snapped_right = if bucket_ms > 0 {
            ((right_edge_ms + bucket_ms - 1) / bucket_ms) * bucket_ms
        } else {
            right_edge_ms
        };
        let snapped_left = snapped_right - bucket_ms * width;
        (snapped_left, bucket_ms)
    }

    /// Convert a column index to a bucket timestamp (left edge of the bucket).
    fn log_chart_col_to_ts(&self, col: u16, data_width: u16) -> i64 {
        let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_width);
        snapped_left + col as i64 * bucket_ms
    }

    /// Convert a bucket timestamp to a column index (clamped to data width).
    pub fn log_chart_ts_to_col(&self, ts: i64, data_width: u16) -> u16 {
        let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_width);
        if bucket_ms == 0 {
            return 0;
        }
        let col = (ts - snapped_left) / bucket_ms;
        col.max(0).min(data_width as i64 - 1) as u16
    }

    /// Convert the current cursor timestamp to a column index, if set.
    pub fn log_chart_cursor_col(&self, data_width: u16) -> Option<u16> {
        self.log_chart_cursor.map(|ts| self.log_chart_ts_to_col(ts, data_width))
    }

    /// Convert the current selection timestamps to column indices, if set.
    pub fn log_chart_selection_cols(&self, data_width: u16) -> Option<(u16, u16)> {
        self.log_chart_selection.map(|(start, end)| {
            (self.log_chart_ts_to_col(start, data_width), self.log_chart_ts_to_col(end, data_width))
        })
    }

    /// Compute a bitmask of non-empty log chart buckets for cursor navigation.
    fn log_chart_nonempty_buckets(&self, data_width: u16) -> Vec<bool> {
        let width = data_width as usize;
        let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_width);
        let snapped_right = snapped_left + bucket_ms * width as i64;

        let visible_svcs: Vec<&String> = self.visible_services();
        let show_all = self.is_all_view();

        let mut nonempty = vec![false; width];
        if bucket_ms == 0 {
            return nonempty;
        }
        for entry in &self.log_entries {
            if entry.timestamp < snapped_left || entry.timestamp >= snapped_right {
                continue;
            }
            if !show_all && !visible_svcs.iter().any(|s| s.as_str() == entry.service) {
                continue;
            }
            let col = ((entry.timestamp - snapped_left) / bucket_ms) as usize;
            let col = col.min(width.saturating_sub(1));
            nonempty[col] = true;
        }
        nonempty
    }

    /// Update autocompletion suggestions based on current cursor position and filter mode.
    pub fn update_filter_completions(&mut self) {
        let line = &self.log_filter_input.lines[self.log_filter_input.cursor_row];
        let col = self.log_filter_input.cursor_col.min(line.len());
        let before = &line[..col];

        let (completions, replace_start) = match self.log_filter_mode {
            FilterMode::Dsl => {
                if let Some(at_pos) = before.rfind('@') {
                    let word = &before[at_pos..];
                    if let Some(colon_pos) = word.find(':') {
                        let field = &word[1..colon_pos];
                        let prefix = &word[colon_pos + 1..];
                        (self.collect_field_values(field, prefix), at_pos + colon_pos + 1)
                    } else {
                        let fields = ["@service", "@level", "@hook", "@message"];
                        let completions: Vec<String> = fields
                            .iter()
                            .filter(|f| f.starts_with(word))
                            .map(|f| f.to_string())
                            .collect();
                        (completions, at_pos)
                    }
                } else {
                    (Vec::new(), col)
                }
            }
            FilterMode::Sql => {
                let word_start = before
                    .rfind(|c: char| c.is_whitespace() || c == '(' || c == ',')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let word = &before[word_start..];
                if !word.is_empty() {
                    let columns = ["id", "timestamp", "service", "hook", "level", "line", "attributes"];
                    let lower = word.to_lowercase();
                    let completions: Vec<String> = columns
                        .iter()
                        .filter(|c| c.starts_with(&lower))
                        .map(|c| c.to_string())
                        .collect();
                    (completions, word_start)
                } else {
                    (Vec::new(), col)
                }
            }
        };
        self.log_filter_input.set_completions(completions, replace_start);
    }

    /// Collect unique field values from log entries for autocompletion.
    fn collect_field_values(&self, field: &str, prefix: &str) -> Vec<String> {
        match field {
            "service" => {
                let mut values: Vec<String> = self.log_entries
                    .iter()
                    .map(|e| e.service.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .filter(|v| prefix.is_empty() || v.starts_with(prefix))
                    .collect();
                values.sort();
                values
            }
            "level" => {
                let levels = ["error", "warn", "info", "debug", "trace", "fatal"];
                levels
                    .iter()
                    .filter(|l| prefix.is_empty() || l.starts_with(prefix))
                    .map(|l| l.to_string())
                    .collect()
            }
            "hook" => {
                let mut values: Vec<String> = self.log_entries
                    .iter()
                    .filter_map(|e| e.hook.as_ref())
                    .cloned()
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .filter(|v| prefix.is_empty() || v.starts_with(prefix))
                    .collect();
                values.sort();
                values
            }
            _ => Vec::new(),
        }
    }

    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Key(key) => self.handle_key(key),
            AppEvent::Mouse(mouse) => self.handle_mouse(mouse),
            AppEvent::Tick => {}
            AppEvent::Resize(_, _) => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        // Confirmation dialog takes priority
        if let Some(ref mut confirm) = self.confirmation {
            match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    confirm.selected = 0;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    confirm.selected = 1;
                }
                KeyCode::Enter => {
                    if confirm.selected == 0 {
                        // Confirm — push action
                        let action = confirm.action.clone();
                        self.pending_actions.push(action);
                    }
                    self.confirmation = None;
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.confirmation = None;
                }
                _ => {}
            }
            return;
        }

        // Overlay events take priority
        match &self.overlay {
            Overlay::Help => {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                        self.overlay = Overlay::None;
                    }
                    _ => {}
                }
                return;
            }
            Overlay::ContextMenu { service, selected, .. } => {
                let service = service.clone();
                let sel = *selected;
                match key.code {
                    KeyCode::Esc => {
                        self.overlay = Overlay::None;
                    }
                    KeyCode::Up => {
                        if let Overlay::ContextMenu { selected, .. } = &mut self.overlay {
                            *selected = selected.saturating_sub(1);
                        }
                    }
                    KeyCode::Down => {
                        if let Overlay::ContextMenu { selected, .. } = &mut self.overlay {
                            *selected = (*selected + 1).min(MENU_ITEMS.len() - 1);
                        }
                    }
                    KeyCode::Enter => {
                        self.overlay = Overlay::None;
                        self.execute_menu_action(&service, sel);
                    }
                    _ => {}
                }
                return;
            }
            Overlay::None => {}
        }

        // Any key dismisses tooltip
        if self.tooltip.is_some() {
            self.tooltip = None;
        }

        // Logs tab with chart focus: handle dedicated controls
        if self.tab == Tab::Logs && self.log_focus == LogFocus::Chart {
            match key.code {
                KeyCode::Char('q') => { self.should_quit = true; return; }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.should_quit = true; return;
                }
                KeyCode::Char('?') => { self.overlay = Overlay::Help; return; }
                KeyCode::Char('p') => {
                    match self.chart_mode {
                        ChartMode::Live => {
                            let now = chrono::Utc::now().timestamp_millis();
                            self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                        }
                        ChartMode::Paused { .. } => {
                            self.chart_mode = ChartMode::Live;
                            self.log_scroll = 0;
                            self.tooltip = None;
                            self.log_chart_cursor = None;
                            self.log_chart_selection = None;
                        }
                    }
                    return;
                }
                KeyCode::Tab => {
                    self.log_focus = LogFocus::Filter;
                    return;
                }
                KeyCode::BackTab => {
                    self.log_focus = LogFocus::Viewer;
                    return;
                }
                KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    // Extend selection leftward, skipping empty buckets
                    if let Some(data_area) = self.log_chart_data_area() {
                        let cursor_col = self.log_chart_cursor_col(data_area.width)
                            .unwrap_or(data_area.width.saturating_sub(1));
                        if cursor_col > 0 {
                            let nonempty = self.log_chart_nonempty_buckets(data_area.width);
                            let mut next = cursor_col - 1;
                            while next > 0 && !nonempty[next as usize] {
                                next -= 1;
                            }
                            let new_ts = self.log_chart_col_to_ts(next, data_area.width);
                            if let Some((start_ts, _end)) = self.log_chart_selection {
                                self.log_chart_cursor = Some(new_ts);
                                self.log_chart_selection = Some((start_ts, new_ts));
                            } else {
                                let start_ts = self.log_chart_col_to_ts(cursor_col, data_area.width);
                                self.log_chart_selection = Some((start_ts, new_ts));
                                self.log_chart_cursor = Some(new_ts);
                            }
                        }
                    }
                    return;
                }
                KeyCode::Left => {
                    // Move cursor left, skipping empty buckets
                    if let Some(data_area) = self.log_chart_data_area() {
                        let cursor_col = self.log_chart_cursor_col(data_area.width)
                            .unwrap_or(data_area.width.saturating_sub(1));
                        if cursor_col > 0 {
                            let nonempty = self.log_chart_nonempty_buckets(data_area.width);
                            // Find next non-empty bucket to the left, or just step by 1
                            let mut next = cursor_col - 1;
                            while next > 0 && !nonempty[next as usize] {
                                next -= 1;
                            }
                            self.log_chart_cursor = Some(self.log_chart_col_to_ts(next, data_area.width));
                        } else {
                            // Pan left
                            let bucket_ms = self.chart_window.as_millis() as i64 / data_area.width.max(1) as i64;
                            if matches!(self.chart_mode, ChartMode::Live) {
                                let now = chrono::Utc::now().timestamp_millis();
                                self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                            }
                            if let ChartMode::Paused { right_edge_ms } = &mut self.chart_mode {
                                *right_edge_ms -= bucket_ms;
                            }
                            self.log_chart_cursor = Some(self.log_chart_col_to_ts(0, data_area.width));
                            self.schedule_log_refetch();
                        }
                        self.log_chart_selection = None;
                    }
                    return;
                }
                KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    // Extend selection rightward, skipping empty buckets
                    if let Some(data_area) = self.log_chart_data_area() {
                        let max_col = data_area.width.saturating_sub(1);
                        let cursor_col = self.log_chart_cursor_col(data_area.width).unwrap_or(0);
                        if cursor_col < max_col {
                            let nonempty = self.log_chart_nonempty_buckets(data_area.width);
                            let mut next = cursor_col + 1;
                            while next < max_col && !nonempty[next as usize] {
                                next += 1;
                            }
                            let new_ts = self.log_chart_col_to_ts(next, data_area.width);
                            if let Some((start_ts, _end)) = self.log_chart_selection {
                                self.log_chart_cursor = Some(new_ts);
                                self.log_chart_selection = Some((start_ts, new_ts));
                            } else {
                                let start_ts = self.log_chart_col_to_ts(cursor_col, data_area.width);
                                self.log_chart_selection = Some((start_ts, new_ts));
                                self.log_chart_cursor = Some(new_ts);
                            }
                        }
                    }
                    return;
                }
                KeyCode::Right => {
                    // Move cursor right, skipping empty buckets
                    if let Some(data_area) = self.log_chart_data_area() {
                        let max_col = data_area.width.saturating_sub(1);
                        let cursor_col = self.log_chart_cursor_col(data_area.width).unwrap_or(0);
                        if cursor_col < max_col {
                            let nonempty = self.log_chart_nonempty_buckets(data_area.width);
                            // Find next non-empty bucket to the right, or just step to end
                            let mut next = cursor_col + 1;
                            while next < max_col && !nonempty[next as usize] {
                                next += 1;
                            }
                            self.log_chart_cursor = Some(self.log_chart_col_to_ts(next, data_area.width));
                        } else {
                            // Pan right
                            let bucket_ms = self.chart_window.as_millis() as i64 / data_area.width.max(1) as i64;
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            if matches!(self.chart_mode, ChartMode::Live) {
                                self.chart_mode = ChartMode::Paused { right_edge_ms: now_ms };
                            }
                            if let ChartMode::Paused { right_edge_ms } = &mut self.chart_mode {
                                *right_edge_ms = (*right_edge_ms + bucket_ms).min(now_ms);
                            }
                            self.log_chart_cursor = Some(self.log_chart_col_to_ts(max_col, data_area.width));
                            self.schedule_log_refetch();
                        }
                        self.log_chart_selection = None;
                    }
                    return;
                }
                KeyCode::Enter => {
                    if let Some((start_ts, end_ts)) = self.log_chart_selection {
                        // Zoom to selection — timestamps are bucket left edges
                        if let Some(data_area) = self.log_chart_data_area() {
                            let (_, bucket_ms) = self.log_chart_snapped_bounds(data_area.width);
                            let t_left = start_ts.min(end_ts);
                            let t_right = start_ts.max(end_ts) + bucket_ms;
                            let new_window = (t_right - t_left).max(5000);
                            self.chart_window = Duration::from_millis(new_window as u64);
                            self.chart_mode = ChartMode::Paused { right_edge_ms: t_right };
                        }
                        self.log_chart_selection = None;
                        self.log_chart_cursor = None;
                    } else if let Some(cursor_ts) = self.log_chart_cursor {
                        // Zoom to single bar at cursor
                        if let Some(data_area) = self.log_chart_data_area() {
                            let (_, bucket_ms) = self.log_chart_snapped_bounds(data_area.width);
                            let t_right = cursor_ts + bucket_ms;
                            let new_window = bucket_ms.max(5000);
                            self.chart_window = Duration::from_millis(new_window as u64);
                            self.chart_mode = ChartMode::Paused { right_edge_ms: t_right };
                        }
                        self.log_chart_selection = None;
                        self.log_chart_cursor = None;
                    }
                    return;
                }
                KeyCode::Esc => {
                    if self.tooltip.is_some() {
                        self.tooltip = None;
                    } else if self.log_chart_selection.is_some() {
                        self.log_chart_selection = None;
                    } else if self.log_chart_cursor.is_some() {
                        self.log_chart_cursor = None;
                    } else {
                        self.log_focus = LogFocus::Viewer;
                    }
                    return;
                }
                KeyCode::Char('i') => {
                    if self.tooltip.is_some() {
                        self.tooltip = None;
                    } else if self.log_chart_cursor.is_some() {
                        if let Some(data_area) = self.log_chart_data_area() {
                            if let Some(cursor_col) = self.log_chart_cursor_col(data_area.width) {
                                let screen_col = data_area.x + cursor_col;
                                // Position tooltip near the top of the data area
                                let screen_row = data_area.y + 1;
                                self.show_log_tooltip_at(screen_col, screen_row);
                            }
                        }
                    }
                    return;
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    let secs = self.chart_window.as_secs();
                    if secs > 10 {
                        self.chart_window = Duration::from_secs(secs.saturating_sub(10).max(10));
                
                    }
                    return;
                }
                KeyCode::Char('-') => {
                    let secs = self.chart_window.as_secs();
                    if secs < 3600 {
                        self.chart_window = Duration::from_secs((secs + 10).min(3600));

                    }
                    return;
                }
                KeyCode::Char('0') => {
                    self.chart_window = Duration::from_secs(300);
                    self.chart_mode = ChartMode::Live;
                    self.log_scroll = 0;
                    self.log_chart_cursor = None;
                    self.log_chart_selection = None;
                    self.tooltip = None;
                    return;
                }
                KeyCode::Home => {
                    if let Some(data_area) = self.log_chart_data_area() {
                        self.log_chart_cursor = Some(self.log_chart_col_to_ts(0, data_area.width));
                    }
                    self.log_chart_selection = None;
                    return;
                }
                KeyCode::End => {
                    if let Some(data_area) = self.log_chart_data_area() {
                        let last_col = data_area.width.saturating_sub(1);
                        self.log_chart_cursor = Some(self.log_chart_col_to_ts(last_col, data_area.width));
                        self.log_chart_selection = None;
                    }
                    return;
                }
                // Service selection still works
                KeyCode::Up => {
                    if self.selected > 0 {
                        self.selected -= 1;
                        self.history_scroll = 0;
                    }
                    return;
                }
                KeyCode::Down => {
                    let max = self.services.len();
                    if self.selected < max {
                        self.selected += 1;
                        self.history_scroll = 0;
                    }
                    return;
                }
                // Fall through to common handlers for other keys
                _ => {}
            }
        }

        // Logs tab with filter focus: delegate to TextInputState
        if self.tab == Tab::Logs && self.log_focus == LogFocus::Filter {
            // App-specific Ctrl shortcuts (not part of generic widget)
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('d') => {
                        self.log_filter_input.clear();
                        self.log_filter_applied = None;
                        self.log_filter_error = None;
                        self.log_filter_dirty = false;
                        self.log_last_id = None;
                        self.log_entries.clear();
                        self.log_scroll = 0;
                        return;
                    }
                    KeyCode::Char('t') => {
                        self.log_filter_mode = match self.log_filter_mode {
                            FilterMode::Dsl => FilterMode::Sql,
                            FilterMode::Sql => FilterMode::Dsl,
                        };
                        self.log_filter_input.set_completions(Vec::new(), 0);
                        return;
                    }
                    _ => {}
                }
            }

            match self.log_filter_input.handle_key(key) {
                TextInputAction::Submit => {
                    let joined = self.log_filter_input.text();
                    let is_sql = self.log_filter_mode == FilterMode::Sql;
                    if joined.trim().is_empty() {
                        self.log_filter_applied = None;
                    } else {
                        self.log_filter_applied = Some((joined.clone(), is_sql));
                        self.log_filter_input.push_history(joined.trim().to_string());
                    }
                    self.log_filter_error = None;
                    self.log_filter_dirty = false;
                    self.log_last_id = None;
                    self.log_entries.clear();
                    self.log_scroll = 0;
                }
                TextInputAction::Cancel => {
                    self.log_focus = LogFocus::Chart;
                }
                TextInputAction::FocusNext => {
                    self.log_focus = LogFocus::Viewer;
                }
                TextInputAction::FocusPrev => {
                    self.log_focus = LogFocus::Chart;
                }
                TextInputAction::CompletionRequested => {
                    self.update_filter_completions();
                }
                TextInputAction::Changed => {
                    self.log_filter_dirty = true;
                    self.update_filter_completions();
                }
                TextInputAction::Noop => {}
            }
            return;
        }

        // Logs tab with viewer focus: dedicated controls
        if self.tab == Tab::Logs && self.log_focus == LogFocus::Viewer {
            match key.code {
                KeyCode::Char('q') => { self.should_quit = true; return; }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.should_quit = true; return;
                }
                KeyCode::Char('?') => { self.overlay = Overlay::Help; return; }
                KeyCode::Char('p') => {
                    match self.chart_mode {
                        ChartMode::Live => {
                            let now = chrono::Utc::now().timestamp_millis();
                            self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                        }
                        ChartMode::Paused { .. } => {
                            self.chart_mode = ChartMode::Live;
                            self.log_scroll = 0;
                            self.log_viewer_cursor = None;
                            self.tooltip = None;
                        }
                    }
                    return;
                }
                KeyCode::Tab => {
                    self.log_focus = LogFocus::Chart;
                    return;
                }
                KeyCode::BackTab => {
                    self.log_focus = LogFocus::Filter;
                    return;
                }
                KeyCode::Enter => {
                    if self.log_viewer_cursor.is_some() {
                        if let Some(idx) = self.cursor_index() {
                            self.open_log_detail(idx);
                        }
                    }
                    return;
                }
                KeyCode::Up => {
                    self.move_cursor_up();
                    self.ensure_cursor_visible();
                    if self.log_detail_open {
                        self.update_log_detail_from_cursor();
                    }
                    return;
                }
                KeyCode::Down => {
                    self.move_cursor_down();
                    self.ensure_cursor_visible();
                    if self.log_detail_open {
                        self.update_log_detail_from_cursor();
                    }
                    return;
                }
                KeyCode::PageUp => {
                    let page = self.log_viewer_area.map(|a| a.height as usize).unwrap_or(20);
                    if matches!(self.chart_mode, ChartMode::Live) {
                        let now = chrono::Utc::now().timestamp_millis();
                        self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                    }
                    self.move_cursor_up_by(page);
                    self.ensure_cursor_visible();
                    return;
                }
                KeyCode::PageDown => {
                    let page = self.log_viewer_area.map(|a| a.height as usize).unwrap_or(20);
                    self.move_cursor_down_by(page);
                    self.ensure_cursor_visible();
                    return;
                }
                KeyCode::Char('+') | KeyCode::Char('=') => {
                    self.max_log_entries = (self.max_log_entries + 1000).min(100_000);
                    return;
                }
                KeyCode::Char('-') => {
                    self.max_log_entries = self.max_log_entries.saturating_sub(1000).max(1000);
                    return;
                }
                KeyCode::Char('0') => {
                    self.chart_window = Duration::from_secs(300);
                    self.chart_mode = ChartMode::Live;
                    self.log_scroll = 0;
                    self.tooltip = None;
                    return;
                }
                KeyCode::Esc => {
                    self.log_focus = LogFocus::Chart;
                    return;
                }
                KeyCode::Left => {
                    self.switch_tab(Tab::Dashboard);
                    return;
                }
                KeyCode::Right => {
                    self.switch_tab(Tab::Detail);
                    return;
                }
                // Fall through for common handlers
                _ => {}
            }
        }

        // Logs tab with detail panel focus: dedicated controls
        if self.tab == Tab::Logs && self.log_focus == LogFocus::Detail {
            match key.code {
                KeyCode::Esc => {
                    self.log_detail_open = false;
                    self.log_detail_entry = None;
                    self.log_focus = LogFocus::Viewer;
                    return;
                }
                KeyCode::Tab => {
                    let count = self.log_detail_copyable_count.max(1);
                    self.log_detail_selected = (self.log_detail_selected + 1) % count;
                    return;
                }
                KeyCode::BackTab => {
                    let count = self.log_detail_copyable_count.max(1);
                    if self.log_detail_selected == 0 {
                        self.log_detail_selected = count - 1;
                    } else {
                        self.log_detail_selected -= 1;
                    }
                    return;
                }
                KeyCode::Char('c') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.copy_detail_selected();
                    return;
                }
                KeyCode::Char('v') => {
                    self.view_log_in_context();
                    return;
                }
                KeyCode::Up => {
                    self.move_cursor_up();
                    self.ensure_cursor_visible();
                    self.update_log_detail_from_cursor();
                    return;
                }
                KeyCode::Down => {
                    self.move_cursor_down();
                    self.ensure_cursor_visible();
                    self.update_log_detail_from_cursor();
                    return;
                }
                KeyCode::PageUp => {
                    let page = self.log_detail_panel_area.map(|a| a.height as usize).unwrap_or(10);
                    self.log_detail_scroll = self.log_detail_scroll.saturating_add(page);
                    return;
                }
                KeyCode::PageDown => {
                    let page = self.log_detail_panel_area.map(|a| a.height as usize).unwrap_or(10);
                    self.log_detail_scroll = self.log_detail_scroll.saturating_sub(page);
                    return;
                }
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.should_quit = true;
                    return;
                }
                _ => { return; }
            }
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('?') => {
                self.overlay = Overlay::Help;
            }

            // Tab navigation: ←→ or H/L (when not conflicting)
            KeyCode::Left => {
                self.switch_tab(match self.tab {
                    Tab::Dashboard => Tab::Detail,
                    Tab::Logs => Tab::Dashboard,
                    Tab::Detail => Tab::Logs,
                });
            }
            KeyCode::Right => {
                self.switch_tab(match self.tab {
                    Tab::Dashboard => Tab::Logs,
                    Tab::Logs => Tab::Detail,
                    Tab::Detail => Tab::Dashboard,
                });
            }
            KeyCode::Tab => {
                self.switch_tab(match self.tab {
                    Tab::Dashboard => Tab::Logs,
                    Tab::Logs => Tab::Detail,
                    Tab::Detail => Tab::Dashboard,
                });
            }
            KeyCode::BackTab => {
                self.switch_tab(match self.tab {
                    Tab::Dashboard => Tab::Detail,
                    Tab::Logs => Tab::Dashboard,
                    Tab::Detail => Tab::Logs,
                });
            }

            // +/- : Logs tab → adjust buffer size, other tabs → adjust refresh rate
            KeyCode::Char('+') | KeyCode::Char('=') => {
                if self.tab == Tab::Logs {
                    self.max_log_entries = (self.max_log_entries + 1000).min(100_000);
                } else {
                    let secs = self.interval.as_secs();
                    if secs > 1 {
                        self.interval = Duration::from_secs(secs - 1);
                    }
                }
            }
            KeyCode::Char('-') => {
                if self.tab == Tab::Logs {
                    self.max_log_entries = self.max_log_entries.saturating_sub(1000).max(1000);
                } else {
                    let secs = self.interval.as_secs();
                    if secs < 30 {
                        self.interval = Duration::from_secs(secs + 1);
                    }
                }
            }

            // Toggle pause/resume (affects both charts and log follow)
            KeyCode::Char('p') => {
                match self.chart_mode {
                    ChartMode::Live => {
                        let now = chrono::Utc::now().timestamp_millis();
                        self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                    }
                    ChartMode::Paused { .. } => {
                        self.chart_mode = ChartMode::Live;
                        self.log_scroll = 0;
                        self.tooltip = None;
                    }
                }
            }
            KeyCode::Char('0') => {
                self.chart_window = Duration::from_secs(300);
                self.chart_mode = ChartMode::Live;
                self.log_scroll = 0;
                self.log_chart_cursor = None;
                self.log_chart_selection = None;
                self.tooltip = None;
            }

            // Service selection
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                    self.history_scroll = 0;
                }
            }
            KeyCode::Down => {
                let max = self.services.len();
                if self.selected < max {
                    self.selected += 1;
                    self.history_scroll = 0;
                }
            }

            // Select/Unselect All (Ctrl+A)
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.toggled_services.len() == self.services.len() {
                    // All selected → unselect all
                    self.toggled_services.clear();
                } else {
                    // Select all services
                    self.toggled_services = self.services.iter().cloned().collect();
                }
                self.focused_service = None;
            }

            // Multi-select toggle
            KeyCode::Char(' ') => {
                if self.selected == 0 {
                    self.toggled_services.clear();
                } else if let Some(svc) = self.services.get(self.selected - 1).cloned() {
                    if self.toggled_services.contains(&svc) {
                        self.toggled_services.remove(&svc);
                    } else {
                        self.toggled_services.insert(svc);
                    }
                    self.focused_service = None;
                }
            }

            // Focus
            KeyCode::Enter => {
                if self.selected == 0 {
                    self.focused_service = None;
                    self.toggled_services.clear();
                } else if let Some(svc) = self.services.get(self.selected - 1) {
                    if self.focused_service.as_deref() == Some(svc.as_str()) {
                        self.focused_service = None;
                    } else {
                        self.focused_service = Some(svc.clone());
                        self.toggled_services.clear();
                    }
                }
            }

            // Escape
            KeyCode::Esc => {
                if self.focused_service.is_some() {
                    self.focused_service = None;
                    self.selected = 0;
                } else if !self.toggled_services.is_empty() {
                    self.toggled_services.clear();
                } else {
                    self.should_quit = true;
                }
            }

            // Service management: start
            KeyCode::Char('s') => {
                if let Some(targets) = self.action_target_services() {
                    let label = if targets.is_empty() {
                        "all services".to_string()
                    } else {
                        targets.join(", ")
                    };
                    self.pending_actions
                        .push(AppAction::StartServices(targets));
                    self.set_feedback(
                        format!("Starting {}...", label),
                        FeedbackKind::Info,
                    );
                }
            }

            // Service management: stop (with confirmation)
            KeyCode::Char('S') => {
                if let Some(targets) = self.action_target_services() {
                    let is_all = self.is_all_action();
                    let msg = if is_all {
                        "Stop all services?".to_string()
                    } else if targets.len() == 1 {
                        format!("Stop service \"{}\"?", targets[0])
                    } else {
                        format!("Stop {} services?", targets.len())
                    };
                    let action = if is_all {
                        AppAction::StopAll
                    } else if targets.len() == 1 {
                        AppAction::StopService(targets[0].clone())
                    } else {
                        AppAction::StopAll
                    };
                    self.confirmation = Some(ConfirmDialog {
                        title: "Confirm Stop".to_string(),
                        message: msg,
                        action,
                        selected: 1, // default to Cancel
                    });
                }
            }

            // Service management: restart (with confirmation)
            KeyCode::Char('r') => {
                if let Some(targets) = self.action_target_services() {
                    let msg = if targets.is_empty() {
                        "Restart all services?".to_string()
                    } else if targets.len() == 1 {
                        format!("Restart service \"{}\"?", targets[0])
                    } else {
                        format!("Restart {} services?", targets.len())
                    };
                    self.confirmation = Some(ConfirmDialog {
                        title: "Confirm Restart".to_string(),
                        message: msg,
                        action: AppAction::RestartServices(targets),
                        selected: 1,
                    });
                }
            }

            _ => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        // Handle overlay clicks first
        match &self.overlay {
            Overlay::Help => {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.overlay = Overlay::None;
                }
                return;
            }
            Overlay::ContextMenu { .. } => {
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Overlay::ContextMenu { service, selected, .. } = &self.overlay {
                            let service = service.clone();
                            let sel = *selected;
                            self.overlay = Overlay::None;
                            self.execute_menu_action(&service, sel);
                        }
                    }
                    MouseEventKind::Moved => {
                        if let Overlay::ContextMenu { y, selected, .. } = &mut self.overlay {
                            let row = mouse.row.saturating_sub(*y + 1) as usize;
                            if row < MENU_ITEMS.len() {
                                *selected = row;
                            }
                        }
                    }
                    _ => {}
                }
                return;
            }
            Overlay::None => {}
        }

        // Handle confirmation dialog clicks
        if self.confirmation.is_some() {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                // Check if a button was clicked
                let mut clicked_button = None;
                for &(idx, rect) in &self.confirm_button_areas {
                    if mouse.column >= rect.x
                        && mouse.column < rect.x + rect.width
                        && mouse.row >= rect.y
                        && mouse.row < rect.y + rect.height
                    {
                        clicked_button = Some(idx);
                        break;
                    }
                }
                match clicked_button {
                    Some(0) => {
                        // Confirm
                        if let Some(confirm) = self.confirmation.take() {
                            self.pending_actions.push(confirm.action);
                        }
                    }
                    Some(1) => {
                        // Cancel
                        self.confirmation = None;
                    }
                    _ => {
                        // Click outside buttons — dismiss
                        self.confirmation = None;
                    }
                }
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.tooltip = None;

                // Check live indicator click
                if let Some(indicator_area) = self.live_indicator_area {
                    if mouse.column >= indicator_area.x
                        && mouse.column < indicator_area.x + indicator_area.width
                        && mouse.row >= indicator_area.y
                        && mouse.row < indicator_area.y + indicator_area.height
                    {
                        match self.chart_mode {
                            ChartMode::Live => {
                                let now = chrono::Utc::now().timestamp_millis();
                                self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                            }
                            ChartMode::Paused { .. } => {
                                self.chart_mode = ChartMode::Live;
                                self.log_scroll = 0;
                            }
                        }
                        return;
                    }
                }

                // Check tab bar click
                for &(tab, rect) in &self.tab_areas {
                    if mouse.column >= rect.x
                        && mouse.column < rect.x + rect.width
                        && mouse.row >= rect.y
                        && mouse.row < rect.y + rect.height
                    {
                        self.switch_tab(tab);
                        return;
                    }
                }

                // Check detail action button clicks
                if self.tab == Tab::Detail {
                    for (action, rect) in &self.detail_button_areas {
                        if mouse.column >= rect.x
                            && mouse.column < rect.x + rect.width
                            && mouse.row >= rect.y
                            && mouse.row < rect.y + rect.height
                        {
                            let action = action.clone();
                            match &action {
                                AppAction::StopService(_) | AppAction::StopAll => {
                                    self.confirmation = Some(ConfirmDialog {
                                        title: "Confirm".to_string(),
                                        message: format!("{:?}", action),
                                        action: action.clone(),
                                        selected: 1,
                                    });
                                }
                                _ => {
                                    self.pending_actions.push(action);
                                }
                            }
                            return;
                        }
                    }
                }

                // Check sidebar border drag (now vertical)
                if let Some(border_x) = self.pane_border_y {
                    if mouse.column >= border_x.saturating_sub(1)
                        && mouse.column <= border_x + 1
                    {
                        self.dragging_border = true;
                        return;
                    }
                }

                // Check chart area click (start potential zoom selection) — Dashboard and Logs
                if self.tab == Tab::Dashboard || self.tab == Tab::Logs {
                    if let Some(chart_kind) = self.hit_test_chart(mouse.column, mouse.row) {
                        if chart_kind == ChartKind::LogLevel {
                            self.log_focus = LogFocus::Chart;

                            // Detect double-click: zoom into bar
                            let now = std::time::Instant::now();
                            let is_double_click = self.last_click
                                .map(|(col, row, t)| col == mouse.column && row == mouse.row && now.duration_since(t).as_millis() < 400)
                                .unwrap_or(false);
                            self.last_click = Some((mouse.column, mouse.row, now));

                            if is_double_click {
                                if let Some(data_area) = self.log_chart_data_area() {
                                    if mouse.column >= data_area.x && mouse.column < data_area.x + data_area.width {
                                        let cursor = mouse.column - data_area.x;
                                        let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_area.width);
                                        let col = cursor as i64;
                                        let t_left = snapped_left + col * bucket_ms;
                                        let t_right = t_left + bucket_ms;
                                        let new_window = (t_right - t_left).max(5000);
                                        self.chart_window = Duration::from_millis(new_window as u64);
                                        self.chart_mode = ChartMode::Paused { right_edge_ms: t_right };
                                        self.log_chart_cursor = None;
                                        self.log_chart_selection = None;
                                    }
                                }
                                return;
                            }

                            // Single click: place cursor at clicked column
                            if let Some(data_area) = self.log_chart_data_area() {
                                if mouse.column >= data_area.x && mouse.column < data_area.x + data_area.width {
                                    let col = mouse.column - data_area.x;
                                    self.log_chart_cursor = Some(self.log_chart_col_to_ts(col, data_area.width));
                                }
                            }
                        } else {
                            self.last_click = Some((mouse.column, mouse.row, std::time::Instant::now()));
                        }
                        self.zoom_state = Some(ZoomState {
                            chart_kind,
                            start_col: mouse.column,
                            current_col: mouse.column,
                            is_real_drag: false,
                        });
                        return;
                    }
                }

                // Check click on filter area — focus filter
                if self.tab == Tab::Logs {
                    if let Some(filter_area) = self.log_filter_area {
                        if mouse.column >= filter_area.x
                            && mouse.column < filter_area.x + filter_area.width
                            && mouse.row >= filter_area.y
                            && mouse.row < filter_area.y + filter_area.height
                        {
                            self.log_focus = LogFocus::Filter;
                            return;
                        }
                    }
                }

                // Check click on log detail panel area — focus detail + select/copy
                if self.tab == Tab::Logs && self.log_detail_open {
                    if let Some(panel_area) = self.log_detail_panel_area {
                        if mouse.column >= panel_area.x
                            && mouse.column < panel_area.x + panel_area.width
                            && mouse.row >= panel_area.y
                            && mouse.row < panel_area.y + panel_area.height
                        {
                            self.log_focus = LogFocus::Detail;
                            // Check if click hits a copyable area
                            let mut clicked_idx = None;
                            for (rect, idx) in &self.log_detail_click_areas {
                                if mouse.column >= rect.x
                                    && mouse.column < rect.x + rect.width
                                    && mouse.row >= rect.y
                                    && mouse.row < rect.y + rect.height
                                {
                                    clicked_idx = Some(*idx);
                                    break;
                                }
                            }
                            if let Some(idx) = clicked_idx {
                                self.log_detail_selected = idx;
                                self.copy_detail_selected();
                            }
                            return;
                        }
                    }
                }

                // Check click on log viewer area — focus viewer + set cursor
                if self.tab == Tab::Logs {
                    if let Some(viewer_area) = self.log_viewer_area {
                        if mouse.column >= viewer_area.x
                            && mouse.column < viewer_area.x + viewer_area.width
                            && mouse.row >= viewer_area.y
                            && mouse.row < viewer_area.y + viewer_area.height
                        {
                            self.log_focus = LogFocus::Viewer;
                            // Compute clicked entry index
                            let row_in_viewer = (mouse.row - viewer_area.y) as usize;
                            let filtered = self.log_filtered_entries();
                            let total = filtered.len();
                            let inner_height = viewer_area.height as usize;
                            let is_following = matches!(self.chart_mode, ChartMode::Live) && self.log_viewer_cursor.is_none();
                            let max_scroll = total.saturating_sub(inner_height);
                            let scroll = if is_following { 0 } else { self.log_scroll.min(max_scroll) };
                            let skip = total.saturating_sub(inner_height + scroll);
                            let clicked_index = skip + row_in_viewer;
                            if clicked_index < total {
                                // Double-click detection
                                let now_inst = std::time::Instant::now();
                                let is_double = self.last_click
                                    .map(|(c, r, t)| {
                                        c == mouse.column && r == mouse.row
                                            && now_inst.duration_since(t).as_millis() < 400
                                    })
                                    .unwrap_or(false);
                                self.log_viewer_cursor = Some(filtered[clicked_index].id);
                                if is_double {
                                    self.open_log_detail(clicked_index);
                                }
                            }
                        }
                    }
                }

                // Check service table click
                if let Some(table_area) = self.service_table_area {
                    if mouse.column >= table_area.x
                        && mouse.column < table_area.x + table_area.width
                        && mouse.row >= table_area.y
                        && mouse.row < table_area.y + table_area.height
                    {
                        // Account for border (1) + header row (1)
                        let row = (mouse.row - table_area.y).saturating_sub(2) as usize;
                        let max = self.services.len();
                        if row <= max {
                            // Ctrl+Click: toggle multi-select
                            if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                                if row >= 1 {
                                    if let Some(svc) = self.services.get(row - 1) {
                                        let svc = svc.clone();
                                        if self.toggled_services.contains(&svc) {
                                            self.toggled_services.remove(&svc);
                                        } else {
                                            self.toggled_services.insert(svc);
                                        }
                                        self.selected = row;
                                    }
                                } else {
                                    // Ctrl+Click on ALL: clear multi-select
                                    self.toggled_services.clear();
                                    self.selected = 0;
                                }
                                return;
                            }

                            // Single click: select service directly
                            self.selected = row;
                        }
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                // Middle click on chart: start pan drag — Dashboard and Logs
                if self.tab == Tab::Dashboard || self.tab == Tab::Logs {
                    if let Some(chart_kind) = self.hit_test_chart(mouse.column, mouse.row) {
                        let right_edge = self.current_right_edge_ms();
                        self.drag_state = Some(DragState {
                            chart_kind,
                            start_col: mouse.column,
                            initial_right_edge_ms: right_edge,
                            is_real_drag: false,
                        });
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(table_area) = self.service_table_area {
                    if mouse.column >= table_area.x
                        && mouse.column < table_area.x + table_area.width
                        && mouse.row >= table_area.y
                        && mouse.row < table_area.y + table_area.height
                    {
                        let row = (mouse.row - table_area.y).saturating_sub(2) as usize;
                        if row >= 1 && row <= self.services.len() {
                            let service = self.services[row - 1].clone();
                            self.overlay = Overlay::ContextMenu {
                                service,
                                x: mouse.column,
                                y: mouse.row,
                                selected: 0,
                            };
                        }
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging_border = false;
                if let Some(zoom) = self.zoom_state.take() {
                    if zoom.is_real_drag {
                        // Complete zoom: compute time range from pixel range
                        let chart_area = match zoom.chart_kind {
                            ChartKind::Cpu => self.cpu_chart_area,
                            ChartKind::Memory => self.memory_chart_area,
                            ChartKind::LogLevel => self.log_chart_area,
                        };
                        if let Some(area) = chart_area {
                            let col_min = zoom.start_col.min(zoom.current_col);
                            let col_max = zoom.start_col.max(zoom.current_col);

                            if zoom.chart_kind == ChartKind::LogLevel {
                                // Use snapped bucket boundaries for log chart
                                if let Some(data_area) = self.log_chart_data_area() {
                                    let (snapped_left, bucket_ms) = self.log_chart_snapped_bounds(data_area.width);
                                    let c_min = col_min.saturating_sub(data_area.x) as i64;
                                    let c_max = col_max.saturating_sub(data_area.x) as i64;
                                    let t_left = snapped_left + c_min * bucket_ms;
                                    let t_right = snapped_left + (c_max + 1) * bucket_ms;
                                    let new_window = (t_right - t_left).max(5000);
                                    self.chart_window = Duration::from_millis(new_window as u64);
                                    self.chart_mode = ChartMode::Paused { right_edge_ms: t_right };
                                }
                            } else {
                                let data_width = (area.width as f64 - 10.0).max(1.0);
                                let data_x_start = area.x + 8; // approx left margin
                                let window_ms = self.chart_window.as_millis() as f64;
                                let right_edge = self.current_right_edge_ms() as f64;
                                let left_edge = right_edge - window_ms;

                                let t_left = left_edge + ((col_min.saturating_sub(data_x_start)) as f64 / data_width) * window_ms;
                                let t_right = left_edge + ((col_max.saturating_sub(data_x_start)) as f64 / data_width) * window_ms;

                                let new_window = (t_right - t_left).max(5000.0);
                                self.chart_window = Duration::from_millis(new_window as u64);
                                self.chart_mode = ChartMode::Paused { right_edge_ms: t_right as i64 };
                            }
                        }
                    } else {
                        // Just a click — show tooltip
                        self.show_tooltip_at(mouse.column, mouse.row, zoom.chart_kind);
                    }
                }
            }
            MouseEventKind::Up(MouseButton::Middle) => {
                if self.drag_state.is_some() {
                    self.drag_state = None;
                    // Re-fetch logs for the new time window after panning
                    self.log_last_id = None;
                    self.log_entries.clear();
                    self.log_scroll = 0;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.dragging_border {
                    // Horizontal border drag (sidebar width)
                    if let Some(table_area) = self.service_table_area {
                        let total_width = if let Some(content_area) = self.content_area {
                            (content_area.x + content_area.width) - table_area.x
                        } else {
                            table_area.width
                        };
                        if total_width > 0 {
                            let ratio = (mouse.column - table_area.x) as f32 / total_width as f32;
                            self.sidebar_width_ratio = ratio.clamp(0.15, 0.45);
                        }
                    }
                } else if let Some(ref mut zoom) = self.zoom_state {
                    // Left drag on chart: zoom selection
                    let dx = mouse.column as i32 - zoom.start_col as i32;
                    if dx.abs() >= 3 {
                        zoom.is_real_drag = true;
                    }
                    zoom.current_col = mouse.column;
                }
            }
            MouseEventKind::Drag(MouseButton::Middle) => {
                // Middle drag on chart: pan
                if let Some(ref mut drag) = self.drag_state {
                    let dx = mouse.column as i32 - drag.start_col as i32;
                    if dx.abs() >= 3 {
                        drag.is_real_drag = true;
                    }
                    if drag.is_real_drag {
                        let chart_area = match drag.chart_kind {
                            ChartKind::Cpu => self.cpu_chart_area,
                            ChartKind::Memory => self.memory_chart_area,
                            ChartKind::LogLevel => self.log_chart_area,
                        };
                        if let Some(area) = chart_area {
                            let data_width = (area.width as i32 - 10).max(1);
                            let window_ms = self.chart_window.as_millis() as i64;
                            let time_delta = (-dx as i64 * window_ms) / data_width as i64;
                            let now_ms = chrono::Utc::now().timestamp_millis();
                            let new_edge = (drag.initial_right_edge_ms + time_delta).min(now_ms);
                            self.chart_mode = ChartMode::Paused { right_edge_ms: new_edge };
                        }
                    }
                }
            }
            MouseEventKind::ScrollUp => {
                // Scroll in detail panel
                if self.log_detail_open {
                    if let Some(panel_area) = self.log_detail_panel_area {
                        if mouse.column >= panel_area.x
                            && mouse.column < panel_area.x + panel_area.width
                            && mouse.row >= panel_area.y
                            && mouse.row < panel_area.y + panel_area.height
                        {
                            self.log_detail_scroll = self.log_detail_scroll.saturating_add(3);
                            return;
                        }
                    }
                }
                if let Some(table_area) = self.service_table_area {
                    if mouse.column >= table_area.x
                        && mouse.column < table_area.x + table_area.width
                        && mouse.row >= table_area.y
                        && mouse.row < table_area.y + table_area.height
                    {
                        // Only scroll when rows exceed visible area (border + header = 3 rows overhead)
                        let visible_rows = table_area.height.saturating_sub(3) as usize;
                        let total_rows = self.services.len() + 1; // +1 for ALL row
                        if total_rows > visible_rows {
                            let max_offset = total_rows.saturating_sub(visible_rows);
                            self.service_table_offset = self.service_table_offset.min(max_offset).saturating_sub(1);
                        } else {
                            self.service_table_offset = 0;
                        }
                        return;
                    }
                }
                if let Some(content_area) = self.content_area {
                    if mouse.column >= content_area.x
                        && mouse.column < content_area.x + content_area.width
                        && mouse.row >= content_area.y
                        && mouse.row < content_area.y + content_area.height
                    {
                        match self.tab {
                            Tab::Logs => {
                                // Check if over log viewer area first
                                if let Some(viewer_area) = self.log_viewer_area {
                                    if mouse.row >= viewer_area.y
                                        && mouse.row < viewer_area.y + viewer_area.height
                                    {
                                        self.log_scroll = self.log_scroll.saturating_add(3);
                                        if matches!(self.chart_mode, ChartMode::Live) {
                                            let now = chrono::Utc::now().timestamp_millis();
                                            self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                                        }
                                        return;
                                    }
                                }
                                // Over log chart area → zoom in
                                if let Some(chart_area) = self.log_chart_area {
                                    if mouse.row >= chart_area.y
                                        && mouse.row < chart_area.y + chart_area.height
                                    {
                                        let secs = self.chart_window.as_secs();
                                        if secs > 10 {
                                            self.chart_window = Duration::from_secs(secs.saturating_sub(10).max(10));
                                    
                                        }
                                        return;
                                    }
                                }
                                // Fallback: scroll logs
                                self.log_scroll = self.log_scroll.saturating_add(3);
                                if matches!(self.chart_mode, ChartMode::Live) {
                                    let now = chrono::Utc::now().timestamp_millis();
                                    self.chart_mode = ChartMode::Paused { right_edge_ms: now };
                                }
                            }
                            Tab::Dashboard => {
                                if let Some(chart_area) = self.chart_area {
                                    if mouse.row >= chart_area.y
                                        && mouse.row < chart_area.y + chart_area.height
                                    {
                                        let secs = self.chart_window.as_secs();
                                        if secs > 10 {
                                            self.chart_window = Duration::from_secs(secs.saturating_sub(10).max(10));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        return;
                    }
                }
            }
            MouseEventKind::ScrollDown => {
                // Scroll in detail panel
                if self.log_detail_open {
                    if let Some(panel_area) = self.log_detail_panel_area {
                        if mouse.column >= panel_area.x
                            && mouse.column < panel_area.x + panel_area.width
                            && mouse.row >= panel_area.y
                            && mouse.row < panel_area.y + panel_area.height
                        {
                            self.log_detail_scroll = self.log_detail_scroll.saturating_sub(3);
                            return;
                        }
                    }
                }
                if let Some(table_area) = self.service_table_area {
                    if mouse.column >= table_area.x
                        && mouse.column < table_area.x + table_area.width
                        && mouse.row >= table_area.y
                        && mouse.row < table_area.y + table_area.height
                    {
                        // Only scroll when rows exceed visible area
                        let visible_rows = table_area.height.saturating_sub(3) as usize;
                        let total_rows = self.services.len() + 1; // +1 for ALL row
                        if total_rows > visible_rows {
                            let max_offset = total_rows.saturating_sub(visible_rows);
                            self.service_table_offset = (self.service_table_offset + 1).min(max_offset);
                        }
                        return;
                    }
                }
                if let Some(content_area) = self.content_area {
                    if mouse.column >= content_area.x
                        && mouse.column < content_area.x + content_area.width
                        && mouse.row >= content_area.y
                        && mouse.row < content_area.y + content_area.height
                    {
                        match self.tab {
                            Tab::Logs => {
                                // Check if over log viewer area first
                                if let Some(viewer_area) = self.log_viewer_area {
                                    if mouse.row >= viewer_area.y
                                        && mouse.row < viewer_area.y + viewer_area.height
                                    {
                                        self.log_scroll = self.log_scroll.saturating_sub(3);
                                        return;
                                    }
                                }
                                // Over log chart area → zoom out
                                if let Some(chart_area) = self.log_chart_area {
                                    if mouse.row >= chart_area.y
                                        && mouse.row < chart_area.y + chart_area.height
                                    {
                                        let secs = self.chart_window.as_secs();
                                        if secs < 3600 {
                                            self.chart_window = Duration::from_secs((secs + 10).min(3600));
                                    
                                        }
                                        return;
                                    }
                                }
                                // Fallback: scroll logs
                                self.log_scroll = self.log_scroll.saturating_sub(3);
                            }
                            Tab::Dashboard => {
                                if let Some(chart_area) = self.chart_area {
                                    if mouse.row >= chart_area.y
                                        && mouse.row < chart_area.y + chart_area.height
                                    {
                                        let secs = self.chart_window.as_secs();
                                        if secs < 3600 {
                                            self.chart_window = Duration::from_secs((secs + 10).min(3600));
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        return;
                    }
                }
            }
            _ => {}
        }
    }

    fn execute_menu_action(&mut self, service: &str, action: usize) {
        match action {
            0 => {
                // Focus
                if self.focused_service.as_deref() == Some(service) {
                    self.focused_service = None;
                } else {
                    self.focused_service = Some(service.to_string());
                }
            }
            1 => {
                // Start service
                self.pending_actions
                    .push(AppAction::StartServices(vec![service.to_string()]));
                self.set_feedback(
                    format!("Starting {}...", service),
                    FeedbackKind::Info,
                );
            }
            2 => {
                // Stop service
                self.confirmation = Some(ConfirmDialog {
                    title: "Confirm Stop".to_string(),
                    message: format!("Stop service \"{}\"?", service),
                    action: AppAction::StopService(service.to_string()),
                    selected: 1,
                });
            }
            3 => {
                // Restart service
                self.confirmation = Some(ConfirmDialog {
                    title: "Confirm Restart".to_string(),
                    message: format!("Restart service \"{}\"?", service),
                    action: AppAction::RestartServices(vec![service.to_string()]),
                    selected: 1,
                });
            }
            _ => {}
        }
    }

    pub fn update_metrics(&mut self, entries: Vec<MonitorMetricEntry>) {
        for entry in entries {
            self.latest.insert(entry.service.clone(), entry.clone());

            let history = self
                .history
                .entry(entry.service.clone())
                .or_insert_with(VecDeque::new);
            history.push_back(entry.clone());
            while history.len() > MAX_HISTORY_POINTS {
                history.pop_front();
            }

            if !self.services.contains(&entry.service) {
                self.services.push(entry.service.clone());
                self.services.sort();
            }
        }
    }

    pub fn append_log_entries(&mut self, entries: Vec<DisplayLogEntry>) {
        for entry in entries {
            self.log_entries.push_back(entry);
        }
        while self.log_entries.len() > self.max_log_entries {
            self.log_entries.pop_front();
        }
    }

    pub fn compute_totals(&self) -> MonitorMetricEntry {
        let mut total = MonitorMetricEntry {
            timestamp: 0,
            service: "ALL".to_string(),
            cpu_percent: 0.0,
            memory_rss: 0,
            memory_vss: 0,
            pids: Vec::new(),
        };
        for entry in self.latest.values() {
            total.cpu_percent += entry.cpu_percent;
            total.memory_rss += entry.memory_rss;
            total.memory_vss += entry.memory_vss;
            total.pids.extend_from_slice(&entry.pids);
            if entry.timestamp > total.timestamp {
                total.timestamp = entry.timestamp;
            }
        }
        total
    }

    pub fn compute_total_history(&self) -> VecDeque<MonitorMetricEntry> {
        let mut timestamps: Vec<i64> = Vec::new();
        for history in self.history.values() {
            for entry in history {
                if !timestamps.contains(&entry.timestamp) {
                    timestamps.push(entry.timestamp);
                }
            }
        }
        timestamps.sort();

        let mut result = VecDeque::new();
        for ts in timestamps {
            let mut total = MonitorMetricEntry {
                timestamp: ts,
                service: "Total".to_string(),
                cpu_percent: 0.0,
                memory_rss: 0,
                memory_vss: 0,
                pids: Vec::new(),
            };
            for history in self.history.values() {
                if let Some(entry) = history.iter().find(|e| e.timestamp == ts) {
                    total.cpu_percent += entry.cpu_percent;
                    total.memory_rss += entry.memory_rss;
                    total.memory_vss += entry.memory_vss;
                    total.pids.extend_from_slice(&entry.pids);
                }
            }
            result.push_back(total);
        }
        result
    }
}
