//! Phase-1d transport: bring up a live scrcpy video session over USB.
//!
//! Uses the plan's USB path — an `adb reverse` tunnel (device dials out to the
//! PC) — with scrcpy frame metadata ON, so each access unit is prefixed by the
//! 12-byte header (PTS + config/keyframe flags + length). Video-only for now.
//!
//! This is the throwaway-friendly Phase-1 version living in pd-engine; it moves
//! to the `pd-adb` crate with discovery/reconnect in Phase 6.

use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SCRCPY_VERSION: &str = "3.3.1";
const DEVICE_JAR: &str = "/data/local/tmp/scrcpy-server.jar";

/// One decoded-stream access unit off the wire.
pub struct Au {
    pub data: Vec<u8>,
    pub pts_100ns: i64,
    pub is_config: bool,
    pub is_key: bool,
}

/// Owns the server process + reverse tunnel; tears them down on drop.
pub struct Session {
    _server: ChildGuard,
    _reverse: ReverseGuard,
}

fn adb_path() -> PathBuf {
    crate::adb_path()
}

/// Locate the pinned scrcpy server jar. Prefers a copy bundled next to the exe
/// (portable/installed layout), then the dev checkout's `assets/server/`.
fn server_jar() -> PathBuf {
    const NAME: &str = "scrcpy-server-v3.3.1.jar";
    let rel = Path::new("assets").join("server").join(NAME);
    if let Some(dir) = crate::exe_dir() {
        for c in [dir.join(NAME), dir.join(&rel)] {
            if c.exists() {
                return c;
            }
        }
    }
    // Dev fallback: relative to the current working directory.
    if Path::new(NAME).exists() {
        return PathBuf::from(NAME);
    }
    rel
}

fn adb_cmd(serial: &Option<String>) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new(adb_path());
    c.creation_flags(crate::CREATE_NO_WINDOW); // no flashing console for adb
    if let Some(s) = serial {
        c.arg("-s").arg(s);
    }
    c
}

fn adb_run(serial: &Option<String>, args: &[&str]) -> io::Result<std::process::Output> {
    adb_cmd(serial).args(args).output()
}

fn scid() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0x1357_9bdf);
    format!("{:08x}", n & 0x7fff_ffff)
}

/// Start a video session and return the connected socket + a session guard.
#[allow(clippy::type_complexity)]
pub fn start(
    max_size: u32,
    bitrate: u32,
    fps: u32,
    codec: &str,
    audio_on: bool,
    control_on: bool,
    serial: Option<String>,
) -> Result<(Session, TcpStream, Option<TcpStream>, Option<TcpStream>), String> {
    let jar = server_jar();
    if !jar.exists() {
        return Err(format!("pinned server jar not found at {}", jar.display()));
    }

    // Confirm the target device is present + authorized (-s targets it).
    let state = adb_run(&serial, &["get-state"]).map_err(|e| format!("adb: {e}"))?;
    if String::from_utf8_lossy(&state.stdout).trim() != "device" {
        return Err("device not authorized (USB debugging on + RSA accepted)".into());
    }

    let push = adb_run(&serial, &["push", jar.to_str().unwrap(), DEVICE_JAR])
        .map_err(|e| format!("push: {e}"))?;
    if !push.status.success() {
        return Err(format!("adb push failed: {}", String::from_utf8_lossy(&push.stderr)));
    }

    // Ephemeral local port so multiple mirrors can run at once.
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("bind: {e}"))?;
    let port = listener.local_addr().map_err(|e| format!("addr: {e}"))?.port();
    listener.set_nonblocking(true).ok();

    let scid = scid();
    let sock_name = format!("localabstract:scrcpy_{scid}");
    let rev = adb_run(&serial, &["reverse", &sock_name, &format!("tcp:{port}")])
        .map_err(|e| format!("reverse: {e}"))?;
    if !rev.status.success() {
        return Err(format!("adb reverse failed: {}", String::from_utf8_lossy(&rev.stderr)));
    }
    let reverse_guard = ReverseGuard {
        remote: sock_name,
        serial: serial.clone(),
    };

    let classpath = format!("CLASSPATH={DEVICE_JAR}");
    let args = [
        "shell",
        &classpath,
        "app_process",
        "/",
        "com.genymobile.scrcpy.Server",
        SCRCPY_VERSION,
        &format!("scid={scid}"),
        "log_level=info",
        "tunnel_forward=false",
        &format!("audio={audio_on}"),
        "audio_codec=raw", // 48 kHz stereo s16le PCM, no decode needed
        &format!("control={control_on}"),
        "cleanup=true",
        &format!("video_codec={codec}"),
        "video_codec_options=i-frame-interval:int=1", // keyframe every 1s (recording/OBS/reconnect)
        &format!("max_size={max_size}"),
        &format!("video_bit_rate={bitrate}"),
        &format!("max_fps={fps}"),
        "send_device_meta=false",
        "send_codec_meta=false",
        "send_frame_meta=true", // 12-byte header per AU: PTS + config/key flags + len
        "send_dummy_byte=false",
    ];
    let child = adb_cmd(&serial)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn server: {e}"))?;
    let server_guard = ChildGuard(child.into());

    // scrcpy dials out in order: video, then audio (if enabled).
    let video = accept(&listener, Duration::from_secs(5))
        .ok_or_else(|| "device never connected back (reverse tunnel)".to_string())?;
    video.set_nodelay(true).ok(); // Nagle off for lowest latency

    let audio = if audio_on {
        let a = accept(&listener, Duration::from_secs(5));
        if let Some(s) = &a {
            s.set_nodelay(true).ok();
        }
        a
    } else {
        None
    };

    let control = if control_on {
        let c = accept(&listener, Duration::from_secs(5));
        if let Some(s) = &c {
            s.set_nodelay(true).ok();
        }
        c
    } else {
        None
    };

    Ok((
        Session {
            _server: server_guard,
            _reverse: reverse_guard,
        },
        video,
        audio,
        control,
    ))
}

/// Read one framed access unit (blocks until a full AU is available).
pub fn read_frame(stream: &mut impl Read) -> io::Result<Au> {
    let mut hdr = [0u8; 12];
    stream.read_exact(&mut hdr)?;
    let pts_flags = u64::from_be_bytes(hdr[0..8].try_into().unwrap());
    let len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;

    let is_config = (pts_flags >> 63) & 1 == 1;
    let is_key = (pts_flags >> 62) & 1 == 1;
    let pts_us = (pts_flags & ((1u64 << 62) - 1)) as i64;

    let mut data = vec![0u8; len];
    stream.read_exact(&mut data)?;
    Ok(Au {
        data,
        pts_100ns: if is_config { 0 } else { pts_us * 10 },
        is_config,
        is_key,
    })
}

fn accept(listener: &TcpListener, budget: Duration) -> Option<TcpStream> {
    let start = Instant::now();
    while start.elapsed() < budget {
        match listener.accept() {
            Ok((s, _)) => {
                s.set_nonblocking(false).ok();
                return Some(s);
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(_) => return None,
        }
    }
    None
}

struct ReverseGuard {
    remote: String,
    serial: Option<String>,
}
impl Drop for ReverseGuard {
    fn drop(&mut self) {
        let _ = adb_run(&self.serial, &["reverse", "--remove", &self.remote]);
    }
}

struct ChildGuard(std::cell::RefCell<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.borrow_mut().kill();
    }
}
