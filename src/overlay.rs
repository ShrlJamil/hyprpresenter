//! Transparent, click-through laser overlay on every output (Phase 4).
//!
//! The overlay creates one `wlr-layer-shell` surface per `wl_output`, each
//! sized to its whole output:
//!
//! * `KeyboardInteractivity::None` (never take keyboard focus)
//! * an explicitly empty input region (pointer events pass through)
//! * one `wl_shm` ARGB8888 buffer per surface, redrawn on demand, with an
//!   independent lifecycle per surface so one slow output cannot stall the
//!   others
//! * `wp_fractional_scale_v1` + `wp_viewport` when available, so a 1.25x
//!   output is rendered at full physical resolution instead of being
//!   stretched from logical pixels
//!
//! The dot position lives in the transport-agnostic core (`command.rs`) in
//! **global logical coordinates**: the union layout comes from Hyprland IPC
//! (`j/monitors`), and each surface converts the global point into its own
//! local coordinates when drawing.  A surface whose output does not contain
//! the dot draws a fully transparent frame.
//!
//! Test control is deliberately a tiny, removable local UNIX socket.  The
//! socket handler is a thin adapter over the command core; the WebSocket
//! network layer only has to feed the same [`Command`] values into it.

use std::{
    fs,
    io::{Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{
            self,
            channel::{channel, Event as ChannelEvent},
            EventLoop,
        },
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    viewporter::client::{wp_viewport, wp_viewporter},
};

use crate::{
    command::{
        parse_color, parse_command, parse_radius, Action, Appearance, Command, LaserState, Layout,
        OutputRect,
    },
    error::{Error, Result},
    hyprland,
    input::{Input, NavKeys, PresentationKey},
};

/// Environment override for the control socket, mainly for automated tests
/// and side-by-side runs.
const SOCKET_ENV: &str = "HYPRPRESENTER_SOCKET";

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => run_overlay(Options::default()),
        Some("run") => run_overlay(Options::parse(&args[1..])?),
        Some("move") => {
            if args.len() != 3 {
                return Err(Error::Overlay("usage: hypresent overlay move X Y".into()));
            }
            let x = parse_i32("X", &args[1])?;
            let y = parse_i32("Y", &args[2])?;
            send_control(&format!("move {x} {y}"))
        }
        Some("center") => send_control("center"),
        Some("show") => send_control("show"),
        Some("hide") => send_control("hide"),
        Some("radius") => {
            if args.len() != 2 {
                return Err(Error::Overlay("usage: hypresent overlay radius N".into()));
            }
            let radius = parse_radius(&args[1]).map_err(Error::Overlay)?;
            send_control(&format!("radius {radius}"))
        }
        Some("color") => {
            if args.len() != 2 {
                return Err(Error::Overlay(
                    "usage: hypresent overlay color NAME|#RRGGBB".into(),
                ));
            }
            let color = parse_color(&args[1]).map_err(Error::Overlay)?;
            send_control(&format!("color {}", color.to_hex()))
        }
        Some("previous") | Some("prev") => send_control("previous"),
        Some("next") => send_control("next"),
        Some("confine") => {
            if args.len() != 2 {
                return Err(Error::Overlay(
                    "usage: hypresent overlay confine OUTPUT|off".into(),
                ));
            }
            send_control(&format!("confine {}", args[1]))
        }
        Some("status") => send_control("status"),
        Some("quit") => send_control("quit"),
        Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some(first) if first.starts_with('-') => run_overlay(Options::parse(args)?),
        Some(other) => Err(Error::Overlay(format!(
            "unknown overlay action `{other}` (try `hypresent overlay --help`)"
        ))),
    }
}

