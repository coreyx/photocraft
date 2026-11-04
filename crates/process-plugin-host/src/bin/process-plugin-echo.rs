//! `process-plugin-echo`: a minimal process plug-in for testing the host.
//!
//! It speaks the protocol over stdin/stdout like a real plug-in, and its first argument picks a
//! behaviour, so the host's integration tests can exercise every path with a real process:
//!
//! | Mode | Behaviour |
//! |---|---|
//! | `conformance` | says hello, asks the host for `doc.list`, answers pings, exits on `shutdown` |
//! | `crash` | says hello, then exits with code 7 |
//! | `invalid-frame` | says hello, then writes bytes that aren't a frame |
//! | `oversize` | says hello, then announces a frame larger than any limit |
//! | `wrong-nonce` | says hello with a nonce it made up |
//! | `hang` | says hello, then stops reading and answering anything |
//! | `ignore-shutdown` | like `conformance`, but ignores `shutdown` (and keeps a heartbeat file) |
//!
//! Everything it receives is written to stderr, which the host keeps in the plug-in's log.

use std::io::{self, BufReader, BufWriter, Write};
use std::time::Duration;

use photocraft_process_plugin_protocol::PROTOCOL_VERSION;
use photocraft_process_plugin_protocol::frame::{self, Frame, Limits};
use photocraft_process_plugin_protocol::message::Header;
use photocraft_process_plugin_protocol::method::{self, HelloParams, Method, PluginIdentity};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "conformance".into());
    if let Err(e) = run(&mode) {
        eprintln!("echo plug-in ({mode}): {e}");
        std::process::exit(2);
    }
}

fn run(mode: &str) -> Result<(), String> {
    let limits = Limits::default();
    let mut input = BufReader::new(io::stdin().lock());
    let mut output = BufWriter::new(io::stdout().lock());
    let nonce = std::env::var("PHOTOCRAFT_PLUGIN_NONCE").map_err(|_| "no PHOTOCRAFT_PLUGIN_NONCE")?;
    let nonce = if mode == "wrong-nonce" { "not-the-nonce".to_string() } else { nonce };
    let hello = HelloParams { protocol: PROTOCOL_VERSION, nonce, plugin: PluginIdentity { id: "org.example.echo".into(), version: "1.0.0".into() } };
    send(&mut output, Header::request::<method::Hello>(1, &hello).map_err(|e| e.message)?, limits)?;
    let welcome = frame::read_frame(&mut input, limits).map_err(|e| e.to_string())?.ok_or("closed before welcome")?;
    eprintln!("welcome: {:?}", welcome.header);
    if !matches!(welcome.header, Header::Response { id: 1, .. }) {
        return Err("the host refused hello".into());
    }

    match mode {
        "crash" => std::process::exit(7),
        "invalid-frame" => {
            output.write_all(b"definitely not a frame, just text").map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_secs(30));
            return Ok(());
        }
        "oversize" => {
            let mut prefix = b"PCPP".to_vec();
            prefix.extend_from_slice(&2u32.to_le_bytes());
            prefix.extend_from_slice(&u64::MAX.to_le_bytes());
            output.write_all(&prefix).map_err(|e| e.to_string())?;
            output.flush().map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_secs(30));
            return Ok(());
        }
        "hang" => {
            // Alive but silent: never reads or answers again.
            std::thread::sleep(Duration::from_secs(120));
            return Ok(());
        }
        _ => {}
    }

    if mode == "ignore-shutdown" {
        heartbeat();
    }
    send(&mut output, Header::Request { id: 2, method: "doc.list".into(), params: serde_json::Value::Null }, limits)?;
    while let Some(frame) = frame::read_frame(&mut input, limits).map_err(|e| e.to_string())? {
        eprintln!("received: {:?}", frame.header);
        if let Header::Request { id, method, .. } = &frame.header {
            if method == method::Ping::NAME {
                send(&mut output, Header::response(*id, &method::Empty {}).map_err(|e| e.message)?, limits)?;
            } else if method == method::Shutdown::NAME && mode != "ignore-shutdown" {
                send(&mut output, Header::response(*id, &method::Empty {}).map_err(|e| e.message)?, limits)?;
                return Ok(());
            }
        }
    }
    Ok(())
}

fn send(output: &mut impl Write, header: Header, limits: Limits) -> Result<(), String> {
    frame::write_frame(output, &Frame::new(header), limits).map_err(|e| e.to_string())
}

/// Touch `<data dir>/heartbeat` every 50 ms, so a test can tell whether the process still runs.
fn heartbeat() {
    let Ok(dir) = std::env::var("PHOTOCRAFT_PLUGIN_DATA_DIR") else { return };
    let path = std::path::Path::new(&dir).join("heartbeat");
    std::thread::spawn(move || {
        let mut beat: u64 = 0;
        loop {
            beat = beat.wrapping_add(1);
            let _ = std::fs::write(&path, beat.to_string());
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}
