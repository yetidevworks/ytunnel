use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::{App, HealthStatus, InputMode, TunnelKind};
use crate::metrics::TunnelMetrics;
use crate::state::TunnelStatus;

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
    let area = centered_rect(70, 80, f.area());

    // Clear the area
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" Help - Press Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let help_text = vec![
        Line::from(Span::styled(
            "NAVIGATION",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ↑/k      ", Style::default().fg(Color::Cyan)),
            Span::raw("Move selection up"),
        ]),
        Line::from(vec![
            Span::styled("  ↓/j      ", Style::default().fg(Color::Cyan)),
            Span::raw("Move selection down"),
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
            Span::raw("  System notifications on tunnel "),
            Span::styled("down/up", Style::default().fg(Color::Red)),
        ]),
    ];

    let help = Paragraph::new(help_text).wrap(Wrap { trim: false });

    f.render_widget(help, inner);
}

fn render_tunnels(f: &mut Frame, app: &App, area: Rect) {
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

            // Show ephemeral tunnels with italic
            let (final_name_style, hostname_display) = match entry.kind {
                TunnelKind::Managed => (name_style, entry.tunnel.hostname.clone()),
                TunnelKind::Ephemeral => (
                    name_style.add_modifier(Modifier::ITALIC),
                    format!("{} [ephemeral]", entry.tunnel.name),
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

    let tunnels_list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    f.render_widget(tunnels_list, area);
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let title = if let Some(entry) = app.tunnels.get(app.selected) {
        format!(" Logs: {} ", entry.tunnel.name)
    } else {
        " Logs ".to_string()
    };

    // Take last N lines that fit in the area
    let available_height = area.height.saturating_sub(2) as usize; // -2 for borders
    let start = if app.logs.len() > available_height {
        app.logs.len() - available_height
    } else {
        0
    };

    let log_lines: Vec<Line> = app.logs[start..]
        .iter()
        .map(|line| {
            let color = if line.contains("ERR") {
                Color::Red
            } else if line.contains("WRN") {
                Color::Yellow
            } else if line.contains("INF") {
                Color::Green
            } else {
                Color::Gray
            };
            Line::from(Span::styled(line.clone(), Style::default().fg(color)))
        })
        .collect();

    let logs = Paragraph::new(log_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    f.render_widget(logs, area);
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let (target, hostname) = match app.selected_tunnel_details() {
        Some(details) => details,
        None => return,
    };

    // Format target URL for display
    let target_url = if target.starts_with("http://") || target.starts_with("https://") {
        target.to_string()
    } else {
        format!("http://{}", target)
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("Destination: ", Style::default().fg(Color::Gray)),
            Span::styled(&target_url, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("Public URL:  ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("https://{}", hostname),
                Style::default().fg(Color::Cyan),
            ),
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
                " (demo) \u{2191}\u{2193}/jk navigate  [c]opy  [r]efresh  [?]help  [q]uit"
                    .to_string()
            } else {
                // Show different help based on whether an ephemeral tunnel is selected
                let is_ephemeral = app
                    .tunnels
                    .get(app.selected)
                    .map(|e| e.kind == TunnelKind::Ephemeral)
                    .unwrap_or(false);

                // Show account switching hint if multiple accounts
                let account_hint = if app.accounts.len() > 1 {
                    " [;]account"
                } else {
                    ""
                };

                if is_ephemeral {
                    format!(
                        " [m]anage [c]opy [o]pen [h]ealth [d]elete [r]efresh{} [?]help [q]uit",
                        account_hint
                    )
                } else {
                    format!(" [a]dd [e]dit [s]tart [S]top [R]estart [A]utostart [c]opy [o]pen [h]ealth [d]elete [r]efresh{} [?]help [q]uit", account_hint)
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

    let help = Paragraph::new(help_text).style(Style::default().fg(Color::DarkGray));

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