fn print_help() {
    println!(
        "hypresent {} — laser overlay

USAGE:
  hypresent overlay [OPTIONS]     run the overlay (all outputs)
  hypresent overlay status        report outputs, layout, buffer and dot state
  hypresent overlay move X Y      move the dot (global logical pixels)
  hypresent overlay center        move the dot to the focused output's center
  hypresent overlay hide          render empty (transparent) surfaces
  hypresent overlay show          draw the dot again
  hypresent overlay radius N      set dot radius, {}-{} logical pixels
  hypresent overlay color C       set dot color (name or #RRGGBB)
  hypresent overlay previous      send the previous key to the focused application
  hypresent overlay next          send the next key to the focused application
  hypresent overlay confine O|off keep the dot on output O (or release it)
  hypresent overlay quit          shut the overlay down cleanly

OPTIONS:
  --output NAME      draw only on this wl_output (default: every output)
  --x N --y N        initial dot position in global logical pixels
  --radius N         dot radius in logical pixels (default: {})
  --color C          initial dot color (name or #RRGGBB, default: red)
  --previous-key N   evdev keycode for `previous` (default: 104, PageUp)
  --next-key N       evdev keycode for `next` (default: 109, PageDown)
  --ws-port N        WebSocket listen port for paired phones (default: 8423)
  --no-ws            disable the WebSocket server (local socket only)
  -h, --help         show this help

Named colors: red, green, blue, yellow, cyan, magenta, white, orange.
Hex colors are #RRGGBB; shell users must quote the leading `#`.

Coordinates are global logical pixels across all outputs: the layout comes
from Hyprland IPC (j/monitors) and each output's surface converts them to
its own local pixels.  `status` reports which output the dot is on.

The controls use a local UNIX socket in $XDG_RUNTIME_DIR (override with
${SOCKET_ENV}) and are intentionally temporary; they will be replaced by the
network command handler in a later phase.",
        env!("CARGO_PKG_VERSION"),
        crate::command::MIN_RADIUS,
        crate::command::MAX_RADIUS,
        crate::command::DEFAULT_RADIUS,
    );
}

#[derive(Debug, Clone)]
struct Options {
    /// Restrict drawing to one output; `None` draws on every output.
    output: Option<String>,
    x: Option<i32>,
    y: Option<i32>,
    appearance: Appearance,
    nav_keys: NavKeys,
    /// WebSocket listen port; `None` disables the network server.
    ws_port: Option<u16>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            output: None,
            x: None,
            y: None,
            appearance: Appearance::default(),
            nav_keys: NavKeys::default(),
            ws_port: Some(crate::net::DEFAULT_WS_PORT),
        }
    }
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut options = Self::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--output" => {
                    i += 1;
                    options.output = Some(
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--output requires a name".into()))?
                            .clone(),
                    );
                }
                "--x" => {
                    i += 1;
                    options.x = Some(parse_i32(
                        "--x",
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--x requires a value".into()))?,
                    )?);
                }
                "--y" => {
                    i += 1;
                    options.y = Some(parse_i32(
                        "--y",
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--y requires a value".into()))?,
                    )?);
                }
                "--radius" => {
                    i += 1;
                    options.appearance.radius = parse_radius(
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--radius requires a value".into()))?,
                    )
                    .map_err(Error::Overlay)?;
                }
                "--color" => {
                    i += 1;
                    options.appearance.color = parse_color(
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--color requires a value".into()))?,
                    )
                    .map_err(Error::Overlay)?;
                }
                "--previous-key" => {
                    i += 1;
                    options.nav_keys.previous = parse_keycode(
                        "--previous-key",
                        args.get(i).ok_or_else(|| {
                            Error::Overlay("--previous-key requires a value".into())
                        })?,
                    )?;
                }
                "--next-key" => {
                    i += 1;
                    options.nav_keys.next = parse_keycode(
                        "--next-key",
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--next-key requires a value".into()))?,
                    )?;
                }
                "--ws-port" => {
                    i += 1;
                    let raw = args
                        .get(i)
                        .ok_or_else(|| Error::Overlay("--ws-port requires a value".into()))?;
                    let port: u16 = raw.parse().map_err(|_| {
                        Error::Overlay(format!("--ws-port must be a port number, got `{raw}`"))
                    })?;
                    if port == 0 {
                        return Err(Error::Overlay("--ws-port must not be 0".into()));
                    }
                    options.ws_port = Some(port);
                }
                "--no-ws" => {
                    options.ws_port = None;
                }
                other => {
                    return Err(Error::Overlay(format!(
                        "unknown overlay option `{other}` (try `hypresent overlay --help`)"
                    )))
                }
            }
            i += 1;
        }
        if options.x.is_some() != options.y.is_some() {
            return Err(Error::Overlay(
                "--x and --y must be supplied together".into(),
            ));
        }
        Ok(options)
    }
}

fn parse_i32(what: &str, raw: &str) -> Result<i32> {
    raw.parse()
        .map_err(|_| Error::Overlay(format!("{what} must be an integer, got `{raw}`")))
}

fn parse_keycode(what: &str, raw: &str) -> Result<u32> {
    let code: u32 = raw
        .parse()
        .map_err(|_| Error::Overlay(format!("{what} must be an evdev keycode, got `{raw}`")))?;
    if code == 0 {
        return Err(Error::Overlay(format!(
            "{what} must be a positive evdev keycode, got 0"
        )));
    }
    Ok(code)
}

// ---------------------------------------------------------------------------
// Local test control (thin adapter over the command core)
// ---------------------------------------------------------------------------

fn control_socket_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var(SOCKET_ENV) {
        return Ok(PathBuf::from(path));
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| {
        Error::Overlay(format!(
            "XDG_RUNTIME_DIR is not set (or set {SOCKET_ENV} to choose a socket path)"
        ))
    })?;
    Ok(PathBuf::from(runtime).join("hyprpresenter-overlay.sock"))
}

