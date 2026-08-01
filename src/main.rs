//! THESIS: One watch console joins Compose intent, runtime state, dependencies,
//! and integrations; it refuses the disconnected table-and-modal admin pattern.
//! OWN-WORLD: Ink-dark equipment fields, steel rules, sea-glass focus, and
//! explicit green, amber, and red state labels with square instrument framing.
//! STORY: Read system condition, inspect one service, see action scope, act, and
//! follow every command through a persistent watch log.
//! FIRST VIEWPORT: Host rail above a service watch and interlock inspector, with
//! live activity anchored below and primary actions in the footer.
//! FORM: Engineering watch console, grounded direction 4, seed 4d608373.
//! FINISH: unreviewed and undocumented is unfinished; this build ends with the
//! finish review, the verdict, and DESIGN.md.

use std::io::{self, IsTerminal};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use docker_services::backup::{BackupPolicy, BackupStore};
use docker_services::compose::{Manager, detect_host};
use docker_services::control::{ActivityLevel, ControlPlane};
use docker_services::tui;
use docker_services::web::{self, WebConfig};
use docker_services::{
    DEFAULT_BACKUP_DIR, DEFAULT_COMPOSE_FILE, DEFAULT_REFRESH_SECONDS, DEFAULT_WEB_BIND,
};

#[derive(Debug, Parser)]
#[command(
    name = "arrman",
    version,
    about = "Fast, Compose-aware media server operations console"
)]
struct Args {
    /// Docker Compose file to manage.
    #[arg(short = 'f', long, default_value = DEFAULT_COMPOSE_FILE)]
    compose_file: PathBuf,

    /// Host or IP used when displaying service URLs.
    #[arg(long, env = "MEDIA_SERVER_HOST")]
    host: Option<String>,

    /// Runtime refresh interval in seconds.
    #[arg(long, default_value_t = DEFAULT_REFRESH_SECONDS, value_parser = parse_interval)]
    interval: f64,

    /// Print one machine-readable snapshot and exit.
    #[arg(long)]
    json: bool,

    /// Render a deterministic terminal snapshot and exit.
    #[arg(long)]
    snapshot: bool,

    /// Use representative data instead of Docker (for previews and tests).
    #[arg(long)]
    demo: bool,

    /// Snapshot width.
    #[arg(long, default_value_t = 120, requires = "snapshot")]
    width: u16,

    /// Snapshot height.
    #[arg(long, default_value_t = 36, requires = "snapshot")]
    height: u16,

    /// Start the authenticated web control room alongside the TUI.
    #[arg(long)]
    web: bool,

    /// Run only the authenticated web control room (no terminal UI).
    #[arg(long)]
    web_only: bool,

    /// Web listener. Loopback is safest; use a TLS reverse proxy for remote binds.
    #[arg(long, default_value = DEFAULT_WEB_BIND)]
    web_bind: SocketAddr,

    /// Private bearer-token file (created with mode 0600 when absent).
    #[arg(long)]
    web_token_file: Option<PathBuf>,

    /// NAS-backed directory for setup bundles and restore staging.
    #[arg(long, default_value = DEFAULT_BACKUP_DIR, env = "DOCKER_SERVICES_BACKUP_DIR")]
    backup_dir: PathBuf,

    /// Scheduled setup-bundle interval in hours.
    #[arg(long, default_value_t = 24.0, value_parser = parse_backup_hours)]
    backup_interval_hours: f64,

    /// Number of setup bundles retained.
    #[arg(long, default_value_t = 14, value_parser = parse_retention)]
    backup_retention: usize,

    /// Create one setup bundle and exit.
    #[arg(long)]
    backup_now: bool,
}

fn parse_interval(value: &str) -> std::result::Result<f64, String> {
    let interval = value
        .parse::<f64>()
        .map_err(|_| "interval must be a number".to_string())?;
    if !(0.25..=3600.0).contains(&interval) {
        return Err("interval must be between 0.25 and 3600 seconds".to_string());
    }
    Ok(interval)
}

