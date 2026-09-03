//! Diagnostic: exercise the mirror's TCP control channel exactly as the
//! dashboard does — bind a localhost listener, spawn `pd-engine --mirror
//! --ctrl-port <port>`, accept the child's dial-back, then send SPACED commands
//! (~1s apart, the case that lost every other command over stdin).
//!
//! Run: cargo run -p pd-engine --example ipc_probe -- <path-to-pd-engine.exe>

use std::io::Write;
use std::net::TcpListener;
use std::process::Command;
use std::time::Duration;

fn main() {
    let exe = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/debug/pd-engine.exe".to_string());

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let mut child = Command::new(&exe)
        .arg("--mirror")
        .arg("--serial")
        .arg("FAKEPROBE")
        .arg("--ctrl-port")
        .arg(port.to_string())
        .env("PRISMDESK_DEBUG", "1")
        .spawn()
        .expect("spawn mirror");

    let (mut stream, _) = listener.accept().expect("accept");
    eprintln!("probe: mirror connected on port {port}");

    for cmd in ["snapshot", "record", "record", "mute", "mute", "stop"] {
        std::thread::sleep(Duration::from_millis(1000));
        let r = stream.write_all(format!("{cmd}\n").as_bytes()).and_then(|_| stream.flush());
        eprintln!("probe: sent {cmd:?} -> {r:?}");
    }

    let status = child.wait();
    eprintln!("probe: child exited: {status:?}");
}
