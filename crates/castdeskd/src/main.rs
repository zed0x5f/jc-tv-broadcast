//! castdeskd — CastDesk daemon: virtual displays → one encode → network streams.

mod api;
mod display;
mod encoder;
mod manager;
mod mediamtx;
mod pipeline;
mod session;

use std::net::{IpAddr, UdpSocket};
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use anyhow::{Context, Result};
use castdesk_core::{Config, paths};
use clap::{Parser, Subcommand};
use tokio::sync::watch;

#[derive(Parser)]
#[command(version, about = "CastDesk daemon")]
struct Cli {
    /// Config file (default ~/.config/castdesk/config.toml)
    #[arg(long, global = true)]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (default).
    Run,
    /// Print detected session, backends and working encoders, then exit.
    Doctor,
}

/// LAN address used in advertised URLs. Connecting a UDP socket sends nothing.
fn lan_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET-1, never routed anywhere real
    Some(s.local_addr().ok()?.ip())
}

fn write_token() -> Result<String> {
    use rand::RngExt;
    let token: String = rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(40)
        .map(char::from)
        .collect();
    let dir = paths::runtime_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let path = paths::token_file();
    std::fs::write(&path, &token)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(token)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,castdeskd=info".into()),
        )
        .init();
    let cli = Cli::parse();
    let config_path = cli.config.unwrap_or_else(paths::config_file);
    let cfg = Config::load(&config_path)?;

    gst::init().context("initialising GStreamer")?;
    let session = session::detect(&session::current_env());
    let encoders = tokio::task::spawn_blocking(encoder::probe_all).await?;

    if matches!(cli.cmd, Some(Cmd::Doctor)) {
        println!("session:  {} / {}", session.kind, session.desktop);
        println!(
            "backends: {}",
            session.backend_order.iter().map(|b| b.label()).collect::<Vec<_>>().join(" → ")
        );
        for e in &encoders {
            println!("encoder:  {:<14} {:<6} {}", e.element, e.codec.label(), if e.working { "OK" } else { "not working" });
        }
        for el in ["pipewiresrc", "ximagesrc", "rtspclientsink", "h264parse", "h265parse"] {
            let ok = gst::ElementFactory::find(el).is_some();
            println!("element:  {el:<14} {}", if ok { "OK" } else { "MISSING" });
        }
        let mtx = mediamtx::locate(&cfg.mediamtx);
        println!("mediamtx: {}", mtx.map(|p| p.display().to_string()).unwrap_or("NOT FOUND".into()));
        println!("lan ip:   {}", lan_ip().map(|i| i.to_string()).unwrap_or("?".into()));
        println!("\nfor missing pieces → packages, run: castdesk-deps");
        return Ok(());
    }

    tracing::info!(kind = %session.kind, desktop = %session.desktop, backends = ?session.backend_order, "session detected");
    let host = if cfg.daemon.advertise_host.is_empty() {
        lan_ip().map(|i| i.to_string()).unwrap_or_else(|| "127.0.0.1".into())
    } else {
        cfg.daemon.advertise_host.clone()
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mtx = mediamtx::MediaMtx::new(cfg.mediamtx.clone());
    let mtx_task = tokio::spawn(mtx.clone().supervise(shutdown_rx));

    let listen = cfg.daemon.listen.clone();
    let manager = manager::Manager::new(
        manager::Context { session, encoders, mediamtx: mtx, host },
        cfg,
        config_path,
    );
    manager.start_configured().await;

    let token = write_token()?;
    let app = api::router(api::AppState { manager: manager.clone(), token: Arc::new(token) });
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding control API on {listen}"))?;
    tracing::info!("control API on http://{listen} (token in {})", paths::token_file().display());

    let shutdown = async {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("shutting down");
    };
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;

    manager.shutdown().await;
    let _ = shutdown_tx.send(true);
    let _ = mtx_task.await;
    let _ = std::fs::remove_file(paths::token_file());
    Ok(())
}
