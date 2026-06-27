//! Command-line surface: subcommand parsing and dispatch.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::config;
use crate::device;
use crate::install;

#[derive(Parser)]
#[command(
    name = "wayland-mouse",
    version,
    about = "Mac-like mouse acceleration for Wayland — pointer + scroll wheel."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,

    /// Print live wheel/pointer speeds for tuning (applies to the default run).
    #[arg(long, global = true)]
    debug: bool,

    /// Use a config file other than /etc/wayland-mouse/config.toml.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the daemon in the foreground (what the systemd service runs).
    Run,
    /// Install the binary, systemd service, config, and desktop integration.
    Install,
    /// Remove everything `install` added and restore desktop settings.
    Uninstall,
    /// Show service state and the effective config.
    Status,
    /// Print the evdev name of each mouse button as you press it (for [[button]] config).
    Buttons,
    /// Inspect the configuration.
    Config {
        /// Print the effective (resolved, DPI-rescaled) settings.
        #[arg(long)]
        print: bool,
        /// Validate syntax, keys, presets, and value ranges.
        #[arg(long)]
        check: bool,
    },
}

/// Parse args and dispatch. The daemon path never returns.
pub fn main() {
    let cli = Cli::parse();
    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(config::CONFIG_PATH));
    match cli.cmd {
        None | Some(Command::Run) => run_daemon(cli.debug, &cfg_path),
        Some(Command::Install) => std::process::exit(install::install()),
        Some(Command::Uninstall) => std::process::exit(install::uninstall()),
        Some(Command::Status) => std::process::exit(install::status()),
        Some(Command::Buttons) => std::process::exit(device::watch_buttons()),
        Some(Command::Config { print: _, check }) => {
            let code = if check {
                config::check(&cfg_path)
            } else {
                config::print_effective(&cfg_path)
            };
            std::process::exit(code);
        }
    }
}

fn run_daemon(debug: bool, path: &std::path::Path) {
    let mut cf = match config::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("wayland-mouse: config error: {e}");
            eprintln!("wayland-mouse: falling back to built-in defaults");
            config::ConfigFile::default()
        }
    };
    if !path.exists() {
        eprintln!(
            "wayland-mouse: no {} — using built-in defaults (preset = mac-like)",
            path.display()
        );
    }
    if debug {
        cf.debug = true;
    }
    device::run(Arc::new(cf)); // loops forever
}
