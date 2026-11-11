//! LAN WebSocket networking (Phase 5).
//!
//! A small synchronous WebSocket server that lets one paired phone drive
//! the laser over the local network.  It is a thin adapter over the command
//! core, exactly like the local UNIX-socket handler:
//!
//! * a dedicated thread accepts **one** client at a time (a presentation
//!   clicker has exactly one driver; further connections wait in the TCP
//!   backlog until the current session ends);
//! * the client must authenticate with the pairing token (see below)
//!   before any command is accepted; commands sent earlier get an error
//!   reply but the connection stays open so the client can still
//!   authenticate (real clients reconnect and race their first command);
//!   after 5 failed authentication attempts the connection is closed;
//! * every later text message is one JSON request, decoded into a
//!   [`Command`] and forwarded to the overlay's event loop through the
//!   same calloop channel the socket handler uses.  The core's reply text
//!   comes back and is wrapped in a JSON envelope.
//!
//! No async runtime: `tungstenite` is a synchronous WebSocket library, and
//! the thread/channel shape mirrors the existing control-socket server.
//!
//! ## Pairing
//!
//! `hypresent pair` generates a random token, stores only its SHA-256
//! hash (mode 0600, under `$XDG_CONFIG_HOME/hyprpresenter/`), and prints
//! the token once.  Presenting the stored hash itself does not work: the
//! server hashes whatever the client presents and compares hashes.  The
//! hash file is re-read for every connection, so pairing (or regenerating,
//! which revokes the old token) takes effect without restarting the
//! overlay.  There is deliberately no TLS in this phase: the trust model
//! is "paired token + local network", as recorded in the README.

