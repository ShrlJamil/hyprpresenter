//! Minimal transparent, click-through laser overlay.
//!
//! The overlay is a single `wlr-layer-shell` surface on the `Overlay` layer,
//! sized to the whole target output.  It is intentionally simple:
//!
//! * `KeyboardInteractivity::None` (never take keyboard focus)
//! * an explicitly empty input region (pointer events pass through)
//! * one `wl_shm` ARGB8888 buffer, redrawn on demand
//! * `wp_fractional_scale_v1` + `wp_viewport` when available, so a 1.25x
//!   output is rendered at full physical resolution instead of being
//!   stretched from logical pixels
//!
//! Coordinates accepted by the test controls are **output-local logical
//! pixels**.  The selected output's logical size is whatever the compositor
//! reports in the layer-shell configure event (1536x960 on a 1920x1200 @ 1.25
//! output), not the physical mode size.
//!
//! Test control is deliberately a tiny, removable local UNIX socket.  The
//! future network layer only has to feed [`Command`] values into the same
//! handler.

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
    Connection, Dispatch, QueueHandle,
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
    viewporter::client::{wp_viewport, wp_viewporter},
};

use crate::error::{Error, Result};

/// Default output to draw on.  Override with `--output NAME`.
const DEFAULT_OUTPUT: &str = "eDP-1";
/// Default dot radius in logical pixels.
const DEFAULT_RADIUS: i32 = 12;
/// Environment override, mainly for automated tests and side-by-side runs.
const SOCKET_ENV: &str = "HYPRPRESENTER_SOCKET";

pub fn run(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        None => run_overlay(Options::default()),
        Some("run") => run_overlay(Options::parse(&args[1..])?),
        Some("move") => {
            if args.len() != 3 {
                return Err(Error::Overlay(
                    "usage: hyprpresenter overlay move X Y".into(),
                ));
            }
            let x = parse_i32("X", &args[1])?;
            let y = parse_i32("Y", &args[2])?;
            send_control(&format!("move {x} {y}"))
        }
        Some("center") => send_control("center"),
        Some("show") => send_control("show"),
        Some("hide") => send_control("hide"),
        Some("status") => send_control("status"),
        Some("quit") => send_control("quit"),
        Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some(first) if first.starts_with('-') => run_overlay(Options::parse(args)?),
        Some(other) => Err(Error::Overlay(format!(
            "unknown overlay action `{other}` (try `hyprpresenter overlay --help`)"
        ))),
    }
}

fn print_help() {
    println!(
        "hyprpresenter {} — laser overlay (proof of concept)

USAGE:
  hyprpresenter overlay [OPTIONS]     run the overlay (default: center of eDP-1)
  hyprpresenter overlay status        report output, surface, buffer and dot state
  hyprpresenter overlay move X Y      move the dot (output-local logical pixels)
  hyprpresenter overlay center        move the dot back to the logical center
  hyprpresenter overlay hide          render an empty (transparent) surface
  hyprpresenter overlay show          draw the dot again
  hyprpresenter overlay quit          shut the overlay down cleanly

OPTIONS:
  --output NAME    select the wl_output (default: {DEFAULT_OUTPUT})
  --x N --y N      initial dot position in output-local logical pixels
  --radius N       dot radius in logical pixels (default: {DEFAULT_RADIUS})
  -h, --help       show this help

The controls use a local UNIX socket in $XDG_RUNTIME_DIR (override with
${SOCKET_ENV}) and are intentionally temporary; they will be replaced by the
network command handler in a later phase.",
        env!("CARGO_PKG_VERSION")
    );
}