fn send_control(command: &str) -> Result<()> {
    let path = control_socket_path()?;
    let mut stream = UnixStream::connect(&path).map_err(|e| {
        Error::Overlay(format!(
            "cannot connect to overlay socket {}: {e} (is the overlay running?)",
            path.display()
        ))
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{command}")
        .and_then(|_| stream.shutdown(Shutdown::Write))
        .map_err(|e| Error::Overlay(format!("failed to send control command: {e}")))?;

    let mut reply = String::new();
    stream
        .read_to_string(&mut reply)
        .map_err(|e| Error::Overlay(format!("failed to read control reply: {e}")))?;
    let reply = reply.trim();
    if reply.is_empty() {
        return Err(Error::Overlay(
            "overlay closed the control connection without a reply".into(),
        ));
    }
    if reply.starts_with("error") {
        return Err(Error::Overlay(reply.to_string()));
    }
    println!("{reply}");
    Ok(())
}

/// One parsed command plus the channel its reply goes to.  Shared by both
/// transports: the local socket handler and the WebSocket server (`net.rs`)
/// forward these into the event loop identically.
pub(crate) struct ControlRequest {
    pub(crate) command: Command,
    /// Where the reply goes; `None` marks a fire-and-forget command
    /// (streaming laser moves over WebSocket) that is never answered.
    pub(crate) reply: Option<mpsc::Sender<String>>,
}

/// Owns the control listener thread; dropping it stops the thread, wakes a
/// blocking `accept()`, and removes the socket file.
struct ControlServer {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl ControlServer {
    fn start(
        path: PathBuf,
        listener: UnixListener,
        requests: calloop::channel::Sender<ControlRequest>,
    ) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let handle = thread::Builder::new()
            .name("hyprpresenter-control".into())
            .spawn(move || control_loop(listener, requests, thread_shutdown))
            .map_err(|e| Error::Overlay(format!("failed to start control thread: {e}")))?;
        Ok(Self {
            path,
            shutdown,
            handle: Some(handle),
        })
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // A blocking accept() will not notice the flag until a connection
        // arrives; this dummy connection is the wakeup.
        let _ = UnixStream::connect(&self.path);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

fn bind_control_socket(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(Error::Overlay(format!(
                "another overlay is already listening on {}",
                path.display()
            )));
        }
        // Only remove a stale socket.  Never delete an unrelated user file.
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_socket() {
            fs::remove_file(path)?;
        } else {
            return Err(Error::Overlay(format!(
                "{} exists and is not a socket",
                path.display()
            )));
        }
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| Error::Overlay(format!("cannot bind {}: {e}", path.display())))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn control_loop(
    listener: UnixListener,
    requests: calloop::channel::Sender<ControlRequest>,
    shutdown: Arc<AtomicBool>,
) {
    for connection in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = connection else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));

        let mut raw = String::new();
        if let Err(e) = stream.read_to_string(&mut raw) {
            let _ = writeln!(stream, "error: failed to read command: {e}");
            continue;
        }

        let command = match parse_command(raw.trim()) {
            Ok(command) => command,
            Err(e) => {
                let _ = writeln!(stream, "error: {e}");
                continue;
            }
        };

        let (reply_tx, reply_rx) = mpsc::channel();
        if requests
            .send(ControlRequest {
                command,
                reply: Some(reply_tx),
            })
            .is_err()
        {
            let _ = writeln!(stream, "error: overlay is shutting down");
            break;
        }

        match reply_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(reply) => {
                let _ = writeln!(stream, "{reply}");
            }
            Err(_) => {
                let _ = writeln!(stream, "error: overlay did not respond in time");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wayland overlay (multi-surface)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleMode {
    /// Scale is 120ths, rendered through a viewport.
    Fractional(u32),
    /// Integer buffer scale, no viewport.
    Integer(u32),
}

/// One output's layer surface plus its private buffer lifecycle.  Surfaces
/// share only the immutable-per-frame inputs (layout + laser state); pools,
/// buffers, and redraw flags are per surface, so a compositor that is slow
/// to release one output's buffer never blocks another output's redraw.
struct OutputSurface {
    output: wl_output::WlOutput,
    name: String,
    layer: LayerSurface,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    pool: Option<SlotPool>,
    /// The single `wl_shm` buffer, reused for every frame on this surface.
    /// Reusing one slot keeps memory bounded; a redraw is deferred until
    /// the compositor sends `wl_buffer.release` for the previous commit.
    buffer: Option<Buffer>,
    /// Logical size from the layer-shell configure event.
    size: Option<(u32, u32)>,
    buffer_size: Option<(u32, u32)>,
    fractional_scale_120: Option<u32>,
    integer_scale: i32,
    configured: bool,
    needs_redraw: bool,
}

impl OutputSurface {
    fn scale_mode(&self) -> ScaleMode {
        match (self.fractional_scale_120, self.viewport.as_ref()) {
            (Some(scale_120), Some(_)) if scale_120 > 0 => ScaleMode::Fractional(scale_120),
            _ => ScaleMode::Integer(self.integer_scale.max(1) as u32),
        }
    }

    fn buffer_geometry(&self, width: u32, height: u32) -> (u32, u32, ScaleMode) {
        let mode = self.scale_mode();
        match mode {
            ScaleMode::Fractional(scale_120) => (
                scale_dimension(width, scale_120),
                scale_dimension(height, scale_120),
                mode,
            ),
            ScaleMode::Integer(scale) => (width * scale, height * scale, mode),
        }
    }

    fn scale_factor(&self) -> f64 {
        match self.scale_mode() {
            ScaleMode::Fractional(scale_120) => scale_120 as f64 / 120.0,
            ScaleMode::Integer(scale) => scale as f64,
        }
    }
}

struct Overlay {
    conn: Connection,

    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    fractional_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,

    surfaces: Vec<OutputSurface>,
    layout: Layout,
    state: LaserState,
    nav_keys: NavKeys,

    /// Virtual keyboard used for `previous`/`next`.  `None` means navigation
    /// is unavailable; `input_error` explains why without stopping the laser.
    input: Option<Input>,
    input_error: Option<String>,

    exit: bool,
    fatal: Option<String>,
}

fn run_overlay(options: Options) -> Result<()> {
    let socket_path = control_socket_path()?;
    let listener = bind_control_socket(&socket_path)?;
    let (request_tx, request_rx) = channel::<ControlRequest>();
    let net_tx = request_tx.clone();
    let control_server = ControlServer::start(socket_path, listener, request_tx)?;

    // The WebSocket server (Phase 5) is a second adapter over the very same
    // channel: both transports feed ControlRequest values to the event loop
    // and differ only in wire format and authentication.  A bind failure
    // degrades to socket-only operation instead of killing the laser, the
    // same philosophy as the virtual-keyboard fallback.
    let _net_server = match options.ws_port {
        Some(port) => match crate::net::NetServer::start(port, net_tx) {
            Ok(server) => {
                eprintln!(
                    "hypresent: websocket server on ws://0.0.0.0:{port} (pairing token required)"
                );
                Some(server)
            }
            Err(e) => {
                eprintln!("hypresent: websocket server unavailable: {e}");
                None
            }
        },
        None => None,
    };

    // Keyboard input gets its own Wayland connection: if the compositor
    // rejects the virtual keyboard, the laser overlay still works and the
    // navigation commands report the exact limitation.
    let (input, input_error) = match Input::connect() {
        Ok(input) => (Some(input), None),
        Err(e) => (None, Some(e)),
    };

    let conn =
        Connection::connect_to_env().map_err(|e| Error::Wayland(format!("connect_to_env: {e}")))?;
    let (globals, mut event_queue) =
        registry_queue_init(&conn).map_err(|e| Error::Wayland(format!("registry init: {e}")))?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|e| Error::Wayland(format!("wl_compositor unavailable: {e}")))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|e| {
        Error::Wayland(format!(
            "zwlr_layer_shell_v1 unavailable (wlr-layer-shell is required): {e}"
        ))
    })?;
    let shm =
        Shm::bind(&globals, &qh).map_err(|e| Error::Wayland(format!("wl_shm unavailable: {e}")))?;
    let output_state = OutputState::new(&globals, &qh);
    let registry_state = RegistryState::new(&globals);
    let fractional_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1> =
        globals.bind(&qh, 1..=1, ()).ok();
    let viewporter: Option<wp_viewporter::WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();

    let mut state = LaserState {
        appearance: options.appearance,
        ..LaserState::default()
    };
    if let (Some(x), Some(y)) = (options.x, options.y) {
        state.pos = Some((x, y));
        state.center_mode = false;
    }

    let mut app = Overlay {
        conn: conn.clone(),
        registry_state,
        output_state,
        compositor,
        layer_shell,
        shm,
        fractional_manager,
        viewporter,
        surfaces: Vec::new(),
        layout: Layout::default(),
        state,
        nav_keys: options.nav_keys,
        input,
        input_error,
        exit: false,
        fatal: None,
    };

    // First roundtrip populates wl_output / xdg-output information so the
    // outputs can be enumerated and matched against the Hyprland layout.
    event_queue
        .roundtrip(&mut app)
        .map_err(|e| Error::Wayland(format!("initial output roundtrip: {e}")))?;
    app.layout = app.build_layout(options.output.as_deref())?;
    if app.state.center_mode {
        if !app.layout.outputs.is_empty() {
            app.state.pos = Some(app.layout.center());
        }
    } else if let Some((x, y)) = app.state.pos {
        app.state.pos = Some(app.layout.clamp(x, y));
    }
    app.create_surfaces(&qh, options.output.as_deref())?;

    // Initial commits (no buffer) ask layer-shell for configures.  A second
    // roundtrip lets the configure handlers allocate and attach the first
    // buffers before entering the long-running event loop.
    event_queue
        .roundtrip(&mut app)
        .map_err(|e| Error::Wayland(format!("initial configure roundtrip: {e}")))?;
    if let Some(fatal) = &app.fatal {
        return Err(Error::Overlay(fatal.clone()));
    }

    let mut event_loop: EventLoop<Overlay> = EventLoop::try_new()
        .map_err(|e| Error::Overlay(format!("failed to create event loop: {e}")))?;
    let handle = event_loop.handle();
    WaylandSource::new(conn, event_queue)
        .insert(handle.clone())
        .map_err(|e| Error::Overlay(format!("failed to insert Wayland source: {e}")))?;
    handle
        .insert_source(request_rx, |event, _, app| {
            if let ChannelEvent::Msg(request) = event {
                let reply = app.apply_command(request.command);
                if let Some(reply_tx) = &request.reply {
                    let _ = reply_tx.send(reply);
                }
            }
        })
        .map_err(|e| Error::Overlay(format!("failed to insert control channel: {e}")))?;

    while !app.exit && app.fatal.is_none() {
        event_loop
            .dispatch(None, &mut app)
            .map_err(|e| Error::Overlay(format!("event loop error: {e}")))?;
        // Commands only record state; draw once per batch so a burst of
        // moves cannot allocate a buffer per message.  Each surface draws
        // independently; a surface whose previous buffer is still in use
        // keeps its flag set and retries after the next dispatch (which
        // delivers `wl_buffer.release`).
        for idx in 0..app.surfaces.len() {
            if app.surfaces[idx].needs_redraw && !app.exit && app.fatal.is_none() {
                app.surfaces[idx].needs_redraw = false;
                if let Err(e) = app.draw_surface(idx) {
                    app.fatal = Some(e.to_string());
                    break;
                }
            }
        }
    }

    drop(control_server);
    if let Some(fatal) = app.fatal {
        return Err(Error::Overlay(fatal));
    }
    Ok(())
}

impl Overlay {
    /// Build the global layout from Hyprland IPC (`j/monitors`), matched
    /// to the compositor's `wl_output`s by name.  Outputs the IPC does not
    /// know fall back to the xdg-output logical position/size; outputs with
    /// neither are laid out only after their first configure (see the
    /// configure handler).
    fn build_layout(&mut self, only: Option<&str>) -> Result<Layout> {
        let monitors = hyprland::monitors().unwrap_or_default();
        let focused = monitors.iter().find(|m| m.focused).map(|m| m.name.clone());

        let mut rects = Vec::new();
        let mut available = Vec::new();
        for output in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&output) else {
                continue;
            };
            let label = info
                .name
                .clone()
                .or_else(|| info.description.clone())
                .unwrap_or_else(|| format!("<id {}>", info.id));
            available.push(label);
            let Some(name) = info.name.clone() else {
                continue;
            };
            if let Some(only) = only {
                if name != only {
                    continue;
                }
            }
            let rect = if let Some(monitor) = monitors.iter().find(|m| m.name == name) {
                let (w, h) = monitor.logical_size();
                OutputRect {
                    name: name.clone(),
                    x: monitor.x,
                    y: monitor.y,
                    width: w,
                    height: h,
                }
            } else if let (Some((x, y)), Some((w, h))) = (info.logical_position, info.logical_size)
            {
                if w <= 0 || h <= 0 {
                    continue;
                }
                OutputRect {
                    name: name.clone(),
                    x,
                    y,
                    width: w as u32,
                    height: h as u32,
                }
            } else {
                continue;
            };
            rects.push(rect);
        }

        if let Some(only) = only {
            if rects.is_empty() {
                return Err(Error::Overlay(format!(
                    "output `{only}` not found (available: {})",
                    if available.is_empty() {
                        "none reported".to_string()
                    } else {
                        available.join(", ")
                    }
                )));
            }
        }
        let focused = focused.filter(|name| rects.iter().any(|r| &r.name == name));
        Ok(Layout::new(rects, focused))
    }

    fn create_surfaces(&mut self, qh: &QueueHandle<Self>, only: Option<&str>) -> Result<()> {
        let mut created = 0usize;
        for output in self.output_state.outputs() {
            let Some(info) = self.output_state.info(&output) else {
                continue;
            };
            let Some(name) = info.name.clone() else {
                continue;
            };
            if let Some(only) = only {
                if name != only {
                    continue;
                }
            }
            // With an --output filter the layout was already checked and
            // errors out for unknown names; without one, every named output
            // gets a surface.
            let surface = self.compositor.create_surface(qh);
            let layer = self.layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Overlay,
                Some("hyprpresenter-laser"),
                Some(&output),
            );

            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            // With opposite edges anchored, a zero size means "full output";
            // the actual logical size arrives in the configure event.
            layer.set_size(0, 0);
            // -1 makes the layer ignore other surfaces' exclusive zones (e.g.
            // a 36px top bar) and extend to the output edges.  The surface is
            // transparent, so it does not visually cover the panel; it just
            // allows the dot to reach every pixel of the monitor.
            layer.set_exclusive_zone(-1);
            layer.set_margin(0, 0, 0, 0);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);

            // Explicitly empty input region: no pointer events are delivered
            // to this surface, so clicks go to whatever is underneath it.
            let region = Region::new(&self.compositor)
                .map_err(|e| Error::Wayland(format!("failed to create empty input region: {e}")))?;
            layer.set_input_region(Some(region.wl_region()));
            drop(region);

            let fractional = self
                .fractional_manager
                .as_ref()
                .map(|manager| manager.get_fractional_scale(layer.wl_surface(), qh, ()));
            let viewport = self
                .viewporter
                .as_ref()
                .map(|viewporter| viewporter.get_viewport(layer.wl_surface(), qh, ()));

            layer.commit();
            self.surfaces.push(OutputSurface {
                output,
                name,
                layer,
                fractional,
                viewport,
                pool: None,
                buffer: None,
                size: None,
                buffer_size: None,
                fractional_scale_120: None,
                integer_scale: info.scale_factor.max(1),
                configured: false,
                needs_redraw: false,
            });
            created += 1;
        }
        self.conn
            .flush()
            .map_err(|e| Error::Wayland(format!("flush initial surface commits: {e}")))?;
        if created == 0 {
            return Err(Error::Overlay(
                "no outputs available for the overlay".into(),
            ));
        }
        Ok(())
    }

    /// The layout rect a surface draws against.  Falls back to a
    /// surface-local rect at the origin when the layout does not know the
    /// output (the configure handler adds such outputs when they report a
    /// size, so this is only a transient state).
    fn surface_rect(&self, surface: &OutputSurface) -> OutputRect {
        self.layout
            .outputs
            .iter()
            .find(|r| r.name == surface.name)
            .cloned()
            .unwrap_or(OutputRect {
                name: surface.name.clone(),
                x: 0,
                y: 0,
                width: surface.size.map(|(w, _)| w).unwrap_or(0),
                height: surface.size.map(|(_, h)| h).unwrap_or(0),
            })
    }

    fn draw_surface(&mut self, idx: usize) -> Result<()> {
        // Compute everything that depends on shared state first, so the
        // mutable borrow of the surface below stays conflict-free.
        let rect = self.surface_rect(&self.surfaces[idx]);
        let dot_local = if self.state.visible {
            self.state.pos.and_then(|(gx, gy)| rect.to_local(gx, gy))
        } else {
            None
        };
        let appearance = self.state.appearance;

        let surface = &mut self.surfaces[idx];
        if !surface.configured {
            return Ok(());
        }
        let (width, height) = match surface.size {
            Some(size) => size,
            None => return Ok(()),
        };
        if width == 0 || height == 0 {
            return Ok(());
        }

        let (buffer_width, buffer_height, mode) = surface.buffer_geometry(width, height);
        let stride = (buffer_width as i32)
            .checked_mul(4)
            .ok_or_else(|| Error::Overlay("buffer stride overflow".into()))?;
        let len = (stride as usize)
            .checked_mul(buffer_height as usize)
            .ok_or_else(|| Error::Overlay("buffer size overflow".into()))?;

        if surface.pool.is_none() {
            surface.pool = Some(
                SlotPool::new(len, &self.shm)
                    .map_err(|e| Error::Overlay(format!("failed to create shm pool: {e}")))?,
            );
        }

        if surface.buffer.is_none() || surface.buffer_size != Some((buffer_width, buffer_height)) {
            // Drop a stale buffer first so its slot returns to the pool
            // before the new allocation (if the compositor released it).
            surface.buffer = None;
            surface.buffer_size = None;
            let (buffer, _) = surface
                .pool
                .as_mut()
                .expect("pool was just created")
                .create_buffer(
                    buffer_width as i32,
                    buffer_height as i32,
                    stride,
                    wl_shm::Format::Argb8888,
                )
                .map_err(|e| Error::Overlay(format!("failed to allocate shm buffer: {e}")))?;
            surface.buffer = Some(buffer);
        }

        let buffer = surface.buffer.take().expect("buffer was just created");
        if buffer.slot().has_active_buffers() {
            // The compositor has not released the previous commit yet.  Keep
            // the buffer and retry after `wl_buffer.release`; allocating a
            // new slot here is what made rapid moves grow the pool without
            // bound in the single-surface version.
            surface.buffer = Some(buffer);
            surface.needs_redraw = true;
            return Ok(());
        }

        let canvas = surface
            .pool
            .as_mut()
            .expect("pool was just created")
            .canvas(&buffer)
            .ok_or_else(|| Error::Overlay("shm buffer is unexpectedly busy".into()))?;

        canvas.fill(0);
        if let Some((x, y)) = dot_local {
            let scale_x = buffer_width as f64 / width as f64;
            let scale_y = buffer_height as f64 / height as f64;
            // Pixel centres: logical pixel (x, y) covers [x,x+1)x[y,y+1).
            let center_x = (x as f64 + 0.5) * scale_x;
            let center_y = (y as f64 + 0.5) * scale_y;
            let radius = appearance.radius as f64 * (scale_x + scale_y) / 2.0;
            paint_dot(
                canvas,
                buffer_width,
                buffer_height,
                center_x,
                center_y,
                radius,
                appearance.color,
            );
        }

        match mode {
            ScaleMode::Fractional(_) => {
                if let Some(viewport) = &surface.viewport {
                    viewport.set_destination(width as i32, height as i32);
                }
                surface
                    .layer
                    .set_buffer_scale(1)
                    .map_err(|_| Error::Overlay("wl_surface v3 is required".into()))?;
            }
            ScaleMode::Integer(scale) => {
                if let Some(viewport) = &surface.viewport {
                    viewport.set_destination(-1, -1);
                }
                surface
                    .layer
                    .set_buffer_scale(scale)
                    .map_err(|_| Error::Overlay("wl_surface v3 is required".into()))?;
            }
        }

        buffer
            .attach_to(surface.layer.wl_surface())
            .map_err(|e| Error::Overlay(format!("failed to attach shm buffer: {e}")))?;
        surface
            .layer
            .wl_surface()
            .damage(0, 0, width as i32, height as i32);
        surface.layer.commit();
        self.conn
            .flush()
            .map_err(|e| Error::Wayland(format!("flush surface commit: {e}")))?;

        surface.buffer = Some(buffer);
        surface.buffer_size = Some((buffer_width, buffer_height));
        Ok(())
    }

    /// Adapter between the transport and the command core: state-changing
    /// commands go to the core; this layer only executes the side effects
    /// (navigation keys, shutdown) and composes the session-rich `status`
    /// reply around the core's state line.
    fn apply_command(&mut self, command: Command) -> String {
        if matches!(command, Command::Status) {
            return self.status_line();
        }
        let outcome = self.state.apply(command, &self.layout);
        if outcome.redraw {
            for surface in &mut self.surfaces {
                surface.needs_redraw = true;
            }
        }
        match outcome.action {
            None => outcome.reply,
            Some(Action::Quit) => {
                self.exit = true;
                outcome.reply
            }
            Some(Action::Navigate(key)) => self.navigate(key),
        }
    }

    fn navigate(&mut self, key: PresentationKey) -> String {
        let result = match self.input.as_mut() {
            Some(input) => input.tap(key, self.nav_keys),
            None => {
                let reason = self.input_error.as_deref().unwrap_or("not initialised");
                return format!("error: virtual keyboard unavailable: {reason}");
            }
        };
        match result {
            Ok(()) => {
                if self.nav_keys == NavKeys::default() {
                    format!("ok {} key={}", key.command_name(), key.key_name())
                } else {
                    // With a custom mapping the PageUp/PageDown names would
                    // be wrong; report the evdev code that was injected.
                    format!(
                        "ok {} keycode={}",
                        key.command_name(),
                        key.evdev_code(self.nav_keys)
                    )
                }
            }
            Err(e) => {
                // A protocol error poisons the connection; keep the laser
                // running and report the failure on subsequent attempts.
                self.input = None;
                self.input_error = Some(e.clone());
                format!("error: {e}")
            }
        }
    }

    fn status_line(&self) -> String {
        let names = self
            .surfaces
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let buffers = self
            .surfaces
            .iter()
            .map(|s| {
                let (bw, bh) = s.buffer_size.unwrap_or((0, 0));
                format!("{}:{}x{}@{:.3}", s.name, bw, bh, s.scale_factor())
            })
            .collect::<Vec<_>>()
            .join(",");
        let configured = self.surfaces.iter().filter(|s| s.configured).count();
        format!(
            "outputs={} {} keyboard={} buffers={} configured={}/{}",
            names,
            self.state.state_line(&self.layout),
            if self.input.is_some() {
                "ready"
            } else {
                "unavailable"
            },
            buffers,
            configured,
            self.surfaces.len(),
        )
    }
}

