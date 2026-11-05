//! Rendering.
//!
//! Testing: covered by the TestBackend tests in `main.rs` (connected,
//! disconnected, failed display, tiny terminal).

use std::time::Duration;

use castdesk_core::{DisplayState, DisplayStatus};
use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Sparkline, Wrap};

use crate::app::{App, HISTORY, Mode};
use crate::form::FieldKind;

fn state_color(s: &DisplayState) -> Color {
    match s {
        DisplayState::Running => Color::Green,
        DisplayState::Starting | DisplayState::Restarting { .. } => Color::Yellow,
        DisplayState::Failed { .. } => Color::Red,
        DisplayState::Stopped => Color::DarkGray,
    }
}

fn dur(s: u64) -> String {
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

pub fn draw(f: &mut Frame, app: &App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(f.area());

    draw_header(f, app, header);
    match &app.connected {
        Some(Err(e)) => draw_disconnected(f, e, body),
        None => f.render_widget(Paragraph::new("connecting to castdeskd…").block(Block::bordered()), body),
        Some(Ok(())) => {
            let [list, detail] =
                Layout::horizontal([Constraint::Length(28), Constraint::Min(40)]).areas(body);
            draw_list(f, app, list);
            draw_detail(f, app, detail);
        }
    }
    draw_footer(f, app, footer);

    match &app.mode {
        Mode::Add(_) => draw_form(f, app),
        Mode::ConfirmDelete(name) => draw_confirm(f, name),
        Mode::Logs { lines, scroll } => draw_logs(f, lines, *scroll),
        Mode::Help => draw_help(f),
        Mode::Normal => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(" CastDesk ", Style::new().bold().fg(Color::Black).bg(Color::Cyan)), Span::raw("  ")];
    match (&app.connected, &app.system) {
        (Some(Ok(())), Some(sys)) => {
            spans.push(Span::styled("● daemon", Style::new().fg(Color::Green)));
            spans.push(Span::raw(format!(" v{}   ", sys.version)));
            spans.push(Span::raw(format!("{}/{}   ", sys.session.desktop, sys.session.kind)));
            let encs: Vec<&str> = sys.encoders.iter().filter(|e| e.working).map(|e| e.element.as_str()).collect();
            spans.push(Span::raw(format!("enc: {}   ", if encs.is_empty() { "none!".to_string() } else { encs.join(" ") })));
            let (dot, c) = if sys.mediamtx.running { ("●", Color::Green) } else { ("●", Color::Red) };
            spans.push(Span::styled(dot, Style::new().fg(c)));
            spans.push(Span::raw(format!(" RTSP {}:{}", sys.host, sys.mediamtx.rtsp_port)));
            if let Some(m) = &sys.mediamtx.message {
                spans.push(Span::styled(format!("  {m}"), Style::new().fg(Color::Yellow)));
            }
        }
        (Some(Ok(())), None) => spans.push(Span::styled("● daemon", Style::new().fg(Color::Green))),
        _ => spans.push(Span::styled("○ daemon not connected", Style::new().fg(Color::Red))),
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::bordered().border_type(BorderType::Rounded)),
        area,
    );
}

fn draw_disconnected(f: &mut Frame, err: &str, area: Rect) {
    let text = vec![
        Line::from("castdeskd is not reachable.".bold()),
        Line::from(""),
        Line::from(Span::styled(err.to_string(), Style::new().fg(Color::Red))),
        Line::from(""),
        Line::from("  s  start the daemon (systemd user unit if installed, else runs castdeskd in the background)"),
        Line::from("  l  view daemon logs"),
        Line::from("  q  quit"),
    ];
    f.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(Block::bordered().title(" Daemon ")),
        area,
    );
}

fn draw_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = if app.displays.is_empty() {
        vec![ListItem::new(Line::from("  (none — press a)").dark_gray())]
    } else {
        app.displays
            .iter()
            .map(|d| {
                ListItem::new(Line::from(vec![
                    Span::styled("● ", Style::new().fg(state_color(&d.state))),
                    Span::raw(format!("{:<12}", d.spec.name)),
                    Span::styled(d.state.label(), Style::new().fg(state_color(&d.state))),
                ]))
            })
            .collect()
    };
    let mut state = ListState::default();
    if !app.displays.is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(
        List::new(items)
            .block(Block::bordered().title(" Displays "))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}

fn kv<'a>(k: &'a str, v: impl Into<Span<'a>>) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<10}"), Style::new().fg(Color::Cyan)), v.into()])
}

