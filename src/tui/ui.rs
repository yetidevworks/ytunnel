use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::{App, Focus, HealthStatus, InputMode, TunnelKind};
use crate::metrics::TunnelMetrics;
use crate::state::TunnelStatus;
use unicode_width::UnicodeWidthChar;

pub fn render(f: &mut Frame, app: &App) {
    // Main layout: tunnels on left, logs/metrics on right, status line, help bar at bottom
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // Content
            Constraint::Length(1), // Status line
            Constraint::Length(1), // Help bar
        ])
        .split(f.area());

    let content_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(main_chunks[0]);

    // Render tunnels list
    render_tunnels(f, app, content_chunks[0]);

    // Right panel: details (fixed), logs (flexible), and optional metrics (fixed)
    let has_metrics = app.selected_metrics().is_some();
    let has_details = app.selected_tunnel_details().is_some();

    if has_details && has_metrics {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4), // Details panel (fixed)
                Constraint::Min(0),    // Logs panel (flexible)
                Constraint::Length(6), // Metrics panel (fixed)
            ])
            .split(content_chunks[1]);

        render_details(f, app, right_chunks[0]);
        render_logs(f, app, right_chunks[1]);
        render_metrics(
            f,
            app.selected_metrics(),
            &app.selected_sparkline(),
            app.selected_health(),
            right_chunks[2],
        );
    } else if has_details {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4), // Details panel (fixed)
                Constraint::Min(0),    // Logs panel (flexible)
            ])
            .split(content_chunks[1]);

        render_details(f, app, right_chunks[0]);
        render_logs(f, app, right_chunks[1]);
    } else if has_metrics {
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(6)])
            .split(content_chunks[1]);

        render_logs(f, app, right_chunks[0]);
        render_metrics(
            f,
            app.selected_metrics(),
            &app.selected_sparkline(),
            app.selected_health(),
            right_chunks[1],
        );
    } else {
        // Just render logs panel
        render_logs(f, app, content_chunks[1]);
    }

    // Render status line
    render_status_line(f, app, main_chunks[1]);

    // Render help bar
    render_help_bar(f, app, main_chunks[2]);

    // Render modals/dialogs on top
    match app.input_mode {
        InputMode::AddName => render_add_dialog(f, "Enter tunnel name:", &app.input, false),
        InputMode::AddTarget => render_add_dialog(
            f,
            "Enter target (e.g., localhost:3000):",
            &app.input,
            app.is_importing,
        ),
        InputMode::AddZone => render_zone_dialog(f, app),
        InputMode::EditTarget => render_edit_dialog(f, app, "Edit target URL:"),
        InputMode::EditZone => render_edit_zone_dialog(f, app),
        InputMode::Confirm => {
            if let Some(ref msg) = app.confirm_message {
                render_confirm_dialog(f, msg);
            }
        }
        InputMode::Help => render_help_modal(f),
        InputMode::Normal => {}
    }
}