fn scale_dimension(logical: u32, scale_120: u32) -> u32 {
    // Round half away from zero, matching the fractional-scale protocol's
    // toplevel rounding recommendation.
    ((logical as u64 * scale_120 as u64 + 60) / 120).max(1) as u32
}

/// Paint an anti-aliased dot into a premultiplied ARGB8888 buffer.
fn paint_dot(
    canvas: &mut [u8],
    buffer_width: u32,
    buffer_height: u32,
    center_x: f64,
    center_y: f64,
    radius: f64,
    color: crate::command::Rgb,
) {
    if buffer_width == 0 || buffer_height == 0 || radius <= 0.0 {
        return;
    }
    let radius = radius.max(1.0);
    let min_x = (center_x - radius - 1.0).floor().max(0.0) as u32;
    let max_x = (center_x + radius + 1.0)
        .ceil()
        .min(buffer_width as f64 - 1.0) as u32;
    let min_y = (center_y - radius - 1.0).floor().max(0.0) as u32;
    let max_y = (center_y + radius + 1.0)
        .ceil()
        .min(buffer_height as f64 - 1.0) as u32;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f64 + 0.5 - center_x;
            let dy = y as f64 + 0.5 - center_y;
            let coverage = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            if coverage <= 0.0 {
                continue;
            }
            let alpha = (coverage * 255.0).round() as u32;
            // Premultiplied ARGB8888, so the transparent parts of the buffer
            // stay genuinely transparent and the surface remains click-through.
            let premultiply = |channel: u8| (channel as u32 * alpha + 127) / 255;
            let pixel = (alpha << 24)
                | (premultiply(color.r) << 16)
                | (premultiply(color.g) << 8)
                | premultiply(color.b);
            let offset = ((y as usize * buffer_width as usize) + x as usize) * 4;
            canvas[offset..offset + 4].copy_from_slice(&pixel.to_ne_bytes());
        }
    }
}

