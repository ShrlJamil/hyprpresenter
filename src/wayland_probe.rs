//! Read-only Wayland protocol probe: connect to the compositor, list the
//! globals it advertises, and report which ones HyprPresenter's later phases
//! depend on (layer-shell for the overlay, virtual-pointer for remote input,
//! fractional-scale/viewporter for HiDPI rendering).
//!
//! This never creates a surface or binds anything — it is a pure discovery
//! step so `hyprpresenter probe` can answer "can the overlay work here?"
//! before Phase 4 commits to an implementation.

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::error::{Error, Result};

/// Interfaces HyprPresenter cares about, with the minimum version we need.
const NEEDED: &[(&str, u32, &str)] = &[
    ("zwlr_layer_shell_v1", 1, "overlay surface (laser pointer)"),
    (
        "zwlr_virtual_pointer_manager_v1",
        1,
        "remote prev/next + pointer injection",
    ),
    (
        "wp_fractional_scale_manager_v1",
        1,
        "fractional-scaling support",
    ),
    ("wp_viewporter", 1, "fractional-scaling support"),
    ("wl_seat", 1, "input seat"),
];

#[derive(Default)]
struct ProbeState {
    globals: Vec<(String, u32)>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name: _,
            interface,
            version,
        } = event
        {
            state.globals.push((interface, version));
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for ProbeState {
    fn event(
        _: &mut Self,
        _: &wl_output::WlOutput,
        _: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // We do not need output details here; Hyprland IPC covers that.
    }
}

/// Connect to the default Wayland display and collect advertised globals.
pub fn run() -> Result<Vec<(String, u32)>> {
    let conn =
        Connection::connect_to_env().map_err(|e| Error::Wayland(format!("connect_to_env: {e}")))?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    let _registry = display.get_registry(&qh, ());

    let mut state = ProbeState::default();
    // Two roundtrips: first delivers the registry globals, second flushes acks.
    queue
        .roundtrip(&mut state)
        .map_err(|e| Error::Wayland(format!("roundtrip: {e}")))?;
    queue
        .roundtrip(&mut state)
        .map_err(|e| Error::Wayland(format!("roundtrip: {e}")))?;

    Ok(state.globals)
}

/// Check the collected globals against `NEEDED` and return a human-readable
/// per-protocol status table.
pub fn report(globals: &[(String, u32)]) -> String {
    let mut lines = Vec::new();
    for (iface, min_ver, why) in NEEDED {
        match globals.iter().find(|(name, _)| name == iface) {
            Some((_, ver)) if ver >= min_ver => {
                lines.push(format!("  OK        {iface:<38} v{ver}  — {why}"));
            }
            Some((_, ver)) => {
                lines.push(format!(
                    "  TOO OLD   {iface:<38} v{ver} (need >={min_ver})  — {why}"
                ));
            }
            None => {
                lines.push(format!("  MISSING   {iface:<38}          — {why}"));
            }
        }
    }
    lines.join("\n")
}