fn render_help_modal(f: &mut Frame) {
    let help_text = vec![
        Line::from(Span::styled(
            "NAVIGATION",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Tab      ", Style::default().fg(Color::Cyan)),
            Span::raw("Switch focus between tunnel list and log pane"),
        ]),
        Line::from(vec![
            Span::styled("  ↑/k      ", Style::default().fg(Color::Cyan)),
            Span::raw("Move selection up (or scroll logs when log pane focused)"),
        ]),
        Line::from(vec![
            Span::styled("  ↓/j      ", Style::default().fg(Color::Cyan)),
            Span::raw("Move selection down (or scroll logs when log pane focused)"),
        ]),
        Line::from(vec![
            Span::styled("  PgUp/Dn  ", Style::default().fg(Color::Cyan)),
            Span::raw("Scroll logs a page (Ctrl+U/Ctrl+D: half a page)"),
        ]),
        Line::from(vec![
            Span::styled("  Home/End ", Style::default().fg(Color::Cyan)),
            Span::raw("Oldest / newest log line (End resumes following)"),
        ]),
        Line::from(vec![
            Span::styled("  Mouse    ", Style::default().fg(Color::Cyan)),
            Span::raw("Click a pane or tunnel; wheel scrolls the pane under the pointer"),
        ]),
        Line::from(vec![
            Span::styled("           ", Style::default().fg(Color::Cyan)),
            Span::raw("Shift+drag to select text (Option+drag in Terminal.app)"),
        ]),
        Line::from(vec![
            Span::styled("  q        ", Style::default().fg(Color::Cyan)),
            Span::raw("Quit ytunnel"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "TUNNEL MANAGEMENT",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  a        ", Style::default().fg(Color::Cyan)),
            Span::raw("Add a new tunnel"),
        ]),
        Line::from(vec![
            Span::styled("  e        ", Style::default().fg(Color::Cyan)),
            Span::raw("Edit tunnel (target URL and zone)"),
        ]),
        Line::from(vec![
            Span::styled("  s        ", Style::default().fg(Color::Cyan)),
            Span::raw("Start selected tunnel"),
        ]),
        Line::from(vec![
            Span::styled("  S        ", Style::default().fg(Color::Cyan)),
            Span::raw("Stop selected tunnel"),
        ]),
        Line::from(vec![
            Span::styled("  R        ", Style::default().fg(Color::Cyan)),
            Span::raw("Restart tunnel (updates daemon config)"),
        ]),
        Line::from(vec![
            Span::styled("  d        ", Style::default().fg(Color::Cyan)),
            Span::raw("Delete selected tunnel"),
        ]),
        Line::from(vec![
            Span::styled("  m        ", Style::default().fg(Color::Cyan)),
            Span::raw("Import ephemeral tunnel as managed"),
        ]),
        Line::from(vec![
            Span::styled("  A        ", Style::default().fg(Color::Cyan)),
            Span::raw("Toggle auto-start on login (⟳ = enabled)"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "QUICK ACTIONS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  c        ", Style::default().fg(Color::Cyan)),
            Span::raw("Copy tunnel URL to clipboard"),
        ]),
        Line::from(vec![
            Span::styled("  o        ", Style::default().fg(Color::Cyan)),
            Span::raw("Open tunnel URL in browser"),
        ]),
        Line::from(vec![
            Span::styled("  h        ", Style::default().fg(Color::Cyan)),
            Span::raw("Check tunnel health now"),
        ]),
        Line::from(vec![
            Span::styled("  r        ", Style::default().fg(Color::Cyan)),
            Span::raw("Refresh tunnel list and status"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "ACCOUNTS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ;        ", Style::default().fg(Color::Cyan)),
            Span::raw("Cycle through accounts"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "METRICS",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::raw("  Metrics auto-refresh every "),
            Span::styled("5 seconds", Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("  Health checks run every "),
            Span::styled("30 seconds", Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("  A tunnel is marked down after "),
            Span::styled("3 failed checks", Style::default().fg(Color::Green)),
            Span::raw(" in a row"),
        ]),
        Line::from(vec![
            Span::raw("  Failures while cloudflared is "),
            Span::styled("reconnecting", Style::default().fg(Color::Green)),
            Span::raw(" don't count"),
        ]),
        Line::from(vec![
            Span::raw("  System notifications on tunnel "),
            Span::styled("down/up", Style::default().fg(Color::Red)),
        ]),
    ];

    // As tall as the help text (plus borders), so nothing is cut off on a
    // normal-sized terminal; a smaller one clips as before
    let width_area = centered_rect(70, 100, f.area());
    let height = (help_text.len() as u16 + 2).min(f.area().height);
    let area = Rect {
        x: width_area.x,
        y: f.area().y + (f.area().height - height) / 2,
        width: width_area.width,
        height,
    };

    // Clear the area
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Help - Press Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let help = Paragraph::new(help_text).wrap(Wrap { trim: false });

    f.render_widget(help, inner);
}

fn render_tunnels(f: &mut Frame, app: &App, area: Rect) {
    app.tunnels_area.set(area);
    // Show account name in title if there are multiple accounts
    let title = if app.demo {
        format!(" Tunnels ({}) [demo] ", app.tunnels.len())
    } else if app.accounts.len() > 1 {
        format!(
            " Tunnels ({}) [{}] ",
            app.tunnels.len(),
            app.current_account_name()
        )
    } else {
        format!(" Tunnels ({}) ", app.tunnels.len())
    };

    let items: Vec<ListItem> = app
        .tunnels
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let (status_color, status_symbol) = match entry.status {
                TunnelStatus::Running => (Color::Green, entry.status.symbol()),
                TunnelStatus::Stopped => (Color::Yellow, entry.status.symbol()),
                TunnelStatus::Error => (Color::Red, entry.status.symbol()),
            };

            let selected = i == app.selected;

            // Base style with optional selection background
            let base_style = if selected {
                Style::default().bg(Color::Rgb(40, 60, 80)) // Subtle blue background
            } else {
                Style::default()
            };

            let name_style = if selected {
                base_style.fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                base_style.fg(Color::Gray)
            };

            // Show tunnels ytunnel doesn't manage with italic
            let (final_name_style, hostname_display) = match entry.kind {
                TunnelKind::Managed => (name_style, entry.tunnel.hostname.clone()),
                TunnelKind::Ephemeral => (
                    name_style.add_modifier(Modifier::ITALIC),
                    format!("{} [ephemeral]", entry.tunnel.name),
                ),
                TunnelKind::External => (
                    name_style.add_modifier(Modifier::ITALIC),
                    "[not managed by ytunnel]".to_string(),
                ),
            };

            let hostname_style = if selected {
                base_style.fg(Color::Rgb(150, 150, 150))
            } else {
                base_style.fg(Color::DarkGray)
            };

            // Auto-start indicator (only for managed tunnels)
            let auto_start_span = if entry.kind == TunnelKind::Managed && entry.tunnel.auto_start {
                Span::styled(" ⟳", base_style.fg(Color::Cyan))
            } else {
                Span::raw("")
            };

            // Health indicator (show warning for unhealthy running tunnels)
            let health_span = if entry.status == TunnelStatus::Running
                && entry.health == HealthStatus::Unhealthy
            {
                Span::styled(" ⚠", base_style.fg(Color::Red))
            } else {
                Span::raw("")
            };

            let line = Line::from(vec![
                Span::styled(format!("{} ", status_symbol), base_style.fg(status_color)),
                Span::styled(format!("{:<12}", entry.tunnel.name), final_name_style),
                Span::styled(hostname_display, hostname_style),
                auto_start_span,
                health_span,
            ]);

            ListItem::new(line).style(base_style)
        })
        .collect();

    let tunnels_border_style = if app.focus == Focus::Tunnels {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let tunnels_list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(tunnels_border_style),
    );

    f.render_widget(tunnels_list, area);
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    app.logs_area.set(area);
    let name = app.tunnels.get(app.selected).map(|e| e.tunnel.name.clone());
    let tail = name.as_deref().and_then(|n| app.log_tails.get(n));

    // What to show: the tail's lines, or a one-line message
    let message = match tail {
        Some(t) if !t.is_empty() => None,
        Some(t) => Some(match &t.error {
            Some(e) => format!("Error reading logs: {}", e),
            None => "No logs yet".to_string(),
        }),
        None => Some("No tunnel selected".to_string()),
    };
    let total = match (&message, tail) {
        (None, Some(t)) => t.len(),
        _ => 1,
    };
    let line_at = |i: usize| -> &str {
        match (&message, tail) {
            (None, Some(t)) => t.line(i),
            (Some(m), _) => m.as_str(),
            _ => "",
        }
    };

    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;

    // Largest useful scroll: the offset at which the oldest line reaches the
    // top of the pane. 0 when everything fits.
    let mut rows = 0;
    let mut first_page_lines = 0;
    while first_page_lines < total && rows < height {
        rows += wrapped_row_count(line_at(first_page_lines), width);
        first_page_lines += 1;
    }
    let max_scroll = if rows < height {
        0
    } else {
        total - first_page_lines
    };
    app.log_max_scroll.set(max_scroll);
    app.log_page.set(height);
    let scroll = app.log_scroll.min(max_scroll);

    // Lines are wrapped here rather than by the Paragraph, so the row count is
    // exact and the newest line is never pushed off the bottom
    let mut visible: Vec<Line> = Vec::with_capacity(height);
    if height > 0 && width > 0 {
        if scroll > 0 && scroll == max_scroll {
            // At the top: start from the first row of the oldest line
            'top: for i in 0..total {
                let line = line_at(i);
                let style = raw_line_style(line);
                for row in wrap_rows(line, width) {
                    if visible.len() == height {
                        break 'top;
                    }
                    visible.push(Line::from(Span::styled(row.to_string(), style)));
                }
            }
        } else {
            // Bottom-anchored: fill upward from the newest line in view
            let bottom = total - 1 - scroll;
            'bottom: for i in (0..=bottom).rev() {
                let line = line_at(i);
                let style = raw_line_style(line);
                for row in wrap_rows(line, width).into_iter().rev() {
                    if visible.len() == height {
                        break 'bottom;
                    }
                    visible.push(Line::from(Span::styled(row.to_string(), style)));
                }
            }
            visible.reverse();
        }
    }

    let name_tag = name
        .as_deref()
        .map(|n| format!(": {}", n))
        .unwrap_or_default();
    let count = if message.is_some() { 0 } else { total };
    let title = if scroll == 0 {
        format!(" Logs{} ({}) ", name_tag, count)
    } else {
        format!(" Logs{} ({} — scrolled, End to follow) ", name_tag, count)
    };
    let logs_border_style = if app.focus == Focus::Logs {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let logs = Paragraph::new(visible).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(logs_border_style)
            .title(title),
    );
    f.render_widget(logs, area);
}