#[derive(Debug, Clone)]
struct Options {
    output: String,
    x: Option<i32>,
    y: Option<i32>,
    radius: i32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            output: DEFAULT_OUTPUT.to_string(),
            x: None,
            y: None,
            radius: DEFAULT_RADIUS,
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
                    options.output = args
                        .get(i)
                        .ok_or_else(|| Error::Overlay("--output requires a name".into()))?
                        .clone();
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
                    options.radius = parse_i32(
                        "--radius",
                        args.get(i)
                            .ok_or_else(|| Error::Overlay("--radius requires a value".into()))?,
                    )?;
                }
                other => {
                    return Err(Error::Overlay(format!(
                        "unknown overlay option `{other}` (try `hyprpresenter overlay --help`)"
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
        if !(1..=500).contains(&options.radius) {
            return Err(Error::Overlay(
                "--radius must be between 1 and 500 logical pixels".into(),
            ));
        }
        Ok(options)
    }
}

fn parse_i32(what: &str, raw: &str) -> Result<i32> {
    raw.parse()
        .map_err(|_| Error::Overlay(format!("{what} must be an integer, got `{raw}`")))
}

// ---------------------------------------------------------------------------
// Local test control
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Move(i32, i32),
    Center,
    Show,
    Hide,
    Status,
    Quit,
}

fn parse_command(raw: &str) -> std::result::Result<Command, String> {
    let mut parts = raw.split_whitespace();
    let command = match parts.next() {
        Some("move") => {
            let x = parts
                .next()
                .ok_or_else(|| "move requires X Y".to_string())?
                .parse::<i32>()
                .map_err(|_| "move X must be an integer".to_string())?;
            let y = parts
                .next()
                .ok_or_else(|| "move requires X Y".to_string())?
                .parse::<i32>()
                .map_err(|_| "move Y must be an integer".to_string())?;
            Command::Move(x, y)
        }
        Some("center") => Command::Center,
        Some("show") => Command::Show,
        Some("hide") => Command::Hide,
        Some("status") => Command::Status,
        Some("quit") | Some("exit") => Command::Quit,
        Some(other) => return Err(format!("unknown command `{other}`")),
        None => return Err("empty command".to_string()),
    };
    if parts.next().is_some() {
        return Err("too many arguments".to_string());
    }
    Ok(command)
}

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

struct ControlRequest {
    command: Command,
    reply: mpsc::Sender<String>,
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
                reply: reply_tx,
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
// Wayland overlay
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleMode {
    /// Scale is 120ths, rendered through a viewport.
    Fractional(u32),
    /// Integer buffer scale, no viewport.
    Integer(u32),
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

    output_name: String,
    target_output: Option<wl_output::WlOutput>,
    output_integer_scale: i32,
    output_logical_size: Option<(u32, u32)>,

    layer: Option<LayerSurface>,
    fractional: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    viewport: Option<wp_viewport::WpViewport>,
    pool: Option<SlotPool>,
    /// The single `wl_shm` buffer, reused for every frame.  Reusing one slot
    /// keeps memory bounded; a redraw is deferred until the compositor sends
    /// `wl_buffer.release` for the previous commit.
    buffer: Option<Buffer>,

    size: Option<(u32, u32)>,
    buffer_size: Option<(u32, u32)>,
    fractional_scale_120: Option<u32>,
    needs_redraw: bool,

    pos: Option<(i32, i32)>,
    center_mode: bool,
    visible: bool,
    radius: i32,

    configured: bool,
    exit: bool,
    fatal: Option<String>,
}

fn run_overlay(options: Options) -> Result<()> {
    let socket_path = control_socket_path()?;
    let listener = bind_control_socket(&socket_path)?;
    let (request_tx, request_rx) = channel::<ControlRequest>();
    let control_server = ControlServer::start(socket_path, listener, request_tx)?;

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

    let mut app = Overlay {
        conn: conn.clone(),
        registry_state,
        output_state,
        compositor,
        layer_shell,
        shm,
        fractional_manager,
        viewporter,
        output_name: options.output.clone(),
        target_output: None,
        output_integer_scale: 1,
        output_logical_size: None,
        layer: None,
        fractional: None,
        viewport: None,
        pool: None,
        buffer: None,
        size: None,
        buffer_size: None,
        fractional_scale_120: None,
        needs_redraw: false,
        pos: options.x.zip(options.y),
        center_mode: options.x.is_none(),
        visible: true,
        radius: options.radius,
        configured: false,
        exit: false,
        fatal: None,
    };

    // First roundtrip populates wl_output / xdg-output information so the
    // requested output can be selected by its compositor name.
    event_queue
        .roundtrip(&mut app)
        .map_err(|e| Error::Wayland(format!("initial output roundtrip: {e}")))?;
    app.select_output(&options.output)?;
    app.create_surface(&qh)?;

    // Initial commit (no buffer) asks layer-shell for a configure.  A second
    // roundtrip lets the configure handler allocate and attach the first
    // buffer before entering the long-running event loop.
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
                let _ = request.reply.send(reply);
            }
        })
        .map_err(|e| Error::Overlay(format!("failed to insert control channel: {e}")))?;

    while !app.exit && app.fatal.is_none() {
        event_loop
            .dispatch(None, &mut app)
            .map_err(|e| Error::Overlay(format!("event loop error: {e}")))?;
        // Commands only record state; draw once per batch so a burst of
        // moves cannot allocate a buffer per message.  A draw that finds the
        // previous buffer still in use leaves the flag set and is retried
        // after the next dispatch (which delivers `wl_buffer.release`).
        if app.needs_redraw && !app.exit && app.fatal.is_none() {
            app.needs_redraw = false;
            if let Err(e) = app.draw() {
                app.fatal = Some(e.to_string());
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
    fn select_output(&mut self, requested: &str) -> Result<()> {
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
            available.push(label.clone());
            if info.name.as_deref() == Some(requested) {
                self.target_output = Some(output);
                self.output_name = requested.to_string();
                self.output_integer_scale = info.scale_factor.max(1);
                self.output_logical_size = info
                    .logical_size
                    .and_then(|(w, h)| (w > 0 && h > 0).then_some((w as u32, h as u32)));
                return Ok(());
            }
        }
        Err(Error::Overlay(format!(
            "output `{requested}` not found (available: {})",
            if available.is_empty() {
                "none reported".to_string()
            } else {
                available.join(", ")
            }
        )))
    }

    fn create_surface(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let output = self
            .target_output
            .clone()
            .ok_or_else(|| Error::Overlay("no target output selected".into()))?;
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("hyprpresenter-laser"),
            Some(&output),
        );

        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // With opposite edges anchored, a zero size means "full output"; the
        // actual logical size arrives in the configure event.
        layer.set_size(0, 0);
        // -1 makes the layer ignore other surfaces' exclusive zones (e.g. a
        // 36px top bar) and extend to the output edges.  The surface is
        // transparent, so it does not visually cover the panel; it just
        // allows the dot to reach every pixel of the monitor.
        layer.set_exclusive_zone(-1);
        layer.set_margin(0, 0, 0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);

        // Explicitly empty input region: no pointer events are delivered to
        // this surface, so clicks go to whatever is underneath it.
        let region = Region::new(&self.compositor)
            .map_err(|e| Error::Wayland(format!("failed to create empty input region: {e}")))?;
        layer.set_input_region(Some(region.wl_region()));
        drop(region);

        if let Some(manager) = &self.fractional_manager {
            self.fractional = Some(manager.get_fractional_scale(layer.wl_surface(), qh, ()));
        }
        if let Some(viewporter) = &self.viewporter {
            self.viewport = Some(viewporter.get_viewport(layer.wl_surface(), qh, ()));
        }

        layer.commit();
        self.conn
            .flush()
            .map_err(|e| Error::Wayland(format!("flush initial surface commit: {e}")))?;
        self.layer = Some(layer);
        Ok(())
    }

    fn scale_mode(&self) -> ScaleMode {
        match (self.fractional_scale_120, self.viewport.as_ref()) {
            (Some(scale_120), Some(_)) if scale_120 > 0 => ScaleMode::Fractional(scale_120),
            _ => ScaleMode::Integer(self.output_integer_scale.max(1) as u32),
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

    fn draw(&mut self) -> Result<()> {
        if !self.configured {
            return Ok(());
        }
        let layer = match self.layer.as_ref() {
            Some(layer) => layer,
            None => return Ok(()),
        };
        let (width, height) = self
            .size
            .ok_or_else(|| Error::Overlay("surface has no configured size".into()))?;
        if width == 0 || height == 0 {
            return Ok(());
        }

        let (buffer_width, buffer_height, mode) = self.buffer_geometry(width, height);
        let stride = (buffer_width as i32)
            .checked_mul(4)
            .ok_or_else(|| Error::Overlay("buffer stride overflow".into()))?;
        let len = (stride as usize)
            .checked_mul(buffer_height as usize)
            .ok_or_else(|| Error::Overlay("buffer size overflow".into()))?;

        if self.pool.is_none() {
            self.pool = Some(
                SlotPool::new(len, &self.shm)
                    .map_err(|e| Error::Overlay(format!("failed to create shm pool: {e}")))?,
            );
        }

        if self.buffer.is_none() || self.buffer_size != Some((buffer_width, buffer_height)) {
            // Drop a stale buffer first so its slot returns to the pool before
            // the new allocation (if the compositor already released it).
            self.buffer = None;
            self.buffer_size = None;
            let (buffer, _) = self
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
            self.buffer = Some(buffer);
        }

        let buffer = self.buffer.take().expect("buffer was just created");
        if buffer.slot().has_active_buffers() {
            // The compositor has not released the previous commit yet.  Keep
            // the buffer and retry after `wl_buffer.release`; allocating a new
            // slot here is what made rapid moves grow the pool without bound.
            self.buffer = Some(buffer);
            self.needs_redraw = true;
            return Ok(());
        }

        let canvas = self
            .pool
            .as_mut()
            .expect("pool was just created")
            .canvas(&buffer)
            .ok_or_else(|| Error::Overlay("shm buffer is unexpectedly busy".into()))?;

        canvas.fill(0);
        if self.visible {
            if let Some((x, y)) = self.pos {
                let scale_x = buffer_width as f64 / width as f64;
                let scale_y = buffer_height as f64 / height as f64;
                // Pixel centres: logical pixel (x, y) covers [x,x+1)x[y,y+1).
                let center_x = (x as f64 + 0.5) * scale_x;
                let center_y = (y as f64 + 0.5) * scale_y;
                let radius = self.radius as f64 * (scale_x + scale_y) / 2.0;
                paint_dot(
                    canvas,
                    buffer_width,
                    buffer_height,
                    center_x,
                    center_y,
                    radius,
                );
            }
        }

        match mode {
            ScaleMode::Fractional(_) => {
                if let Some(viewport) = &self.viewport {
                    viewport.set_destination(width as i32, height as i32);
                }
                layer
                    .set_buffer_scale(1)
                    .map_err(|_| Error::Overlay("wl_surface v3 is required".into()))?;
            }
            ScaleMode::Integer(scale) => {
                if let Some(viewport) = &self.viewport {
                    viewport.set_destination(-1, -1);
                }
                layer
                    .set_buffer_scale(scale)
                    .map_err(|_| Error::Overlay("wl_surface v3 is required".into()))?;
            }
        }

        buffer
            .attach_to(layer.wl_surface())
            .map_err(|e| Error::Overlay(format!("failed to attach shm buffer: {e}")))?;
        layer.wl_surface().damage(0, 0, width as i32, height as i32);
        layer.commit();
        self.conn
            .flush()
            .map_err(|e| Error::Wayland(format!("flush surface commit: {e}")))?;

        self.buffer = Some(buffer);
        self.buffer_size = Some((buffer_width, buffer_height));
        Ok(())
    }

    fn apply_command(&mut self, command: Command) -> String {
        match command {
            Command::Move(x, y) => {
                let (x, y) = self.clamp_position(x, y);
                self.pos = Some((x, y));
                self.center_mode = false;
                self.needs_redraw = true;
                format!("ok dot={x},{y}")
            }
            Command::Center => {
                let Some((width, height)) = self.size else {
                    return "error: surface is not configured yet".to_string();
                };
                self.center_mode = true;
                self.pos = Some((width as i32 / 2, height as i32 / 2));
                self.needs_redraw = true;
                format!("ok dot={},{}", width / 2, height / 2)
            }
            Command::Show => {
                self.visible = true;
                self.needs_redraw = true;
                "ok visible=1".to_string()
            }
            Command::Hide => {
                self.visible = false;
                self.needs_redraw = true;
                "ok visible=0".to_string()
            }
            Command::Status => self.status_line(),
            Command::Quit => {
                self.exit = true;
                "ok shutting down".to_string()
            }
        }
    }

    fn clamp_position(&self, x: i32, y: i32) -> (i32, i32) {
        clamp_position(self.size, x, y)
    }

    fn status_line(&self) -> String {
        let (logical_w, logical_h) = self.size.unwrap_or((0, 0));
        let (buffer_w, buffer_h) = self.buffer_size.unwrap_or((0, 0));
        let (x, y) = self.pos.unwrap_or((0, 0));
        let scale = match self.scale_mode() {
            ScaleMode::Fractional(scale_120) => scale_120 as f64 / 120.0,
            ScaleMode::Integer(scale) => scale as f64,
        };
        format!(
            "output={} visible={} logical={}x{} buffer={}x{} dot={},{} radius={} scale={:.3} configured={}",
            self.output_name,
            u8::from(self.visible),
            logical_w,
            logical_h,
            buffer_w,
            buffer_h,
            x,
            y,
            self.radius,
            scale,
            u8::from(self.configured),
        )
    }
}

fn clamp_position(size: Option<(u32, u32)>, x: i32, y: i32) -> (i32, i32) {
    match size {
        Some((width, height)) => (
            x.clamp(0, width.saturating_sub(1) as i32),
            y.clamp(0, height.saturating_sub(1) as i32),
        ),
        None => (x.max(0), y.max(0)),
    }
}

fn scale_dimension(logical: u32, scale_120: u32) -> u32 {
    // Round half away from zero, matching the fractional-scale protocol's
    // toplevel rounding recommendation.
    ((logical as u64 * scale_120 as u64 + 60) / 120).max(1) as u32
}

/// Paint an anti-aliased red dot into a premultiplied ARGB8888 buffer.
fn paint_dot(
    canvas: &mut [u8],
    buffer_width: u32,
    buffer_height: u32,
    center_x: f64,
    center_y: f64,
    radius: f64,
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
            // Premultiplied red: (A << 24) | (R << 16) with G = B = 0.
            let pixel = (alpha << 24) | (alpha << 16);
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
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.output_integer_scale = new_factor.max(1);
        // A fractional scale is more precise; only re-render when the integer
        // fallback is actually in use.
        if matches!(self.scale_mode(), ScaleMode::Integer(_)) && self.configured {
            self.needs_redraw = true;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
        // The proof-of-concept target output is unrotated.  Rotated outputs
        // would need the buffer transform applied while drawing.
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
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.target_output.as_ref() != Some(&output) {
            return;
        }
        if let Some(info) = self.output_state.info(&output) {
            self.output_integer_scale = info.scale_factor.max(1);
            self.output_logical_size = info
                .logical_size
                .and_then(|(w, h)| (w > 0 && h > 0).then_some((w as u32, h as u32)));
        }
        if matches!(self.scale_mode(), ScaleMode::Integer(_)) && self.configured {
            self.needs_redraw = true;
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.target_output.as_ref() == Some(&output) {
            self.fatal = Some("target output was removed".to_string());
        }
    }
}

impl LayerShellHandler for Overlay {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if self.layer.as_ref() != Some(layer) {
            return;
        }
        let (mut width, mut height) = configure.new_size;
        if width == 0 || height == 0 {
            if let Some((fallback_w, fallback_h)) = self.output_logical_size {
                if width == 0 {
                    width = fallback_w;
                }
                if height == 0 {
                    height = fallback_h;
                }
            }
        }
        if width == 0 || height == 0 {
            self.fatal = Some(format!(
                "compositor sent an unusable layer-shell size {width}x{height}"
            ));
            return;
        }

        self.size = Some((width, height));
        self.configured = true;
        if self.center_mode || self.pos.is_none() {
            self.pos = Some((width as i32 / 2, height as i32 / 2));
        } else if let Some((x, y)) = self.pos {
            self.pos = Some(self.clamp_position(x, y));
        }

        if let Err(e) = self.draw() {
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
        _proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.fractional_scale_120 = Some(scale);
            if state.configured {
                state.needs_redraw = true;
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

    #[test]
    fn parses_test_commands() {
        assert_eq!(parse_command("move 12 34"), Ok(Command::Move(12, 34)));
        assert_eq!(parse_command("move -5 0"), Ok(Command::Move(-5, 0)));
        assert_eq!(parse_command("center"), Ok(Command::Center));
        assert_eq!(parse_command("show"), Ok(Command::Show));
        assert_eq!(parse_command("hide"), Ok(Command::Hide));
        assert_eq!(parse_command("status"), Ok(Command::Status));
        assert_eq!(parse_command("quit"), Ok(Command::Quit));
        assert_eq!(parse_command("exit"), Ok(Command::Quit));

        assert!(parse_command("").is_err());
        assert!(parse_command("move 1").is_err());
        assert!(parse_command("move 1 2 3").is_err());
        assert!(parse_command("move a b").is_err());
        assert!(parse_command("nope").is_err());
    }

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
        paint_dot(&mut canvas, width, height, 16.0, 16.0, 8.0);

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
    fn clamps_positions_to_surface() {
        let size = Some((1536, 960));
        assert_eq!(clamp_position(size, -10, 2000), (0, 959));
        assert_eq!(clamp_position(size, 100, 200), (100, 200));
        assert_eq!(clamp_position(size, 1535, 959), (1535, 959));
        assert_eq!(clamp_position(None, -10, -20), (0, 0));
    }
}