use std::{
    fs,
    io::Read,
    net::{TcpListener, TcpStream, UdpSocket},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tungstenite::Message;

use smithay_client_toolkit::reexports::calloop;

use crate::{
    command::{check_radius, parse_color, Command},
    error::{Error, Result},
    overlay::ControlRequest,
};

/// Default WebSocket port.  Override with `overlay --ws-port`.
pub const DEFAULT_WS_PORT: u16 = 8423;

/// Environment override for the pairing hash file, mainly for tests.
const PAIRING_ENV: &str = "HYPRPRESENTER_PAIRING_FILE";

/// How long a fresh connection has to complete authentication.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);
/// Failed authentication attempts tolerated per connection before it is
/// closed.  (Online guessing is hopeless against a 192-bit token either
/// way; the cap just keeps a confused client from holding the single
/// session slot forever.)
const MAX_AUTH_ATTEMPTS: u32 = 5;
/// How long the server thread waits for the event loop to answer one
/// forwarded command before telling the client it gave up.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A session with no traffic at all — not even a protocol ping — for this
/// long is considered dead and is evicted.  A phone that vanishes from the
/// network (app killed, Wi-Fi dropped) otherwise leaves a half-open TCP
/// connection that would hold the single session slot forever: observed
/// live when a tester app's abandoned session blocked the real app, whose
/// handshakes then sat in the TCP backlog until they timed out.  Healthy
/// clients ping every few seconds, so this only fires on silence.
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// Pairing token
// ---------------------------------------------------------------------------

/// Path of the file holding the hex SHA-256 of the pairing token.
pub fn pairing_file_path() -> PathBuf {
    if let Ok(path) = std::env::var(PAIRING_ENV) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("hyprpresenter").join("pairing.sha256")
}

/// Normalise a token for hashing: lowercase, with spaces and dashes (the
/// grouping used when the token is displayed) removed.
pub fn normalize_token(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

/// SHA-256 of the normalised token, hex-encoded.
pub fn hash_token(raw: &str) -> String {
    let digest = Sha256::digest(normalize_token(raw).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Load the stored token hash, if a token has been provisioned.
pub fn load_token_hash() -> Option<String> {
    fs::read_to_string(pairing_file_path())
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Does `presented` match the provisioned token?
fn token_matches(presented: &str) -> bool {
    match load_token_hash() {
        Some(stored) => {
            let candidate = hash_token(presented);
            // The candidate is a hash of what was presented; the stored
            // value alone cannot be replayed as a token.
            candidate.len() == stored.len()
                && candidate
                    .bytes()
                    .zip(stored.bytes())
                    .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                    == 0
        }
        None => false,
    }
}

/// Generate a fresh 192-bit random token as ungrouped hex.
fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 24];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| Error::Overlay(format!("cannot read /dev/urandom: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Best-effort primary LAN address of this machine (UDP connect trick: no
/// traffic is sent, the socket just reveals which local address the kernel
/// would use).  Used only to print a helpful pairing URL.
fn lan_ip_hint() -> Option<String> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

/// The pairing URI carried by the QR code: everything the phone app needs
/// to fill its pairing form in one scan.
pub fn pairing_uri(host: &str, port: u16, token: &str) -> String {
    format!("hyprpresenter://pair?host={host}&port={port}&token={token}")
}

/// Render a payload as a terminal QR code (Unicode half-blocks, with a
/// quiet zone) for the phone camera to scan off the screen.
pub fn pairing_qr_terminal(payload: &str) -> std::result::Result<String, String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(payload.as_bytes())
        .map_err(|e| format!("cannot build QR code: {e}"))?;
    Ok(code
        .render::<Dense1x2>()
        .dark_color(Dense1x2::Light)
        .light_color(Dense1x2::Dark)
        .quiet_zone(true)
        .build())
}

/// `hypresent pair` — provision the pairing token.
///
/// Pairing is deliberately single-step: there is exactly one valid token
/// at a time, and asking to pair again MEANS rotating it — the new hash
/// replaces the old one, so the previous token stops working the moment
/// this runs.  The old `--regenerate` flag spelling is still accepted as
/// an alias so habits and scripts keep working.
pub fn cmd_pair(args: &[String]) -> Result<()> {
    match args {
        [] => {}
        [flag] if flag == "--regenerate" => {}
        _ => {
            return Err(Error::Overlay("usage: hypresent pair".into()));
        }
    }

    let path = pairing_file_path();
    let replacing = path.exists();
    let token = generate_token()?;
    if replacing {
        println!("Replacing the previous pairing — its token stops working now.");
        println!();
    }
    if let Some(parent) = path.parent() {
        // Only chmod a directory we just created: tightening permissions
        // is for the fresh config dir, and chmod-ing a pre-existing dir
        // we do not own (e.g. /tmp via the env override) fails with EPERM.
        if !parent.exists() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    fs::write(&path, format!("{}\n", hash_token(&token)))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;

    let grouped = token
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("hex is ASCII"))
        .collect::<Vec<_>>()
        .join(" ");
    let host = lan_ip_hint();
    println!("Pairing token (shown once — scan the QR with the phone app):");
    println!();
    println!("  {grouped}");
    println!();
    match &host {
        Some(host) => {
            println!("Server URL: ws://{host}:{DEFAULT_WS_PORT}");
            match pairing_qr_terminal(&pairing_uri(host, DEFAULT_WS_PORT, &token)) {
                Ok(qr) => {
                    println!();
                    println!("Scan to pair (the QR contains host, port, and token):");
                    println!("{qr}");
                }
                Err(e) => eprintln!("hypresent: {e}"),
            }
        }
        None => println!("Server URL: ws://<this-machine-LAN-IP>:{DEFAULT_WS_PORT}"),
    }
    println!(
        "Only the token's SHA-256 hash is stored, in {} (mode 0600).",
        path.display()
    );
    println!("Regenerating the token revokes this one immediately.");
    Ok(())
}

// ---------------------------------------------------------------------------
// JSON wire protocol (v1)
// ---------------------------------------------------------------------------

/// One decoded client message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsRequest {
    /// `{"v":1,"type":"auth","token":"…"}` — must be the first message.
    Auth(String),
    /// Any command message; decoded straight into the core's `Command`.
    Cmd(Command),
}

#[derive(Deserialize)]
struct WireRequest {
    v: Option<u32>,
    #[serde(rename = "type")]
    kind: Option<String>,
    token: Option<String>,
    cmd: Option<String>,
    output: Option<serde_json::Value>,
    x: Option<i32>,
    y: Option<i32>,
    value: Option<i32>,
    color: Option<String>,
}

/// Decode one JSON text message into a [`WsRequest`], reusing the command
/// core's validators so the WS and socket transports can never disagree
/// about what a legal radius or colour is.
pub fn parse_request(text: &str) -> std::result::Result<WsRequest, String> {
    let wire: WireRequest = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    if let Some(v) = wire.v {
        if v != 1 {
            return Err(format!("unsupported protocol version {v} (expected 1)"));
        }
    }
    if wire.kind.as_deref() == Some("auth") {
        let token = wire
            .token
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| "auth requires a token".to_string())?;
        return Ok(WsRequest::Auth(token));
    }
    let cmd = wire.cmd.ok_or_else(|| "missing `cmd`".to_string())?;
    let command = match cmd.as_str() {
        "move" => {
            let x = wire.x.ok_or_else(|| "move requires `x`".to_string())?;
            let y = wire.y.ok_or_else(|| "move requires `y`".to_string())?;
            Command::Move(x, y)
        }
        "center" => Command::Center,
        "show" => Command::Show,
        "hide" => Command::Hide,
        "radius" => {
            let value = wire
                .value
                .ok_or_else(|| "radius requires `value`".to_string())?;
            check_radius(value)?;
            Command::Radius(value)
        }
        "color" => {
            let raw = wire
                .color
                .ok_or_else(|| "color requires `color`".to_string())?;
            Command::Color(parse_color(&raw)?)
        }
        "confine" => {
            // `output` absent or null both release the confinement (serde
            // maps both to None for an Option field); a string names the
            // output to confine to.
            match wire.output {
                None => Command::Confine(None),
                Some(serde_json::Value::String(name)) if name == "off" || name == "none" => {
                    Command::Confine(None)
                }
                Some(serde_json::Value::String(name)) => Command::Confine(Some(name)),
                Some(_) => {
                    return Err("confine `output` must be a string or null".to_string());
                }
            }
        }
        "previous" | "prev" => Command::Previous,
        "next" => Command::Next,
        "status" => Command::Status,
        "quit" => Command::Quit,
        other => return Err(format!("unknown cmd `{other}`")),
    };
    Ok(WsRequest::Cmd(command))
}

/// Wrap a successful core reply in the v1 envelope.
pub fn ok_reply(reply: &str) -> String {
    serde_json::json!({"v": 1, "ok": true, "reply": reply}).to_string()
}

/// Send one text frame and flush it immediately.  `tungstenite`'s `write`
/// only queues the frame in an internal buffer; without an explicit flush
/// a reply can sit unsent while the server is parked in `read` (observed
/// live: commands were executed but no reply ever reached the client).
fn send_frame(ws: &mut tungstenite::WebSocket<TcpStream>, message: Message) {
    let _ = ws.write(message);
    let _ = ws.flush();
}

fn send_text(ws: &mut tungstenite::WebSocket<TcpStream>, text: String) {
    send_frame(ws, Message::text(text));
}

/// Wrap an error in the v1 envelope.
pub fn error_reply(message: &str) -> String {
    serde_json::json!({"v": 1, "ok": false, "error": message}).to_string()
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

/// Owns the WebSocket listener thread; dropping it stops the thread, wakes
/// a blocking `accept()`, and drops any active session.
pub struct NetServer {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    wake_addr: std::net::SocketAddr,
}

impl NetServer {
    pub fn start(
        port: u16,
        requests: calloop::channel::Sender<ControlRequest>,
    ) -> std::result::Result<Self, String> {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .map_err(|e| format!("cannot bind ws://0.0.0.0:{port}: {e}"))?;
        let wake_addr = listener
            .local_addr()
            .map_err(|e| format!("cannot read listener address: {e}"))?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let handle = thread::Builder::new()
            .name("hyprpresenter-ws".into())
            .spawn(move || serve_loop(listener, requests, thread_shutdown))
            .map_err(|e| format!("failed to start websocket thread: {e}"))?;
        Ok(Self {
            shutdown,
            handle: Some(handle),
            wake_addr,
        })
    }
}

impl Drop for NetServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake a blocking accept(); an active session notices the flag on
        // its next read-timeout poll (<= 500 ms).
        let _ = TcpStream::connect(self.wake_addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn serve_loop(
    listener: TcpListener,
    requests: calloop::channel::Sender<ControlRequest>,
    shutdown: Arc<AtomicBool>,
) {
    for connection in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = connection else {
            continue;
        };
        // One driver at a time: this call blocks until the session ends.
        handle_client(stream, &requests, &shutdown);
    }
}

/// Run one client session: authenticate, then forward commands until the
/// client disconnects, sends `quit`, or the server shuts down.
fn handle_client(
    stream: TcpStream,
    requests: &calloop::channel::Sender<ControlRequest>,
    shutdown: &Arc<AtomicBool>,
) {
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_nodelay(true);
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("hypresent: ws handshake from {peer} failed: {e}");
            return;
        }
    };
    eprintln!("hypresent: ws client {peer} connected");

    // -- Authentication phase ------------------------------------------------
    let mut failed_auths = 0u32;
    let auth_deadline = Instant::now() + AUTH_TIMEOUT;
    let authenticated = loop {
        if shutdown.load(Ordering::SeqCst) || Instant::now() > auth_deadline {
            return;
        }
        match ws.read() {
            Ok(Message::Text(text)) => match parse_request(&text) {
                Ok(WsRequest::Auth(token)) => {
                    if load_token_hash().is_none() {
                        eprintln!("hypresent: ws auth from {peer} refused: no token provisioned");
                        send_text(
                            &mut ws,
                            error_reply(
                                "no pairing token provisioned; run `hypresent pair` on the desktop",
                            ),
                        );
                        return;
                    }
                    if token_matches(&token) {
                        send_text(&mut ws, ok_reply("authenticated"));
                        eprintln!("hypresent: ws client {peer} authenticated");
                        break true;
                    }
                    failed_auths += 1;
                    eprintln!(
                        "hypresent: ws auth from {peer} failed: bad token (attempt {failed_auths}/{MAX_AUTH_ATTEMPTS})"
                    );
                    send_text(&mut ws, error_reply("invalid pairing token"));
                    if failed_auths >= MAX_AUTH_ATTEMPTS {
                        return;
                    }
                }
                Ok(WsRequest::Cmd(_)) => {
                    // Tolerated: the client may have reconnected and raced
                    // a command ahead of its auth. Refuse the command, keep
                    // the session in the auth phase.
                    send_text(
                        &mut ws,
                        error_reply("not authenticated: send an auth message first"),
                    );
                }
                Err(e) => {
                    send_text(&mut ws, error_reply(&e));
                }
            },
            Ok(Message::Ping(payload)) => {
                // Must go through the flushing helper like every other
                // send: a queued-but-unsent pong makes phone clients'
                // ping watchdogs kill the session after ~2 missed pings
                // (observed live with a 2000 ms ping interval).
                send_frame(&mut ws, Message::Pong(payload));
            }
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => return,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => {
                eprintln!("hypresent: ws error from {peer} during auth: {e}");
                return;
            }
        }
    };
    if !authenticated {
        return;
    }

    // -- Command phase ---------------------------------------------------------
    let mut last_activity = Instant::now();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let read_result = ws.read();
        if read_result.is_ok() {
            // Any frame — command, ping, close — proves the peer is alive.
            last_activity = Instant::now();
        }
        match read_result {
            Ok(Message::Text(text)) => match parse_request(&text) {
                Ok(WsRequest::Cmd(command)) => {
                    let is_quit = matches!(command, Command::Quit);
                    let (reply_tx, reply_rx) = std::sync::mpsc::channel();
                    if requests
                        .send(ControlRequest {
                            command,
                            reply: reply_tx,
                        })
                        .is_err()
                    {
                        send_text(&mut ws, error_reply("overlay is shutting down"));
                        return;
                    }
                    match reply_rx.recv_timeout(REPLY_TIMEOUT) {
                        Ok(reply) => {
                            // The core's error replies keep their text form;
                            // the envelope's ok flag mirrors them.
                            if reply.starts_with("error") {
                                let message =
                                    reply.strip_prefix("error: ").unwrap_or(&reply).to_string();
                                send_text(&mut ws, error_reply(&message));
                            } else {
                                send_text(&mut ws, ok_reply(&reply));
                            }
                        }
                        Err(_) => {
                            send_text(&mut ws, error_reply("overlay did not respond in time"));
                        }
                    }
                    if is_quit {
                        return;
                    }
                }
                Ok(WsRequest::Auth(_)) => {
                    send_text(&mut ws, error_reply("already authenticated"));
                }
                Err(e) => {
                    send_text(&mut ws, error_reply(&e));
                }
            },
            Ok(Message::Ping(payload)) => {
                // Must go through the flushing helper like every other
                // send: a queued-but-unsent pong makes phone clients'
                // ping watchdogs kill the session after ~2 missed pings
                // (observed live with a 2000 ms ping interval).
                send_frame(&mut ws, Message::Pong(payload));
            }
            Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                eprintln!("hypresent: ws client {peer} disconnected");
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                if last_activity.elapsed() > SESSION_IDLE_TIMEOUT {
                    eprintln!(
                        "hypresent: ws client {peer} evicted: no traffic for {}s",
                        SESSION_IDLE_TIMEOUT.as_secs()
                    );
                    return;
                }
                continue;
            }
            Err(e) => {
                eprintln!("hypresent: ws error from {peer}: {e}");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Rgb;

    #[test]
    fn sha256_known_vector_and_token_normalisation() {
        // SHA-256("abc") is a published test vector; normalize_token("a b-c")
        // is "abc", so the token hash must equal it.
        assert_eq!(
            hash_token("a b-c"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(normalize_token("AB12 cd-34"), "ab12cd34");
        assert_eq!(hash_token("AB12CD34"), hash_token("ab12 cd34"));
    }

    #[test]
    fn parses_auth_and_commands() {
        assert_eq!(
            parse_request(r#"{"v":1,"type":"auth","token":"deadbeef"}"#),
            Ok(WsRequest::Auth("deadbeef".to_string()))
        );
        assert_eq!(
            parse_request(r#"{"v":1,"cmd":"move","x":-5,"y":600}"#),
            Ok(WsRequest::Cmd(Command::Move(-5, 600)))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"radius","value":20}"#),
            Ok(WsRequest::Cmd(Command::Radius(20)))
        );
        assert_eq!(
            parse_request(r##"{"cmd":"color","color":"#33CCFF"}"##),
            Ok(WsRequest::Cmd(Command::Color(Rgb {
                r: 0x33,
                g: 0xcc,
                b: 0xff
            })))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"next"}"#),
            Ok(WsRequest::Cmd(Command::Next))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"status"}"#),
            Ok(WsRequest::Cmd(Command::Status))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"confine","output":"DP-1"}"#),
            Ok(WsRequest::Cmd(Command::Confine(Some("DP-1".to_string()))))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"confine","output":null}"#),
            Ok(WsRequest::Cmd(Command::Confine(None)))
        );
        assert_eq!(
            parse_request(r#"{"cmd":"confine"}"#),
            Ok(WsRequest::Cmd(Command::Confine(None)))
        );
    }

    #[test]
    fn rejects_bad_requests() {
        assert!(parse_request("not json").is_err());
        assert!(parse_request(r#"{"v":2,"cmd":"next"}"#).is_err());
        assert!(parse_request(r#"{"type":"auth"}"#).is_err());
        assert!(parse_request(r#"{"cmd":"move","x":1}"#).is_err());
        assert!(parse_request(r#"{"cmd":"radius","value":0}"#).is_err());
        assert!(parse_request(r#"{"cmd":"radius","value":501}"#).is_err());
        assert!(parse_request(r##"{"cmd":"color","color":"#ff33"}"##).is_err());
        assert!(parse_request(r#"{"cmd":"explode"}"#).is_err());
        assert!(parse_request(r#"{}"#).is_err());
    }

    #[test]
    fn pairing_uri_and_qr() {
        assert_eq!(
            pairing_uri("192.168.1.7", 8423, "ab12cd"),
            "hyprpresenter://pair?host=192.168.1.7&port=8423&token=ab12cd"
        );
        let qr =
            pairing_qr_terminal(&pairing_uri("192.168.1.7", 8423, "ab12cd")).expect("QR renders");
        // A scannable-looking block: several lines, roughly square, with the
        // finder pattern's solid block present.
        assert!(qr.lines().count() >= 10);
        assert!(qr.contains('█'));
    }

    #[test]
    fn envelope_shapes() {
        let ok: serde_json::Value = serde_json::from_str(&ok_reply("ok dot=1,2")).unwrap();
        assert_eq!(ok["v"], 1);
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["reply"], "ok dot=1,2");
        let err: serde_json::Value = serde_json::from_str(&error_reply("nope")).unwrap();
        assert_eq!(err["ok"], false);
        assert_eq!(err["error"], "nope");
    }
}