// Split a line into rows of at most `width` display columns (character
// wrapping, like a terminal). An empty line is one empty row.
fn wrap_rows(line: &str, width: usize) -> Vec<&str> {
    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut row_width = 0;
    for (i, c) in line.char_indices() {
        let w = c.width().unwrap_or(0);
        if row_width + w > width && row_width > 0 {
            rows.push(&line[row_start..i]);
            row_start = i;
            row_width = 0;
        }
        row_width += w;
    }
    rows.push(&line[row_start..]);
    rows
}

fn wrapped_row_count(line: &str, width: usize) -> usize {
    wrap_rows(line, width).len()
}

// Color raw cloudflared lines by level marker (ERR red, WRN yellow, INF
// green), plus HTTP status-class coloring for DBG response lines (2xx green,
// 3xx cyan, 4xx yellow, 5xx magenta). Everything else stays white for
// readability.
fn raw_line_style(line: &str) -> Style {
    if line.contains("ERR") {
        return Style::default().fg(Color::Red);
    }
    if line.contains("WRN") {
        return Style::default().fg(Color::Yellow);
    }
    if line.contains("INF") {
        return Style::default().fg(Color::Green);
    }
    // DBG response lines look like "... DBG 200 OK ..." or "... DBG 302 Found ...".
    // Peek at the token right after " DBG "; if it's a 3-digit ASCII number, color
    // by status class. Otherwise it's a DBG request (GET/POST/...) or noise → white.
    if let Some(after_dbg) = line.split(" DBG ").nth(1) {
        if let Some(first_token) = after_dbg.split_whitespace().next() {
            if first_token.len() == 3 && first_token.chars().all(|c| c.is_ascii_digit()) {
                let class = first_token.chars().next().unwrap();
                return match class {
                    '2' => Style::default().fg(Color::Green),
                    '3' => Style::default().fg(Color::Cyan),
                    '4' => Style::default().fg(Color::Yellow),
                    '5' => Style::default().fg(Color::Magenta),
                    _ => Style::default().fg(Color::White),
                };
            }
        }
    }
    Style::default().fg(Color::White)
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let (target, hostname) = match app.selected_tunnel_details() {
        Some(details) => details,
        None => return,
    };

    // We know neither the target nor the hostname of a tunnel ytunnel didn't
    // create, so show why rather than a bare "http://"
    let unknown = "unknown (config lives on the machine running cloudflared)";

    let target_url = if target.is_empty() {
        unknown.to_string()
    } else if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        format!("http://{}", target)
    };

    let public_url = if hostname.is_empty() {
        unknown.to_string()
    } else {
        format!("https://{}", hostname)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Destination: ", Style::default().fg(Color::Gray)),
            Span::styled(&target_url, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Public URL:  ", Style::default().fg(Color::Gray)),
            Span::styled(public_url, Style::default().fg(Color::Cyan)),
        ]),
    ];

    let details = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Details ")
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(details, area);
}

