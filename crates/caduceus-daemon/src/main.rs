//! `caduceusd` — orchestrator daemon binary entry point.
//!
//! Per the implementation DAG (todo `f01-daemon-scaffold`), this binary
//! drives the lifecycle FSM:
//!
//! ```text
//!     Booting → Ready → Draining → Halted
//! ```
//!
//! `main()` parses CLI arguments, loads the config, sets up the lifecycle
//! handle + signal handlers, and exits 0 on a clean drain.  Subsystems
//! (mailbox, IPC, dispatch loop, snapshot RPC) plug in across subsequent
//! Phase 0 / Phase 3 todos.
//!
//! Spec cross-references:
//!
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.1** — boot reconcile
//!   sweep MUST run before first dispatch tick.  This binary spawns the
//!   sweep task between `mark_ready()` and the dispatch loop.
//! - **`spec-caduceus-orchestrator-algorithm.md` §3.5** — `on_shutdown`
//!   sets `state.shutting_down = true`.  Here that's
//!   `Lifecycle::mark_draining()`.

use caduceus_daemon::{Config, DaemonError, DaemonResult, Lifecycle, ShutdownReason};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    if let Err(e) = run().await {
        eprintln!("caduceusd: fatal: {e}");
        return std::process::ExitCode::from(1);
    }
    std::process::ExitCode::SUCCESS
}

async fn run() -> DaemonResult<()> {
    init_tracing();

    let args = parse_args();
    let config = Config::from_path(&args.config_path).map_err(DaemonError::from)?;
    config.validate().map_err(DaemonError::from)?;

    tracing::info!(
        workflow_path = %config.workflow_path.display(),
        workspace_root = %config.workspace_root.display(),
        max_concurrency = config.max_concurrency,
        "caduceusd: configuration loaded"
    );

    let lifecycle = Lifecycle::new();

    install_signal_handlers(lifecycle.clone());

    // ────────────────── Phase 0 scaffold complete ─────────────────────
    // Subsequent foundations (f04-clock, f05-cmd-mailbox, f06-ipc, f07-storage,
    // f08-logging-telemetry) plug in here.  Phase 3 `or00-boot-reconcile-sweep`
    // runs after `mark_ready()` completes.
    lifecycle.mark_ready();
    tracing::info!("caduceusd: ready (P0 scaffold)");

    // For the scaffold, we simply wait for a shutdown signal.  When the
    // dispatch loop lands (P3 `or10-dispatch-loop`), this becomes a
    // `tokio::select!` over the loop and the signal future.
    wait_for_shutdown(&lifecycle).await;

    tracing::info!("caduceusd: drain complete");
    lifecycle.mark_halted();
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_env("CADUCEUSD_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}

#[derive(Debug)]
struct Args {
    config_path: PathBuf,
}

fn parse_args() -> Args {
    let mut config_path: Option<PathBuf> = None;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" | "--config" => {
                config_path = iter.next().map(PathBuf::from);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "--version" => {
                println!("caduceusd {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("caduceusd: unknown argument: {other}");
                print_help();
                std::process::exit(2);
            }
        }
    }
    let config_path = config_path.unwrap_or_else(default_config_path);
    Args { config_path }
}

fn print_help() {
    println!(
        "caduceusd — orchestrator daemon for caduceus engine

USAGE:
    caduceusd [OPTIONS]

OPTIONS:
    -c, --config <PATH>    Path to caduceusd.toml (default: $XDG_CONFIG_HOME/caduceus/caduceusd.toml)
    -h, --help             Print this help
        --version          Print version

ENVIRONMENT:
    CADUCEUSD_LOG          tracing-subscriber filter directive (default: info)
"
    );
}

fn default_config_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("caduceus").join("caduceusd.toml");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("caduceus")
            .join("caduceusd.toml");
    }
    PathBuf::from("caduceusd.toml")
}

#[cfg(unix)]
fn install_signal_handlers(lifecycle: Lifecycle) {
    use tokio::signal::unix::{signal, SignalKind};
    let lc = lifecycle.clone();
    tokio::spawn(async move {
        let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        let mut intr = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let reason = tokio::select! {
            _ = term.recv() => ShutdownReason::Signal,
            _ = intr.recv() => ShutdownReason::Signal,
        };
        tracing::warn!(?reason, "caduceusd: shutdown signal received");
        lc.mark_draining();
    });
}

#[cfg(not(unix))]
fn install_signal_handlers(lifecycle: Lifecycle) {
    let lc = lifecycle.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::warn!("caduceusd: Ctrl+C received");
            lc.mark_draining();
        }
    });
}

async fn wait_for_shutdown(lifecycle: &Lifecycle) {
    // Polling cadence: cheap and visible; replaced by a Notify when the
    // mailbox lands.
    while !lifecycle.is_shutting_down() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