fn draw_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(d) = app.selected() else {
        f.render_widget(
            Paragraph::new(vec![
                Line::from("No virtual displays yet."),
                Line::from(""),
                Line::from("Press a to add one. Use backend \"test\" for color bars without touching your desktop."),
            ])
            .wrap(Wrap { trim: true })
            .block(Block::bordered().title(" Details ")),
            area,
        );
        return;
    };
    let block = Block::bordered().title(format!(" {} ", d.spec.name));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [text_area, spark_area] =
        Layout::vertical([Constraint::Min(10), Constraint::Length(5)]).areas(inner);
    f.render_widget(Paragraph::new(detail_lines(d)).wrap(Wrap { trim: false }), text_area);

    let data: Vec<u64> = app
        .history
        .get(&d.spec.name)
        .map(|h| h.iter().copied().collect())
        .unwrap_or_default();
    let width = spark_area.width as usize;
    let data = &data[data.len().saturating_sub(width.min(HISTORY))..];
    let max = (d.spec.video.bitrate_kbps as u64 * 3 / 2).max(1);
    f.render_widget(
        Sparkline::default()
            .block(Block::new().title(format!(" bitrate (last {}s, scale 0–{} Mb/s) ", data.len(), max / 1000)).dark_gray())
            .data(data)
            .max(max)
            .style(Style::new().fg(Color::Green)),
        spark_area,
    );
}

pub fn detail_lines(d: &DisplayStatus) -> Vec<Line<'static>> {
    let v = &d.spec.video;
    let s = &d.stats;
    let mut lines = vec![];
    let state = match &d.state {
        DisplayState::Running => format!("running  (up {})", dur(s.uptime_s)),
        DisplayState::Restarting { attempt, .. } => format!("restarting (attempt {attempt})"),
        other => other.label().to_string(),
    };
    lines.push(kv("State", Span::styled(state, Style::new().fg(state_color(&d.state)).bold())));
    match &d.state {
        DisplayState::Failed { error } | DisplayState::Restarting { last_error: error, .. } => {
            lines.push(kv("Error", Span::styled(error.clone(), Style::new().fg(Color::Red))));
            if matches!(d.state, DisplayState::Failed { .. }) {
                lines.push(kv("", Span::raw("fix the cause, then press r to retry").dark_gray()));
            }
        }
        _ => {}
    }
    lines.push(kv(
        "Backend",
        format!(
            "{}{}",
            d.backend.map(|b| b.label()).unwrap_or("…"),
            d.output_name.as_ref().map(|o| format!("   output {o}")).unwrap_or_default()
        ),
    ));
    lines.push(kv(
        "Video",
        format!(
            "{}x{} @ {} fps   {}   {:.1} Mb/s CBR   GOP {}s   {}",
            v.width,
            v.height,
            v.fps,
            v.codec.label(),
            v.bitrate_kbps as f64 / 1000.0,
            v.gop_seconds,
            d.spec.placement.label()
        ),
    ));
    lines.push(kv("Encoder", d.encoder.clone().unwrap_or_else(|| "…".into())));
    lines.push(Line::from(""));
    lines.push(kv(
        "Live",
        format!(
            "{:>6.2} Mb/s   {:>5.1} fps   frames {}   dropped {}   clients {}",
            s.bitrate_kbps / 1000.0,
            s.fps,
            s.frames_total,
            s.dropped_total,
            s.rtsp_clients
        ),
    ));
    lines.push(Line::from(""));
    for w in v.compat_warnings() {
        lines.push(kv("Note", Span::styled(w, Style::new().fg(Color::Yellow))));
    }
    for u in &d.urls {
        let label = match u.kind.as_str() {
            "hls" => "HLS",
            "webrtc" => "WebRTC",
            _ => "Stream",
        };
        lines.push(kv(label, Span::styled(u.url.clone(), Style::new().bold().underlined())));
        if u.kind != "rtsp" {
            continue;
        }
        // TCP: works through firewalls that block RTP/UDP and doesn't smear
        // on lossy Wi-Fi. Drop the flag for the lowest latency on wired LANs.
        lines.push(kv("  VLC", Span::raw(format!("vlc --rtsp-tcp --network-caching=200 {}", u.url)).dark_gray()));
        lines.push(kv("  mpv", Span::raw(format!("mpv --profile=low-latency --untimed --rtsp-transport=tcp {}", u.url)).dark_gray()));
        lines.push(kv("  ffplay", Span::raw(format!("ffplay -rtsp_transport tcp -fflags nobuffer -flags low_delay -framedrop {}", u.url)).dark_gray()));
    }
    lines
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if let Some((msg, ok, at)) = &app.toast
        && at.elapsed() < Duration::from_secs(6) {
            let c = if *ok { Color::Green } else { Color::Red };
            f.render_widget(Paragraph::new(Span::styled(format!(" {msg}"), Style::new().fg(c))), area);
            return;
        }
    let keys = [
        ("a", "add"), ("d", "delete"), ("r", "restart"), ("c", "copy URL"),
        ("s/S", "start/stop daemon"), ("l", "logs"), ("?", "help"), ("q", "quit"),
    ];
    let mut spans = vec![];
    for (k, v) in keys {
        spans.push(Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(Color::Gray)));
        spans.push(Span::raw(format!(" {v}  ")));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn popup(area: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(v);
    r
}

