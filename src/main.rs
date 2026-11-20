use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use flipperaki::config::{Config, TransportKind};
use flipperaki::ipc::{self, IpcClient, Request};
use flipperaki::limits::HOOK_INPUT_MAX_BYTES;
use flipperaki::{daemon, hooks, mcp, setup};

/// A Bluetooth-first Claude desk buddy and MCP controller for Flipper Zero.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Own the device connection (run one per user).
    Daemon,
    /// Serve the MCP tools on stdio (launched by Claude).
    Mcp,
    /// Forward one Claude Code hook event from stdin (launched by Claude Code).
    Hook,
    /// Show daemon and device status.
    Status,
    /// Stop all agent control and revoke any active approval.
    Pause,
    /// Allow new control requests again (each still needs physical approval).
    Resume,
    /// Choose the device and transport the daemon connects to.
    Enroll {
        #[arg(value_enum)]
        transport: TransportKind,
        /// CoreBluetooth peripheral id from `flipperaki scan` (BLE only).
        #[arg(long)]
        peripheral: Option<String>,
    },
    /// List nearby Flipper BLE peripherals and USB serial ports.
    Scan,
    /// Add Flipperaki hooks to Claude Code settings (keeps a backup).
    InstallHooks {
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Remove only Flipperaki's hooks from Claude Code settings.
    UninstallHooks {
        #[arg(long)]
        settings: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Diagnostics go to stderr only; stdout belongs to MCP in `mcp` mode.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FLIPPERAKI_LOG")
                .unwrap_or_else(|_| "warn,flipperaki=info".into()),
        )
        .init();

    if matches!(cli.command, Command::Hook) {
        // Hooks must never fail or slow down Claude: always exit 0, print nothing.
        forward_hook();
        return ExitCode::SUCCESS;
    }
    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("flipperaki: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[tokio::main]
async fn run(command: Command) -> anyhow::Result<()> {
    let dir = ipc::runtime_dir()?;
    let socket = ipc::socket_path(&dir);
    match command {
        Command::Daemon => daemon::run(&dir).await,
        Command::Mcp => mcp::run(&socket).await,
        Command::Hook => Ok(()),
        Command::Status => print_reply(&socket, Request::Status).await,
        Command::Pause => print_reply(&socket, Request::Pause { paused: true }).await,
        Command::Resume => print_reply(&socket, Request::Pause { paused: false }).await,
        Command::Enroll {
            transport,
            peripheral,
        } => {
            let mut config = Config::load(&dir)?;
            config.transport = transport;
            if peripheral.is_some() {
                config.ble_peripheral_id = peripheral;
            }
            anyhow::ensure!(
                transport != TransportKind::Ble || config.ble_peripheral_id.is_some(),
                "BLE enrollment needs --peripheral (see `flipperaki scan`)"
            );
            config.save(&dir)?;
            eprintln!("enrolled; restart the daemon to use it");
            Ok(())
        }
        Command::Scan => daemon::scan().await,
        Command::InstallHooks { settings } => {
            let path = settings
                .or_else(setup::default_settings_path)
                .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
            let exe = std::env::current_exe()?;
            setup::edit_settings_file(&path, |s| setup::with_hooks(s, &exe))?;
            eprintln!("hooks added to {}", path.display());
            Ok(())
        }
        Command::UninstallHooks { settings } => {
            let path = settings
                .or_else(setup::default_settings_path)
                .ok_or_else(|| anyhow::anyhow!("HOME is not set"))?;
            setup::edit_settings_file(&path, setup::without_hooks)?;
            eprintln!("hooks removed from {}", path.display());
            Ok(())
        }
    }
}

async fn print_reply(socket: &std::path::Path, request: Request) -> anyhow::Result<()> {
    let mut client = IpcClient::connect(socket).await?;
    let response = client.call(&request).await?;
    #[allow(clippy::print_stdout)]
    {
        println!("{}", serde_json::to_string_pretty(&response)?);
    }
    Ok(())
}

/// Hook budget: Claude waits for this process, so give up quickly.
const HOOK_DEADLINE: Duration = Duration::from_millis(500);

fn forward_hook() {
    let mut input = Vec::new();
    let limit = u64::try_from(HOOK_INPUT_MAX_BYTES + 1).unwrap_or(u64::MAX);
    if std::io::stdin()
        .take(limit)
        .read_to_end(&mut input)
        .is_err()
    {
        return;
    }
    let Ok(report) = hooks::parse_hook(&input) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    runtime.block_on(async {
        let Ok(dir) = ipc::runtime_dir() else { return };
        let send = async {
            let mut client = IpcClient::connect(&ipc::socket_path(&dir)).await.ok()?;
            client.call(&Request::Activity { report }).await.ok()
        };
        // An absent daemon or Flipper is normal; drop the event silently.
        let _ = tokio::time::timeout(HOOK_DEADLINE, send).await;
    });
}
