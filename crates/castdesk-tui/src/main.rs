//! castdesk-tui — terminal front end for castdeskd.

mod app;
mod form;
mod service;
mod ui;

use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tokio::sync::mpsc;

use app::{App, Update};

#[derive(Parser)]
#[command(version, about = "CastDesk terminal UI: manage virtual displays and streams")]
struct Cli {}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = Cli::parse();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (refresh_tx, refresh_rx) = mpsc::unbounded_channel();
    app::spawn_poller(tx.clone(), refresh_rx);
    let mut app = App::new(tx);

    let mut terminal = ratatui::init();
    let res = (|| -> Result<()> {
        loop {
            while let Ok(u) = rx.try_recv() {
                if matches!(u, Update::Refresh) {
                    let _ = refresh_tx.send(());
                }
                app.apply(u);
            }
            terminal.draw(|f| ui::draw(f, &app))?;
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        app.on_key(k);
                    }
                }
            }
            if app.quit {
                return Ok(());
            }
        }
    })();
    ratatui::restore();
    res
}

#[cfg(test)]
mod tests {
    use castdesk_core::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn status(name: &str, state: DisplayState) -> DisplayStatus {
        DisplayStatus {
            spec: DisplaySpec::new(name),
            state,
            backend: Some(BackendKind::Mutter),
            encoder: Some("vah264enc".into()),
            output_name: None,
            urls: vec![StreamUrl { kind: "rtsp".into(), url: format!("rtsp://10.0.0.5:8554/{name}") }],
            stats: StreamStats { bitrate_kbps: 11900.0, fps: 60.0, rtsp_clients: 2, ..Default::default() },
        }
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| ui::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn renders_connected_and_disconnected() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(tx);
        app.apply(Update::Disconnected("daemon not reachable".into()));
        assert!(render(&app, 100, 30).contains("castdeskd is not reachable"));

        app.apply(Update::Snapshot {
            system: None,
            displays: vec![
                status("tv1", DisplayState::Running),
                status("tv2", DisplayState::Failed { error: "portal denied".into() }),
            ],
        });
        let screen = render(&app, 140, 30);
        assert!(screen.contains("tv1") && screen.contains("tv2"));
        assert!(screen.contains("rtsp://10.0.0.5:8554/tv1"));
        assert!(screen.contains("11.90 Mb/s"));

        app.selected = 1;
        assert!(render(&app, 140, 30).contains("portal denied"));
        // tiny terminal must not panic
        render(&app, 20, 6);
    }

    #[tokio::test]
    async fn add_form_flow() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut app = App::new(tx);
        app.apply(Update::Snapshot { system: None, displays: vec![status("tv1", DisplayState::Running)] });
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        let app::Mode::Add(form) = &app.mode else { panic!("form not open") };
        assert_eq!(form.fields[0].value, "tv2");
        assert!(render(&app, 120, 30).contains("Add virtual display"));
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, app::Mode::Normal));
    }
}