fn draw_form(f: &mut Frame, app: &App) {
    let Mode::Add(form) = &app.mode else { return };
    let area = popup(f.area(), 90, form.fields.len() as u16 + 5);
    f.render_widget(Clear, area);
    let mut lines = vec![];
    for (i, field) in form.fields.iter().enumerate() {
        let focused = i == form.focus;
        let value = match field.kind {
            FieldKind::Text if focused => format!("{}▏", field.value),
            FieldKind::Text => field.value.clone(),
            FieldKind::Choice => format!("◂ {} ▸", field.value),
        };
        let style = if focused { Style::new().bold().fg(Color::Yellow) } else { Style::new() };
        lines.push(Line::from(vec![
            Span::styled(format!(" {} {:<13}", if focused { "›" } else { " " }, field.label), style),
            Span::styled(format!("{value:<22}"), style),
            Span::raw(if focused { field.hint } else { "" }).dark_gray(),
        ]));
    }
    lines.push(Line::from(""));
    if let Some(e) = &form.error {
        lines.push(Line::from(Span::styled(format!(" {e}"), Style::new().fg(Color::Red))));
    } else {
        lines.push(Line::from(" Tab/↑↓ move · ←/→ change · Enter create · Esc cancel").dark_gray());
    }
    f.render_widget(
        Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Double).title(" Add virtual display ")),
        area,
    );
}

fn draw_confirm(f: &mut Frame, name: &str) {
    let area = popup(f.area(), 56, 5);
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(format!(" Remove display {name:?} and stop its stream?")),
            Line::from(""),
            Line::from(" y = remove · any other key = cancel").dark_gray(),
        ])
        .block(Block::bordered().title(" Confirm ").red()),
        area,
    );
}

fn draw_logs(f: &mut Frame, lines: &[String], scroll: usize) {
    let area = f.area().inner(ratatui::layout::Margin { horizontal: 2, vertical: 1 });
    f.render_widget(Clear, area);
    let height = area.height.saturating_sub(2) as usize;
    let start = (scroll + 1).saturating_sub(height);
    let shown: Vec<Line> = lines[start.min(lines.len())..(scroll + 1).min(lines.len())]
        .iter()
        .map(|l| {
            let c = if l.contains("ERROR") { Color::Red } else if l.contains("WARN") { Color::Yellow } else { Color::Reset };
            Line::from(Span::styled(l.clone(), Style::new().fg(c)))
        })
        .collect();
    f.render_widget(
        Paragraph::new(shown).block(
            Block::bordered().title(format!(" castdeskd logs ({}/{}) · ↑↓ PgUp PgDn G · r reload · Esc close ", scroll + 1, lines.len())),
        ),
        area,
    );
}

fn draw_help(f: &mut Frame) {
    let area = popup(f.area(), 70, 20);
    f.render_widget(Clear, area);
    let rows = [
        ("↑↓ / j k", "select display"),
        ("a", "add a virtual display + stream"),
        ("d", "remove selected display"),
        ("r", "restart selected (recreates the virtual monitor; retries a failed one)"),
        ("c", "copy the RTSP URL (wl-copy / xclip / OSC 52)"),
        ("s", "start castdeskd (systemd --user unit, or background process)"),
        ("S", "stop castdeskd"),
        ("l", "daemon logs (journalctl --user -u castdeskd)"),
        ("q / Esc", "quit the TUI (the daemon keeps streaming)"),
    ];
    let mut lines: Vec<Line> = rows
        .iter()
        .map(|(k, v)| Line::from(vec![Span::styled(format!(" {k:<10}"), Style::new().fg(Color::Cyan)), Span::raw(*v)]))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(" Displays and settings persist in ~/.config/castdesk/config.toml.").dark_gray());
    lines.push(Line::from(" Press any key to close.").dark_gray());
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(Block::bordered().title(" Help ")),
        area,
    );
}
