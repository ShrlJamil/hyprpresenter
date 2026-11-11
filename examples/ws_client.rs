//! Minimal WebSocket test client for the Phase 5 server (development tool).
//!
//! Usage:
//!   cargo run --example ws_client -- ws://127.0.0.1:8423 TOKEN [MESSAGE ...]
//!
//! Authenticates with TOKEN (pass `NONE` to skip authentication, for
//! negative testing), then sends each MESSAGE verbatim as one text frame
//! and prints the server's reply.  With no MESSAGE it sends a `status`
//! command.  Example messages:
//!   '{"cmd":"move","x":2240,"y":540}'  '{"cmd":"next"}'  '{"cmd":"status"}'

use std::time::Duration;

use tungstenite::{stream::MaybeTlsStream, Message};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: ws_client <ws://host:port> <TOKEN|NONE> [MESSAGE ...]");
        std::process::exit(2);
    }
    let (mut ws, _response) = tungstenite::connect(&args[0]).expect("connect failed");
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    }

    if args[1] != "NONE" {
        let auth = serde_json::json!({"v":1,"type":"auth","token":args[1]}).to_string();
        ws.write(Message::text(auth)).expect("send auth");
        // tungstenite's write() only queues the frame; flush pushes it out.
        ws.flush().expect("flush auth");
        match ws.read() {
            Ok(msg) => println!("auth -> {}", msg.to_text().unwrap_or("<binary>")),
            Err(e) => println!("auth -> <closed: {e}>"),
        }
    }

    let messages: Vec<String> = if args.len() > 2 {
        args[2..].to_vec()
    } else {
        vec![r#"{"cmd":"status"}"#.to_string()]
    };
    for message in &messages {
        if ws.write(Message::text(message.clone())).is_err() {
            println!("{message} -> <send failed, connection closed>");
            break;
        }
        let _ = ws.flush();
        match ws.read() {
            Ok(msg) => println!("{message} -> {}", msg.to_text().unwrap_or("<binary>")),
            Err(e) => println!("{message} -> <closed: {e}>"),
        }
    }
}