fn parse_backup_hours(value: &str) -> std::result::Result<f64, String> {
    let hours = value
        .parse::<f64>()
        .map_err(|_| "backup interval must be a number of hours".to_string())?;
    if !(1.0 / 12.0..=24.0 * 365.0).contains(&hours) {
        return Err("backup interval must be between 5 minutes and 365 days".to_string());
    }
    Ok(hours)
}

fn parse_retention(value: &str) -> std::result::Result<usize, String> {
    let retention = value
        .parse::<usize>()
        .map_err(|_| "backup retention must be a whole number".to_string())?;
    if !(2..=365).contains(&retention) {
        return Err("backup retention must be between 2 and 365".to_string());
    }
    Ok(retention)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let host = args.host.unwrap_or_else(detect_host);
    let manager = Manager::new(args.compose_file, host);

    if args.demo && !args.snapshot && !args.web_only {
        bail!("--demo is supported with --snapshot or --web-only");
    }
    if (args.json || args.snapshot || args.backup_now) && (args.web || args.web_only) {
        bail!("one-shot JSON, snapshot, and backup modes cannot start the web server");
    }

    if args.snapshot {
        let output = if args.demo {
            tui::demo_snapshot(args.width, args.height)
        } else {
            let snapshot = manager
                .snapshot(true, true, None)
                .context("could not build terminal snapshot")?;
            tui::snapshot_from_data(snapshot, args.width, args.height)
        }?;
        print!("{output}");
        return Ok(());
    }

    if args.json {
        let snapshot = manager
            .snapshot(true, true, None)
            .context("could not build JSON snapshot")?;
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }

    let backup_policy = BackupPolicy::new(
        args.backup_dir,
        Duration::from_secs_f64(args.backup_interval_hours * 3600.0),
        args.backup_retention,
    );
    let control = ControlPlane::new(manager.clone(), BackupStore::new(backup_policy));

    if args.backup_now {
        let record = control
            .create_backup("manual", "cli")
            .context("could not create setup bundle")?;
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    let web_requested = args.web || args.web_only;
    let token_file = args.web_token_file.unwrap_or_else(|| {
        manager
            .compose_file()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".docker-services-web-token")
    });
    let web_handle = if web_requested {
        let handle = web::start(
            control.clone(),
            WebConfig {
                bind: args.web_bind,
                token_file,
                refresh_interval: Duration::from_secs_f64(args.interval),
                demo: args.demo,
            },
        )?;
        let display_host = if handle.address.ip().is_unspecified() {
            manager.host().to_string()
        } else {
            handle.address.ip().to_string()
        };
        eprintln!(
            "Web control room: http://{}:{}",
            display_host,
            handle.address.port()
        );
        eprintln!("Web token file: {}", handle.token_file.display());
        if handle.address.ip().is_loopback() {
            eprintln!(
                "Remote browser tunnel: ssh -L {}:127.0.0.1:{} root@{}",
                handle.address.port(),
                handle.address.port(),
                manager.host()
            );
        } else {
            eprintln!(
                "WARNING: bearer tokens cross this plain-HTTP listener; use a trusted network or TLS reverse proxy."
            );
        }
        control.record(
            "web",
            ActivityLevel::Success,
            format!("Web control room listening on {}", handle.address),
        );
        Some(handle)
    } else {
        None
    };

    let _backup_scheduler = (!args.demo).then(|| control.start_backup_scheduler());

    if args.web_only {
        return web_handle
            .expect("web-only mode starts a web handle")
            .wait();
    }

    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!(
            "interactive mode needs a terminal; use --json or --web-only for non-interactive output"
        );
    }
    let result = tui::run(control, args.interval);
    if let Some(handle) = web_handle {
        handle.shutdown()?;
    }
    result
}
