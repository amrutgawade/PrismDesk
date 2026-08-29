//! PrismDesk Phase-0 Spike B — scrcpy socket -> raw H.264 dump.
//!
//! Proves the transport pipe end-to-end: push the pinned scrcpy 3.3.1 server via
//! the bundled adb, start it over a USB **`adb reverse`** tunnel (the plan's USB
//! transport), accept its video socket, and dump the raw H.264 elementary
//! stream to a playable `.h264` file.
//!
//! Reverse tunnel (tunnel_forward=false): the PC listens and the *device*
//! connects out to us, so there is no connect race and no dummy-byte handshake.
//! The server is told to omit ALL framing (send_device_meta / send_codec_meta /
//! send_frame_meta / send_dummy_byte = false), so the socket carries only the
//! H.264 elementary stream. Phase 1 flips send_frame_meta back on for PTS.
//!
//! Requires: an Android device connected with USB debugging authorized.
//! Run:   cargo run -p scrcpy-dump            (captures ~5s)
//!        cargo run -p scrcpy-dump -- 10      (captures ~10s)

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SCRCPY_VERSION: &str = "3.3.1";
const DEVICE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";
const LOCAL_PORT: u16 = 27183;

fn adb_path() -> PathBuf {
    let bundled = Path::new(r"C:\platform-tools\adb.exe");
    if bundled.exists() {
        bundled.to_path_buf()
    } else {
        PathBuf::from("adb")
    }
}

fn server_jar() -> PathBuf {
    PathBuf::from("assets/server/scrcpy-server-v3.3.1.jar")
}

fn adb(args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new(adb_path()).args(args).output()
}

fn scid() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0x1234_5678);
    format!("{:08x}", nanos & 0x7fff_ffff)
}

