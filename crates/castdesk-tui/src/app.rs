//! TUI state and input handling. Network calls run on tokio tasks and report
//! back through a channel, so the UI never blocks on the daemon.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use castdesk_core::client::ApiClient;
use castdesk_core::{DisplaySpec, DisplayStatus, SystemInfo, VideoSettings};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use crate::form::{AddForm, FieldKind, suggest_name};
use crate::service;

pub const HISTORY: usize = 120;

pub enum Update {
    Snapshot {
        system: Option<SystemInfo>,
        displays: Vec<DisplayStatus>,
    },
    Disconnected(String),
    Toast(String, bool),
    Logs(Vec<String>),
    Refresh,
}

pub enum Mode {
    Normal,
    Add(AddForm),
    ConfirmDelete(String),
    Logs { lines: Vec<String>, scroll: usize },
    Help,
}

pub struct App {
    pub mode: Mode,
    pub system: Option<SystemInfo>,
    pub displays: Vec<DisplayStatus>,
    pub selected: usize,
    pub connected: Option<Result<(), String>>,
    pub toast: Option<(String, bool, Instant)>,
    /// Bitrate history (kb/s) per display for the sparkline.
    pub history: HashMap<String, VecDeque<u64>>,
    pub quit: bool,
    tx: mpsc::UnboundedSender<Update>,
}

fn client() -> Result<ApiClient, String> {
    ApiClient::from_environment().map_err(|e| e.to_string())
}