fn render_metrics(
    f: &mut Frame,
    metrics: Option<&TunnelMetrics>,
    sparkline: &str,
    health: HealthStatus,
    area: Rect,
) {
    let metrics = match metrics {
        Some(m) => m,
        None => return,
    };

    // Format response codes
    let mut codes: Vec<_> = metrics.response_codes.iter().collect();
    codes.sort_by_key(|(k, _)| *k);
    let codes_str: String = codes
        .iter()
        .map(|(code, count)| format!("{}:{}", code, count))
        .collect::<Vec<_>>()
        .join("  ");

    // Health status formatting
    let (health_symbol, health_color, health_text) = match health {
        HealthStatus::Unknown => ("?", Color::Gray, "unknown"),
        HealthStatus::Healthy => ("✓", Color::Green, "healthy"),
        HealthStatus::Unhealthy => ("✗", Color::Red, "unreachable"),
        HealthStatus::Checking => ("…", Color::Yellow, "checking"),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Requests: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.total_requests),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("    Errors: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.request_errors),
                Style::default().fg(if metrics.request_errors > 0 {
                    Color::Red
                } else {
                    Color::Green
                }),
            ),
            Span::styled("    Active: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.concurrent_requests),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("    Health: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{} {}", health_symbol, health_text),
                Style::default().fg(health_color),
            ),
        ]),
        Line::from(vec![
            Span::styled("HA Connections: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.ha_connections),
                Style::default().fg(if metrics.ha_connections >= 4 {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
            Span::styled("    Edge: ", Style::default().fg(Color::Gray)),
            Span::styled(
                metrics.locations_string(),
                Style::default().fg(Color::Magenta),
            ),
        ]),
        Line::from(vec![
            Span::styled("Status Codes: ", Style::default().fg(Color::Gray)),
            Span::styled(
                if codes_str.is_empty() {
                    "none".to_string()
                } else {
                    codes_str
                },
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("Traffic: ", Style::default().fg(Color::Gray)),
            Span::styled(
                if sparkline.is_empty() {
                    "waiting...".to_string()
                } else {
                    sparkline.to_string()
                },
                Style::default().fg(Color::Green),
            ),
        ]),
    ];

    let metrics_widget = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Metrics ")
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(metrics_widget, area);
}

fn render_status_line(f: &mut Frame, app: &App, area: Rect) {
    // Show spinner if active, otherwise show status message
    let (status_text, style) = if let Some(spinner_text) = app.spinner.display() {
        (spinner_text, Style::default().fg(Color::Cyan))
    } else {
        let text = app.status_message.as_deref().unwrap_or("").to_string();
        let style = if text.starts_with("Error") {
            Style::default().fg(Color::Red)
        } else if text.contains("Imported")
            || text.contains("Started")
            || text.contains("Deleted")
            || text.contains("updated")
        {
            Style::default().fg(Color::Green)
        } else {
            Style::default().fg(Color::Yellow)
        };
        (text, style)
    };

    let status = Paragraph::new(format!(" {}", status_text)).style(style);
    f.render_widget(status, area);
}

fn render_help_bar(f: &mut Frame, app: &App, area: Rect) {
    let help_text = match app.input_mode {
        InputMode::Normal => {
            if app.demo {
                " (demo) Tab:focus  \u{2191}\u{2193}/jk navigate  [c]opy  [r]efresh  [?]help  [q]uit"
                    .to_string()
            } else {
                // Show account switching hint if multiple accounts
                let account_hint = if app.accounts.len() > 1 {
                    " [;]account"
                } else {
                    ""
                };

                // Only offer actions that apply to the selected tunnel's kind
                match app.selected_kind() {
                    Some(TunnelKind::Ephemeral) => format!(
                        " Tab:focus [m]anage [c]opy [o]pen [h]ealth [d]elete [r]efresh{} [?]help [q]uit",
                        account_hint
                    ),
                    Some(TunnelKind::External) => format!(
                        " Tab:focus (not managed by ytunnel) [a]dd [r]efresh{} [?]help [q]uit",
                        account_hint
                    ),
                    _ => format!(" Tab:focus [a]dd [e]dit [s]tart [S]top [R]estart [A]utostart [c]opy [o]pen [h]ealth [d]elete [r]efresh{} [?]help [q]uit", account_hint),
                }
            }
        }
        InputMode::AddName | InputMode::AddTarget => {
            " Enter value, then press Enter. Esc to cancel.".to_string()
        }
        InputMode::AddZone => " ↑/↓ select zone  Enter confirm  Esc cancel".to_string(),
        InputMode::EditTarget => " Edit target URL, then press Enter. Esc to cancel.".to_string(),
        InputMode::EditZone => " ↑/↓ select zone  Enter confirm  Esc cancel".to_string(),
        InputMode::Confirm => " y confirm  n/Esc cancel".to_string(),
        InputMode::Help => " Press Esc or ? to close help".to_string(),
    };

    let help = Paragraph::new(help_text).style(Style::default().fg(Color::White));

    f.render_widget(help, area);
}

fn render_add_dialog(f: &mut Frame, prompt: &str, input: &str, is_importing: bool) {
    let area = centered_rect(60, 25, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let title = if is_importing {
        " Import Tunnel "
    } else {
        " Add Tunnel "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(block, area);

    // Build styled content matching zone dialog style
    let lines = vec![
        Line::from(Span::styled(prompt, Style::default().fg(Color::Yellow))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(input, Style::default().fg(Color::Green)),
            Span::styled("_", Style::default().fg(Color::White)),
        ]),
    ];

    let text = Paragraph::new(lines)
        .block(Block::default().padding(ratatui::widgets::Padding::new(2, 2, 1, 1)));

    f.render_widget(text, area);
}

fn render_zone_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 50, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let title = if app.is_importing {
        " Import: Select Zone "
    } else {
        " Select Zone "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(block, area);

    // Build zone lines with selection indicator
    let header_lines = 5; // Name, Target, empty, Select zone:, empty
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("Name: "),
            Span::styled(
                app.new_tunnel_name.as_deref().unwrap_or(""),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::raw("Target: "),
            Span::styled(
                app.new_tunnel_target.as_deref().unwrap_or(""),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Select zone:",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
    ];

    // Add zone options
    for (i, zone) in app.zones.iter().enumerate() {
        let selected = i == app.zone_selected;
        let prefix = if selected { "> " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, zone.name),
            style,
        )));
    }

    // Calculate scroll to keep selected item visible
    // Available height = area height - borders (2) - padding (2)
    let available_height = area.height.saturating_sub(4) as usize;
    let scroll = if available_height > header_lines {
        let visible_zones = available_height - header_lines;
        if app.zone_selected >= visible_zones {
            (app.zone_selected - visible_zones + 1) as u16
        } else {
            0
        }
    } else {
        0
    };

    let content = Paragraph::new(lines)
        .block(Block::default().padding(ratatui::widgets::Padding::new(2, 2, 1, 1)))
        .scroll((scroll, 0));

    f.render_widget(content, area);
}

fn render_edit_dialog(f: &mut Frame, app: &App, prompt: &str) {
    let area = centered_rect(60, 30, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit Tunnel ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(block, area);

    // Build styled content showing tunnel being edited
    let lines = vec![
        Line::from(vec![
            Span::raw("Editing: "),
            Span::styled(
                app.editing_tunnel_name.as_deref().unwrap_or(""),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(prompt, Style::default().fg(Color::Yellow))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "> ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&app.input, Style::default().fg(Color::Green)),
            Span::styled("_", Style::default().fg(Color::White)),
        ]),
    ];

    let text = Paragraph::new(lines)
        .block(Block::default().padding(ratatui::widgets::Padding::new(2, 2, 1, 1)));

    f.render_widget(text, area);
}

fn render_edit_zone_dialog(f: &mut Frame, app: &App) {
    let area = centered_rect(60, 50, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Edit: Select Zone ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    f.render_widget(block, area);

    // Build zone lines with selection indicator
    let header_lines = 6; // Editing, Name, Target, empty, Select zone:, empty
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::raw("Editing: "),
            Span::styled(
                app.editing_tunnel_name.as_deref().unwrap_or(""),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("New Target: "),
            Span::styled(
                app.new_tunnel_target.as_deref().unwrap_or(""),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Select zone:",
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
    ];

    // Add zone options
    for (i, zone) in app.zones.iter().enumerate() {
        let selected = i == app.zone_selected;
        let is_original = app.original_zone_id.as_deref() == Some(&zone.id);
        let prefix = if selected { "> " } else { "  " };
        let suffix = if is_original { " (current)" } else { "" };
        let style = if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(
            format!("{}{}{}", prefix, zone.name, suffix),
            style,
        )));
    }

    // Calculate scroll to keep selected item visible
    let available_height = area.height.saturating_sub(4) as usize;
    let scroll = if available_height > header_lines {
        let visible_zones = available_height - header_lines;
        if app.zone_selected >= visible_zones {
            (app.zone_selected - visible_zones + 1) as u16
        } else {
            0
        }
    } else {
        0
    };

    let content = Paragraph::new(lines)
        .block(Block::default().padding(ratatui::widgets::Padding::new(2, 2, 1, 1)))
        .scroll((scroll, 0));

    f.render_widget(content, area);
}

fn render_confirm_dialog(f: &mut Frame, message: &str) {
    let area = centered_rect(60, 15, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Confirm ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let text = Paragraph::new(message)
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::NONE));
    f.render_widget(text, inner);
}

// Create a centered rect of given percentage of the parent
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::log_tail::LogTail;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn wrap_rows_splits_by_width() {
        assert_eq!(wrap_rows("", 4), vec![""]);
        assert_eq!(wrap_rows("abcd", 4), vec!["abcd"]);
        assert_eq!(wrap_rows("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // Wide characters take two columns
        assert_eq!(wrap_rows("ab界cd", 4), vec!["ab界", "cd"]);
    }

    // A demo app whose selected tunnel has `n` numbered log lines
    fn app_with_lines(n: usize) -> App {
        let mut app = App::new_demo();
        app.load_demo_tunnels();
        let name = app.tunnels[app.selected].tunnel.name.clone();
        let lines = (0..n).map(|i| format!("line {:04}", i)).collect();
        app.log_tails.insert(name, LogTail::placeholder(lines));
        app
    }

    // Render the log pane alone into a 40x12 buffer (10 rows inside the
    // border) and return its inner rows, trimmed
    fn draw(app: &App) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| render_logs(f, app, f.area())).unwrap();
        let buf = terminal.backend().buffer().clone();
        (1..11)
            .map(|y| {
                (1..39)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn follows_the_newest_line() {
        let app = app_with_lines(100);
        let rows = draw(&app);
        assert_eq!(rows.first().unwrap(), "line 0090");
        assert_eq!(rows.last().unwrap(), "line 0099");
    }

    #[test]
    fn home_shows_a_full_first_page_and_paging_back_works() {
        let mut app = app_with_lines(100);
        draw(&app);
        app.scroll_logs_top();
        let rows = draw(&app);
        assert_eq!(rows.first().unwrap(), "line 0000");
        assert_eq!(rows.last().unwrap(), "line 0009");

        // Extra scrolling up does not build up past the top
        app.scroll_logs_up(500);
        app.scroll_logs_down(app.log_page_size());
        let rows = draw(&app);
        assert_eq!(rows.first().unwrap(), "line 0009");
    }

    #[test]
    fn scrolling_does_nothing_when_everything_fits() {
        let mut app = app_with_lines(5);
        draw(&app);
        app.scroll_logs_up(3);
        assert_eq!(app.log_scroll, 0);
        assert!(app.log_follow);
        assert_eq!(draw(&app).first().unwrap(), "line 0000");
    }

    #[test]
    fn long_lines_wrap_without_hiding_the_newest_line() {
        let mut app = App::new_demo();
        app.load_demo_tunnels();
        let name = app.tunnels[app.selected].tunnel.name.clone();
        let mut lines: Vec<String> = (0..20).map(|i| format!("line {:04}", i)).collect();
        lines.push("x".repeat(100)); // 3 rows at width 38
        lines.push("newest".into());
        app.log_tails.insert(name, LogTail::placeholder(lines));
        let rows = draw(&app);
        assert_eq!(rows.last().unwrap(), "newest");
        assert_eq!(rows[rows.len() - 2], "x".repeat(24));
    }

    #[test]
    fn file_tail_replaces_placeholder_and_scrolled_view_stays_put() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.log");
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..50 {
            writeln!(f, "line {:04}", i).unwrap();
        }

        // A placeholder from when the tunnel was ephemeral is swapped for the file
        let mut app = app_with_lines(3);
        let name = app.tunnels[app.selected].tunnel.name.clone();
        app.refresh_file_tail(name.clone(), path.clone());
        assert_eq!(draw(&app).last().unwrap(), "line 0049");

        // Scrolled up, new lines arriving don't move the view
        app.scroll_logs_up(5);
        let before = draw(&app);
        assert_eq!(before.last().unwrap(), "line 0044");
        for i in 50..53 {
            writeln!(f, "line {:04}", i).unwrap();
        }
        app.refresh_file_tail(name.clone(), path.clone());
        assert_eq!(draw(&app), before);

        // End resumes following
        app.scroll_logs_bottom();
        assert_eq!(draw(&app).last().unwrap(), "line 0052");
    }
}