fn main() {
    match run() {
        Ok(bytes) if bytes > 0 => {
            println!("\n[PASS] Received a real H.264 stream over the adb reverse tunnel.");
            println!("       => Transport pipe proven. Phase 1 can build the decode/render loop.");
        }
        Ok(_) => {
            eprintln!("\n[FAIL] Connected but received 0 bytes — see the [srv] log above.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("\n[error] {e}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<u64, String> {
    let seconds: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);

    println!("PrismDesk · Phase-0 Spike B — scrcpy {SCRCPY_VERSION} -> raw H.264 dump");
    println!("--------------------------------------------------------------------");
    println!("adb        : {}", adb_path().display());

    let jar = server_jar();
    if !jar.exists() {
        return Err(format!("pinned server jar not found at {} (run from workspace root)", jar.display()));
    }

    // --- device present? -------------------------------------------------
    let devices = adb(&["devices"]).map_err(|e| format!("failed to run adb: {e}"))?;
    let list = String::from_utf8_lossy(&devices.stdout);
    println!("\n[devices]\n{}", list.trim_end());
    if !list.lines().skip(1).any(|l| l.trim_end().ends_with("\tdevice")) {
        return Err("no authorized device (enable USB debugging + accept the RSA prompt)".into());
    }

    // --- push the pinned server -----------------------------------------
    println!("\n[push] {} -> {DEVICE_JAR}", jar.display());
    let push = adb(&["push", jar.to_str().unwrap(), DEVICE_JAR]).map_err(|e| format!("adb push spawn: {e}"))?;
    if !push.status.success() {
        return Err(format!("adb push failed: {}", String::from_utf8_lossy(&push.stderr)));
    }

    // --- PC listens; reverse-tunnel the device socket to us --------------
    let listener = TcpListener::bind(("127.0.0.1", LOCAL_PORT))
        .map_err(|e| format!("bind 127.0.0.1:{LOCAL_PORT}: {e}"))?;
    listener.set_nonblocking(true).ok();

    let scid = scid();
    let sock_name = format!("localabstract:scrcpy_{scid}");
    let tcp_spec = format!("tcp:{LOCAL_PORT}");
    println!("[reverse] {sock_name} -> {tcp_spec}");
    let rev = adb(&["reverse", &sock_name, &tcp_spec]).map_err(|e| format!("adb reverse spawn: {e}"))?;
    if !rev.status.success() {
        return Err(format!("adb reverse failed: {}", String::from_utf8_lossy(&rev.stderr)));
    }
    let _rev_guard = ReverseGuard { remote: sock_name.clone() };

    // --- launch the server (video-only, no framing, reverse tunnel) ------
    let classpath = format!("CLASSPATH={DEVICE_JAR}");
    let server_args = [
        "shell",
        &classpath,
        "app_process",
        "/",
        "com.genymobile.scrcpy.Server",
        SCRCPY_VERSION,
        &format!("scid={scid}"),
        "log_level=debug",
        "tunnel_forward=false", // reverse: the device connects out to us
        "audio=false",
        "control=false",
        "cleanup=true",
        "video_codec=h264",
        "max_size=1280",
        "video_bit_rate=8000000",
        "max_fps=60",
        "send_device_meta=false",
        "send_codec_meta=false",
        "send_frame_meta=false",
        "send_dummy_byte=false",
    ];
    println!("[server] app_process com.genymobile.scrcpy.Server {SCRCPY_VERSION} scid={scid} (reverse, video-only, no framing)");
    let mut server: Child = Command::new(adb_path())
        .args(server_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn server: {e}"))?;
    spawn_log_reader(server.stdout.take(), "srv");
    spawn_log_reader(server.stderr.take(), "srv");
    let _srv_guard = ChildGuard(&mut server as *mut Child);

    // --- accept the device's outbound connection -------------------------
    let mut stream = match accept_retry(&listener, Duration::from_secs(5)) {
        Some(s) => s,
        None => {
            std::thread::sleep(Duration::from_millis(500)); // let the log flush
            return Err("device never connected back — see [srv] log above".into());
        }
    };
    stream.set_nonblocking(false).ok();
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok();
    println!("[socket] device connected");

    // --- capture -> file -------------------------------------------------
    std::fs::create_dir_all("captures").ok();
    let out_path = format!("captures/spikeb-{scid}.h264");
    let mut out = std::fs::File::create(&out_path).map_err(|e| format!("create {out_path}: {e}"))?;

    println!("\n[capture] dumping ~{seconds}s to {out_path} ...");
    let mut buf = vec![0u8; 256 * 1024];
    let mut total: u64 = 0;
    let mut first_bytes: Vec<u8> = Vec::new();
    let start = Instant::now();
    let mut last_report = Instant::now();
    while start.elapsed() < Duration::from_secs(seconds) {
        match stream.read(&mut buf) {
            Ok(0) => {
                println!("[socket] server closed the stream");
                break;
            }
            Ok(n) => {
                if first_bytes.len() < 8 {
                    first_bytes.extend_from_slice(&buf[..n.min(8 - first_bytes.len())]);
                }
                out.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
                total += n as u64;
                if last_report.elapsed() >= Duration::from_millis(1000) {
                    let mbps = (total as f64 * 8.0 / 1_000_000.0) / start.elapsed().as_secs_f64();
                    println!("   {:>7.2} MB   ~{:>5.1} Mbps", total as f64 / (1024.0 * 1024.0), mbps);
                    last_report = Instant::now();
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                eprintln!("[socket] read error: {e}");
                break;
            }
        }
    }
    out.flush().ok();

    println!("\n[result] {total} bytes ({:.2} MB) -> {out_path}", total as f64 / (1024.0 * 1024.0));
    if total > 0 {
        let hex: Vec<String> = first_bytes.iter().map(|b| format!("{b:02x}")).collect();
        let looks_h264 = first_bytes.starts_with(&[0, 0, 0, 1]) || first_bytes.starts_with(&[0, 0, 1]);
        println!(
            "         first bytes: {}   {}",
            hex.join(" "),
            if looks_h264 { "<- valid H.264 Annex-B start code" } else { "(no start code?)" }
        );
        println!("         Play it:  ffplay {out_path}    (or drop into VLC)");
    }
    Ok(total)
}

fn spawn_log_reader(pipe: Option<impl Read + Send + 'static>, tag: &'static str) {
    if let Some(p) = pipe {
        std::thread::spawn(move || {
            for line in BufReader::new(p).lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    println!("[{tag}] {line}");
                }
            }
        });
    }
}

fn accept_retry(listener: &TcpListener, budget: Duration) -> Option<TcpStream> {
    let start = Instant::now();
    while start.elapsed() < budget {
        match listener.accept() {
            Ok((s, _)) => return Some(s),
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    }
    None
}

struct ReverseGuard {
    remote: String,
}
impl Drop for ReverseGuard {
    fn drop(&mut self) {
        let _ = adb(&["reverse", "--remove", &self.remote]);
    }
}

struct ChildGuard(*mut Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = (*self.0).kill();
        }
    }
}