/// Poll the daemon once a second.
pub fn spawn_poller(tx: mpsc::UnboundedSender<Update>, mut refresh: mpsc::UnboundedReceiver<()>) {
    tokio::spawn(async move {
        let mut system: Option<SystemInfo> = None;
        let mut last_system = Instant::now() - Duration::from_secs(60);
        loop {
            match client() {
                Ok(c) => {
                    if system.is_none() || last_system.elapsed() > Duration::from_secs(5) {
                        system = c.system().await.ok();
                        last_system = Instant::now();
                    }
                    let msg = match c.displays().await {
                        Ok(displays) => Update::Snapshot { system: system.clone(), displays },
                        Err(e) => {
                            system = None;
                            Update::Disconnected(e.to_string())
                        }
                    };
                    if tx.send(msg).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    system = None;
                    if tx.send(Update::Disconnected(e)).is_err() {
                        return;
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                r = refresh.recv() => if r.is_none() { return },
            }
        }
    });
}

impl App {
    pub fn new(tx: mpsc::UnboundedSender<Update>) -> Self {
        Self {
            mode: Mode::Normal,
            system: None,
            displays: vec![],
            selected: 0,
            connected: None,
            toast: None,
            history: HashMap::new(),
            quit: false,
            tx,
        }
    }

    pub fn selected(&self) -> Option<&DisplayStatus> {
        self.displays.get(self.selected)
    }

    fn toast(&mut self, msg: impl Into<String>, ok: bool) {
        self.toast = Some((msg.into(), ok, Instant::now()));
    }

    pub fn apply(&mut self, u: Update) {
        match u {
            Update::Snapshot { system, displays } => {
                self.connected = Some(Ok(()));
                if system.is_some() {
                    self.system = system;
                }
                for d in &displays {
                    let h = self.history.entry(d.spec.name.clone()).or_default();
                    h.push_back(d.stats.bitrate_kbps.max(0.0) as u64);
                    while h.len() > HISTORY {
                        h.pop_front();
                    }
                }
                self.history.retain(|k, _| displays.iter().any(|d| &d.spec.name == k));
                self.displays = displays;
                if self.selected >= self.displays.len() {
                    self.selected = self.displays.len().saturating_sub(1);
                }
            }
            Update::Disconnected(e) => {
                self.connected = Some(Err(e));
                self.system = None;
                self.displays.clear();
            }
            Update::Toast(m, ok) => self.toast(m, ok),
            Update::Logs(lines) => {
                let scroll = lines.len().saturating_sub(1);
                self.mode = Mode::Logs { lines, scroll };
            }
            Update::Refresh => {}
        }
    }

    /// Run an API call in the background; report result as a toast.
    fn api<F, Fut>(&self, ok_msg: String, f: F)
    where
        F: FnOnce(ApiClient) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send,
    {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let res = match client() {
                Ok(c) => f(c).await,
                Err(e) => Err(e),
            };
            let _ = match res {
                Ok(()) => tx.send(Update::Toast(ok_msg, true)),
                Err(e) => tx.send(Update::Toast(e, false)),
            };
            let _ = tx.send(Update::Refresh);
        });
    }

    fn blocking(&self, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = match f() {
                Ok(m) => tx.send(Update::Toast(m, true)),
                Err(e) => tx.send(Update::Toast(e, false)),
            };
        });
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        let mode = std::mem::replace(&mut self.mode, Mode::Normal);
        self.mode = match mode {
            Mode::Normal => self.normal_key(key),
            Mode::Add(form) => self.form_key(form, key),
            Mode::ConfirmDelete(name) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let n = name.clone();
                    self.api(format!("removed {name}"), move |c| async move {
                        c.remove_display(&n).await.map(|_| ()).map_err(|e| e.to_string())
                    });
                    Mode::Normal
                }
                _ => Mode::Normal,
            },
            Mode::Logs { lines, mut scroll } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => Mode::Normal,
                KeyCode::Up | KeyCode::Char('k') => {
                    scroll = scroll.saturating_sub(1);
                    Mode::Logs { lines, scroll }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    scroll = (scroll + 1).min(lines.len().saturating_sub(1));
                    Mode::Logs { lines, scroll }
                }
                KeyCode::PageUp => {
                    scroll = scroll.saturating_sub(20);
                    Mode::Logs { lines, scroll }
                }
                KeyCode::PageDown | KeyCode::End | KeyCode::Char('G') => {
                    scroll = if key.code == KeyCode::PageDown {
                        (scroll + 20).min(lines.len().saturating_sub(1))
                    } else {
                        lines.len().saturating_sub(1)
                    };
                    Mode::Logs { lines, scroll }
                }
                KeyCode::Char('r') => {
                    self.load_logs();
                    Mode::Logs { lines, scroll }
                }
                _ => Mode::Logs { lines, scroll },
            },
            Mode::Help => Mode::Normal,
        };
    }

    fn load_logs(&self) {
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Update::Logs(service::read_logs(500)));
        });
    }

    fn normal_key(&mut self, key: KeyEvent) -> Mode {
        let connected = matches!(self.connected, Some(Ok(())));
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') if !self.displays.is_empty() => {
                self.selected = (self.selected + 1) % self.displays.len();
            }
            KeyCode::Up | KeyCode::Char('k') if !self.displays.is_empty() => {
                self.selected = (self.selected + self.displays.len() - 1) % self.displays.len();
            }
            KeyCode::Char('?') | KeyCode::Char('h') => return Mode::Help,
            KeyCode::Char('l') => {
                self.load_logs();
                return Mode::Logs { lines: vec!["loading…".into()], scroll: 0 };
            }
            KeyCode::Char('s') => self.blocking(service::start_daemon),
            KeyCode::Char('S') => self.blocking(service::stop_daemon),
            KeyCode::Char('a') | KeyCode::Char('n') if connected => {
                let names: Vec<String> = self.displays.iter().map(|d| d.spec.name.clone()).collect();
                return Mode::Add(AddForm::new(suggest_name(&names), &VideoSettings::default()));
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(d) = self.selected() {
                    return Mode::ConfirmDelete(d.spec.name.clone());
                }
            }
            KeyCode::Char('r') => {
                if let Some(d) = self.selected() {
                    let n = d.spec.name.clone();
                    self.api(format!("restarting {n}"), move |c| async move {
                        c.restart_display(&n).await.map(|_| ()).map_err(|e| e.to_string())
                    });
                }
            }
            KeyCode::Char('c') | KeyCode::Char('y') => {
                if let Some(url) = self.selected().and_then(|d| d.urls.first()).map(|u| u.url.clone()) {
                    let msg = service::copy(&url);
                    self.toast(msg, true);
                }
            }
            KeyCode::Char('a') if !connected => {
                self.toast("daemon not connected — press s to start it", false);
            }
            _ => {}
        }
        Mode::Normal
    }

    fn form_key(&mut self, mut form: AddForm, key: KeyEvent) -> Mode {
        match key.code {
            KeyCode::Esc => return Mode::Normal,
            KeyCode::Tab | KeyCode::Down => form.next(),
            KeyCode::BackTab | KeyCode::Up => form.prev(),
            KeyCode::Enter => match form.to_spec() {
                Ok(spec) => {
                    self.submit(spec);
                    return Mode::Normal;
                }
                Err(e) => form.error = Some(e),
            },
            KeyCode::Left => form.focused().cycle(-1),
            KeyCode::Right | KeyCode::Char(' ') if form.focused().kind == FieldKind::Choice => {
                form.focused().cycle(1)
            }
            KeyCode::Backspace if form.focused().kind == FieldKind::Text => {
                form.focused().value.pop();
            }
            KeyCode::Char(c) if form.focused().kind == FieldKind::Text => {
                form.focused().value.push(c);
                form.error = None;
            }
            _ => {}
        }
        Mode::Add(form)
    }

    fn submit(&self, spec: DisplaySpec) {
        let name = spec.name.clone();
        self.api(format!("added {name}"), move |c| async move {
            c.add_display(&spec).await.map(|_| ()).map_err(|e| e.to_string())
        });
    }
}
