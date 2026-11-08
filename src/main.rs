//! Transparent click-through laser overlay on Hyprland's `overlay` layer.
//!
//! A small shared-memory surface renders a red dot at a controllable position.
//! The surface uses:
//! - `zwlr_layer_shell_v1` with `Layer::Overlay`, `KeyboardInteractivity::None`
//! - an empty input region (pointer passthrough)
//! - `wl_shm` for software rendering (no GPU dependency)
//!
//! Test control is via a small local UNIX socket so no networking is needed in
//! this phase.  The socket handler feeds the same command values the future
//! WebSocket command handler will use, so it can be replaced without touching
//! the rendering code.

mod error;
mod hyprland;
mod overlay;
mod wayland_probe;

use std::process::ExitCode;

use error::{Error, Result};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hyprpresenter: error: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("status") => cmd_status(),
        Some("probe") => cmd_probe(),
        Some("overlay") => {
            let rest = &args[1..];
            overlay::run(rest)
        }
        Some("--version") | Some("-V") => {
            println!("hyprpresenter {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => Err(Error::UnknownCommand(other.to_string())),
        None => {
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    println!(
        "hyprpresenter {} — presentation remote for Hyprland

USAGE:
  hyprpresenter status       health check: Hyprland session, version, monitors
  hyprpresenter probe        check which Wayland protocols the compositor offers
  hyprpresenter overlay      run the laser overlay (`overlay --help` for controls)
  hyprpresenter --version    print version

The phone remote, slide control, and networking are not implemented yet —\
see README.md for the planned architecture.",
        env!("CARGO_PKG_VERSION")
    );
}

fn cmd_status() -> Result<()> {
    let version = hyprland::version()?;
    let monitors = hyprland::monitors()?;

    println!("hyprpresenter {} — status", env!("CARGO_PKG_VERSION"));
    println!();
    println!(
        "  Hyprland : {} (branch {}, commit {})",
        version.version,
        version.branch,
        &version.commit[..7.min(version.commit.len())]
    );
    println!("  Monitors : {}", monitors.len());
    for m in &monitors {
        println!(
            "    - {:<8} {}x{} at ({},{}{}{})",
            m.name,
            m.width,
            m.height,
            m.x,
            m.y,
            if m.active { ", active" } else { "" },
            if m.focused { ", focused" } else { "" },
        );
    }
    Ok(())
}

fn cmd_probe() -> Result<()> {
    let globals = wayland_probe::run()?;
    println!(
        "hyprpresenter {} — Wayland probe",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!(
        "  compositor advertised {} globals; HyprPresenter needs:",
        globals.len()
    );
    println!("{}", wayland_probe::report(&globals));
    Ok(())
}
