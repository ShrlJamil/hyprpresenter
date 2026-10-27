//! Minimal client for Hyprland's UNIX-socket IPC (`.socket.sock`).
//!
//! Hyprland exposes one socket per session at
//! `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`.
//! A request is a single write of an ASCII command terminated by EOF
//! (`shutdown(SHUT_WR)`); the reply is the connection's remaining bytes.
//! Commands prefixed with `j/` return JSON.
//!
//! Only what Phase 3 needs is implemented: a raw `request()` plus the
//! `version` and `monitors` commands. Later phases (multi-monitor overlay
//! placement, active-output detection) can build directly on `request`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{Error, Result};

/// One output as reported by `hyprctl j/monitors`.
///
/// Only the fields HyprPresenter needs are deserialized; unknown fields are
/// ignored, so this keeps working across Hyprland releases that add fields.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // data model is shared; not every field is read yet
pub struct Monitor {
    pub id: i32,
    pub name: String,
    /// Current output resolution in layout pixels.
    pub width: i32,
    pub height: i32,
    /// Position in the global layout space (can be negative).
    pub x: i32,
    pub y: i32,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub active: bool,
}

/// Version info as reported by `hyprctl j/version`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // data model is shared; not every field is read yet
pub struct Version {
    pub branch: String,
    pub commit: String,
    pub version: String,
    #[serde(default)]
    pub tag: String,
}

/// Path to the IPC socket for the running Hyprland instance.
pub fn socket_path() -> Result<PathBuf> {
    let sig =
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").map_err(|_| Error::NoHyprlandSignature)?;
    let dir =
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| format!("/run/user/{}", current_uid()));
    Ok(PathBuf::from(dir)
        .join("hypr")
        .join(sig)
        .join(".socket.sock"))
}

/// Read the current uid from /proc without adding a `nix`/`libc` dependency.
/// Used only to guess `XDG_RUNTIME_DIR` when it is not exported.
fn current_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(1000)
}

/// Send one IPC command and return the raw reply.
pub fn request(command: &str) -> Result<String> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path).map_err(|e| Error::HyprlandSocket {
        path: path.display().to_string(),
        source: e,
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream
        .write_all(command.as_bytes())
        .and_then(|_| stream.shutdown(std::net::Shutdown::Write))
        .map_err(|e| Error::HyprlandSocket {
            path: path.display().to_string(),
            source: e,
        })?;

    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|e| Error::HyprlandSocket {
            path: path.display().to_string(),
            source: e,
        })?;

    // Hyprland prefixes genuine failures with "Invalid" / "invalid".
    if reply.starts_with("Invalid") {
        return Err(Error::HyprlandReply(reply.trim_end().to_string()));
    }
    Ok(reply)
}

/// `j/version` — used by `hyprpresenter status`.
pub fn version() -> Result<Version> {
    let reply = request("j/version")?;
    serde_json::from_str(&reply).map_err(Error::InvalidJson)
}

/// `j/monitors` — used by `hyprpresenter status` and, later, overlay placement.
pub fn monitors() -> Result<Vec<Monitor>> {
    let reply = request("j/monitors")?;
    serde_json::from_str(&reply).map_err(Error::InvalidJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed-down but realistic `j/monitors` reply. Parsing must tolerate
    /// extra/missing fields, because Hyprland adds fields between releases.
    #[test]
    fn parses_monitor_list() {
        let raw = r#"[
            {
                "id": 0, "name": "eDP-1", "description": "KDB 0x0702",
                "make": "KDB", "model": "0x0702", "serial": "",
                "width": 1920, "height": 1200,
                "physicalWidth": 300, "physicalHeight": 190,
                "refreshRate": 59.999, "x": -1536, "y": 120,
                "activeWorkspace": {"id": 1, "name": "1"},
                "focused": true, "active": true
            },
            {
                "id": 1, "name": "DP-1", "width": 1920, "height": 1080,
                "x": 0, "y": 0
            }
        ]"#;
        let monitors: Vec<Monitor> = serde_json::from_str(raw).expect("valid monitor json");
        assert_eq!(monitors.len(), 2);
        assert_eq!(monitors[0].name, "eDP-1");
        assert_eq!((monitors[0].x, monitors[0].y), (-1536, 120));
        assert!(monitors[0].focused);
        // Missing booleans default to false rather than failing.
        assert!(!monitors[1].focused);
        assert!(!monitors[1].active);
    }

    /// `j/version` reply shape, including a missing optional `tag`.
    #[test]
    fn parses_version() {
        let raw = r#"{
            "branch": "main", "commit": "abc123def456", "version": "0.56.2",
            "dirty": false, "commit_message": "x", "commit_date": "y",
            "tag": "v0.56.2", "commits": "7661"
        }"#;
        let v: Version = serde_json::from_str(raw).expect("valid version json");
        assert_eq!(v.version, "0.56.2");
        assert_eq!(v.tag, "v0.56.2");

        let minimal: Version = serde_json::from_str(r#"{"branch":"b","commit":"c","version":"1"}"#)
            .expect("version without tag parses");
        assert_eq!(minimal.tag, "");
    }

    /// `socket_path` must fail cleanly (exit code 2) outside a Hyprland
    /// session, and build the documented path inside one.
    #[test]
    fn socket_path_uses_env() {
        // No signature -> NoHyprlandSignature, whatever else is set.
        std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
        assert!(matches!(socket_path(), Err(Error::NoHyprlandSignature)));

        // With a signature it joins XDG_RUNTIME_DIR (or the /run/user fallback)
        // hypr/<sig>/.socket.sock.
        std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "test_sig");
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp/hp-test-runtime");
        let p = socket_path().expect("path with env set");
        assert_eq!(
            p,
            PathBuf::from("/tmp/hp-test-runtime/hypr/test_sig/.socket.sock")
        );

        // Restore for other tests / the running session.
        if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE_X") {
            std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", sig);
        }
    }
}
