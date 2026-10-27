//! `hyprpresenter` — presentation remote for Hyprland (desktop component).
//!
//! Current status (Phase 3): an inspectable, verifiable foundation.
//!   * `hyprpresenter status` — health check: who am I running under, which
//!     Hyprland instance, which outputs.
//!   * `hyprpresenter probe`  — Wayland protocol discovery: can this
//!     compositor host a layer-shell overlay and a virtual pointer?
//!
//! Networking, pairing, slide control, and the laser overlay itself are
//! deliberately NOT implemented yet (see README.md).

mod error;
mod hyprland;
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
        Some("--version") | Some("-V") => {
            println!("hyprpresenter {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => Err(Error::HyprlandReply(format!(
            "unknown command `{other}` (expected `status`, `probe`, or `--version`)"
        ))),
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
  hyprpresenter status     health check: Hyprland session, version, monitors
  hyprpresenter probe      check which Wayland protocols the compositor offers
  hyprpresenter --version  print version

The overlay, phone remote, and slide control are not implemented yet —
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
