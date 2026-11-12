//! Transparent click-through laser overlay on Hyprland's `overlay` layer.
//!
//! A small shared-memory surface renders a red dot at a controllable position.
//! The surface uses:
//! - `zwlr_layer_shell_v1` with `Layer::Overlay`, `KeyboardInteractivity::None`
//! - an empty input region (pointer passthrough)
//! - `wl_shm` for software rendering (no GPU dependency)
//!
//! Control arrives over a small local UNIX socket and, since Phase 5, a
//! LAN WebSocket server for paired phones.  Both handlers feed the same
//! command values into the command core, so transports can change without
//! touching the rendering code.

mod command;
mod error;
mod hyprland;
mod input;
mod net;
mod overlay;
mod wayland_probe;

use std::process::ExitCode;

use error::{Error, Result};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hypresent: error: {e}");
            ExitCode::from(e.exit_code() as u8)
        }
    }
}

fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("status") => cmd_status(),
        Some("probe") => cmd_probe(),
        Some("pair") => net::cmd_pair(&args[1..]),
        Some("overlay") => {
            let rest = &args[1..];
            overlay::run(rest)
        }
        Some("--version") | Some("-V") => {
            println!("hypresent {}", env!("CARGO_PKG_VERSION"));
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
        "hypresent {} — presentation remote for Hyprland

USAGE:
  hypresent status       health check: Hyprland session, version, monitors
  hypresent probe        check which Wayland protocols the compositor offers
  hypresent overlay      run the laser overlay (`overlay --help` for controls)
  hypresent pair         pair a phone (fresh QR + token; starts the overlay if needed)
  hypresent --version    print version

The overlay serves paired phones over LAN WebSocket; the Android app
lives in mobile/ in this repository — see README.md.",
        env!("CARGO_PKG_VERSION")
    );
}

fn cmd_status() -> Result<()> {
    let version = hyprland::version()?;
    let monitors = hyprland::monitors()?;

    println!("hypresent {} — status", env!("CARGO_PKG_VERSION"));
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
    println!("hypresent {} — Wayland probe", env!("CARGO_PKG_VERSION"));
    println!();
    println!(
        "  compositor advertised {} globals; hypresent needs:",
        globals.len()
    );
    println!("{}", wayland_probe::report(&globals));
    Ok(())
}