impl ProvidesRegistryState for Overlay {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState];
}

impl CompositorHandler for Overlay {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        let Some(idx) = self
            .surfaces
            .iter()
            .position(|s| s.layer.wl_surface().id() == surface.id())
        else {
            return;
        };
        self.surfaces[idx].integer_scale = new_factor.max(1);
        // A fractional scale is more precise; only re-render when the
        // integer fallback is actually in use on this surface.
        if matches!(self.surfaces[idx].scale_mode(), ScaleMode::Integer(_))
            && self.surfaces[idx].configured
        {
            self.surfaces[idx].needs_redraw = true;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // Rotated outputs are not specially handled: the buffer is drawn in
        // the surface's logical orientation, as in the single-output proof
        // of concept.
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Overlay {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // Surfaces are created once at startup from the enumerated outputs;
        // hot-plug after startup is not handled in this phase.
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let Some(idx) = self.surfaces.iter().position(|s| s.output == output) else {
            return;
        };
        if let Some(info) = self.output_state.info(&output) {
            self.surfaces[idx].integer_scale = info.scale_factor.max(1);
        }
        if matches!(self.surfaces[idx].scale_mode(), ScaleMode::Integer(_))
            && self.surfaces[idx].configured
        {
            self.surfaces[idx].needs_redraw = true;
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(idx) = self.surfaces.iter().position(|s| s.output == output) {
            let removed = self.surfaces.remove(idx);
            self.layout.outputs.retain(|r| r.name != removed.name);
            if self.layout.focused.as_deref() == Some(removed.name.as_str()) {
                self.layout.focused = None;
            }
            if self.surfaces.is_empty() {
                self.exit = true;
            }
        }
    }
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(idx) = self.surfaces.iter().position(|s| &s.layer == layer) {
            self.surfaces.remove(idx);
        }
        if self.surfaces.is_empty() {
            self.exit = true;
        }
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(idx) = self.surfaces.iter().position(|s| &s.layer == layer) else {
            return;
        };
        let (mut width, mut height) = configure.new_size;
        if width == 0 || height == 0 {
            // Fall back to the layout size for this output (its logical
            // size from Hyprland IPC / xdg-output).
            let rect = self.surface_rect(&self.surfaces[idx]);
            if width == 0 {
                width = rect.width;
            }
            if height == 0 {
                height = rect.height;
            }
        }
        if width == 0 || height == 0 {
            self.fatal = Some(format!(
                "compositor sent an unusable layer-shell size {width}x{height} for output {}",
                self.surfaces[idx].name
            ));
            return;
        }

        // An output the layout did not know (no IPC entry, no xdg-output
        // geometry) joins the layout at the origin with its configured
        // size, so single-output fallback behaviour is preserved.
        if !self
            .layout
            .outputs
            .iter()
            .any(|r| r.name == self.surfaces[idx].name)
        {
            self.layout.outputs.push(OutputRect {
                name: self.surfaces[idx].name.clone(),
                x: 0,
                y: 0,
                width,
                height,
            });
            if self.state.pos.is_none() {
                self.state.pos = Some(self.layout.center());
            }
        }

        self.surfaces[idx].size = Some((width, height));
        self.surfaces[idx].configured = true;
        if let Err(e) = self.draw_surface(idx) {
            self.fatal = Some(e.to_string());
        }
    }
}

impl ShmHandler for Overlay {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()> for Overlay {
    fn event(
        _state: &mut Self,
        _proxy: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        _event: wp_fractional_scale_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for Overlay {
    fn event(
        state: &mut Self,
        proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if let Some(idx) = state
                .surfaces
                .iter()
                .position(|s| s.fractional.as_ref().map(|f| f.id()) == Some(proxy.id()))
            {
                state.surfaces[idx].fractional_scale_120 = Some(scale);
                if state.surfaces[idx].configured {
                    state.surfaces[idx].needs_redraw = true;
                }
            }
        }
    }
}

impl Dispatch<wp_viewporter::WpViewporter, ()> for Overlay {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewporter::WpViewporter,
        _event: wp_viewporter::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wp_viewport::WpViewport, ()> for Overlay {
    fn event(
        _state: &mut Self,
        _proxy: &wp_viewport::WpViewport,
        _event: wp_viewport::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_compositor!(Overlay);
delegate_output!(Overlay);
delegate_layer!(Overlay);
delegate_shm!(Overlay);
delegate_registry!(Overlay);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Rgb;

    #[test]
    fn fractional_buffer_size_rounds_to_physical_pixels() {
        // 1920x1200 at scale 1.25 is a 1536x960 logical surface.
        assert_eq!(scale_dimension(1536, 150), 1920);
        assert_eq!(scale_dimension(960, 150), 1200);
        assert_eq!(scale_dimension(100, 120), 100);
        assert_eq!(scale_dimension(1, 1), 1);
    }

    #[test]
    fn paints_opaque_center_and_transparent_corners() {
        let width = 32u32;
        let height = 32u32;
        let mut canvas = vec![0u8; (width * height * 4) as usize];
        paint_dot(&mut canvas, width, height, 16.0, 16.0, 8.0, Rgb::RED);

        let center = ((16 * width + 16) * 4) as usize;
        let pixel = u32::from_ne_bytes(canvas[center..center + 4].try_into().unwrap());
        assert_eq!(pixel >> 24, 0xff, "centre alpha should be opaque");
        assert_eq!((pixel >> 16) & 0xff, 0xff, "centre should be red");
        assert_eq!(pixel & 0xff, 0, "blue channel should be zero");

        let corner = 0usize;
        assert_eq!(
            u32::from_ne_bytes(canvas[corner..corner + 4].try_into().unwrap()),
            0,
            "corner must stay fully transparent"
        );
    }

    #[test]
    fn paints_requested_color_premultiplied() {
        let width = 16u32;
        let height = 16u32;
        let mut canvas = vec![0u8; (width * height * 4) as usize];
        let blue = Rgb {
            r: 0x10,
            g: 0x20,
            b: 0xff,
        };
        paint_dot(&mut canvas, width, height, 8.0, 8.0, 5.0, blue);

        let center = ((8 * width + 8) * 4) as usize;
        let pixel = u32::from_ne_bytes(canvas[center..center + 4].try_into().unwrap());
        assert_eq!(pixel >> 24, 0xff, "centre alpha should be opaque");
        assert_eq!((pixel >> 16) & 0xff, 0x10);
        assert_eq!((pixel >> 8) & 0xff, 0x20);
        assert_eq!(pixel & 0xff, 0xff);
    }
}
