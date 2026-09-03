//! PrismDesk engine — Phase 1 core mirror.
//!
//! Milestone 1c: decode the canned scrcpy capture (H.264 -> NVDEC -> NV12) and
//! render each frame into the dedicated flip-model window via the BT.709
//! YUV->RGB shader. This is the full decode->render path against a file; 1d
//! swaps the file for the live reverse-tunnel socket.
//!
//! Run:  cargo run -p pd-engine                 (play the canned capture ~12s)
//!       cargo run -p pd-engine -- blank         (milestone 1a: animated clear)

use std::time::{Duration, Instant};

use pd_decode::Decoder;
use pd_render::Mirror;

mod transport;

/// Live capture/quality configuration (the product's resolution/fps/quality
/// controls). Defaults = the verified "Balanced" preset: 1600 long-edge source
/// (supersamples a 1080p panel), H.264 (lowest latency), 20 Mbps, 60 fps.
struct Config {
    max_size: u32,
    bitrate: u32,
    fps: u32,
    codec: String,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_size: 1600, bitrate: 20_000_000, fps: 60, codec: "h264".into() }
    }
}

fn main() -> windows_core::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("blank") => blank_demo(),               // 1a: animated clear
        Some("file") => play_capture(),              // 1c: play the canned capture
        _ => live_mirror(parse_config(&args)),       // 1d: live mirror (USB)
    }
}

fn parse_config(args: &[String]) -> Config {
    let mut c = Config::default();
    // Presets first so explicit flags can still override them.
    if let Some(p) = flag_value(args, "--preset") {
        match p.as_str() {
            "crisp" | "reading" => {
                c = Config { max_size: 1920, bitrate: 28_000_000, fps: 60, codec: "h265".into() }
            }
            "lowlatency" | "low" => {
                c = Config { max_size: 1366, bitrate: 15_000_000, fps: 90, codec: "h264".into() }
            }
            _ => {} // "balanced" == defaults
        }
    }
    if let Some(v) = flag_value(args, "--max-size").and_then(|s| s.parse().ok()) {
        c.max_size = v;
    }
    if let Some(v) = flag_value(args, "--bitrate").and_then(|s| s.parse::<u32>().ok()) {
        c.bitrate = v * 1_000_000; // Mbps -> bps
    }
    if let Some(v) = flag_value(args, "--fps").and_then(|s| s.parse().ok()) {
        c.fps = v;
    }
    if let Some(v) = flag_value(args, "--codec") {
        c.codec = v;
    }
    c
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// Phase 1d — live mirror. Bring up the reverse-tunnel video session, read
/// framed access units on a net thread, and decode+present on the main thread
/// with present-on-arrival + drop-to-latest so latency can never accumulate.
fn live_mirror(cfg: Config) -> windows_core::Result<()> {
    use std::sync::mpsc;

    // Window persists across reconnects; only the session + decoder recycle.
    let mut mirror = Mirror::new(500, 1040, "PrismDesk — Live Mirror")?;
    println!("PrismDesk · live mirror (USB, adb reverse)");
    println!(
        "  {} · max_size={} · {} Mbps · {} fps · F11 fullscreen · close window to stop",
        cfg.codec, cfg.max_size, cfg.bitrate / 1_000_000, cfg.fps
    );

    let mut backoff = Duration::from_millis(200);
    let cap = Duration::from_secs(5);

    'session: loop {
        // Bring up a session; on failure (device unplugged/sleeping) retry with
        // bounded backoff while the window stays responsive.
        let (session, stream) = match transport::start(cfg.max_size, cfg.bitrate, cfg.fps, &cfg.codec)
        {
            Ok(s) => {
                backoff = Duration::from_millis(200);
                println!("[transport] connected");
                s
            }
            Err(e) => {
                eprintln!("[transport] {e} — retry in {:.1}s", backoff.as_secs_f32());
                if pump_for(&mut mirror, backoff) {
                    break 'session;
                }
                backoff = (backoff * 2).min(cap);
                continue;
            }
        };

        let (tx, rx) = mpsc::channel::<transport::Au>();
        let mut netstream = stream;
        let net = std::thread::spawn(move || loop {
            match transport::read_frame(&mut netstream) {
                Ok(au) => {
                    if tx.send(au).is_err() {
                        break;
                    }
                }
                Err(_) => break, // EOF / disconnect
            }
        });

        let mut dec = Decoder::new(mirror.device())?;
        let mut out = Vec::new();

        // Returns true if the user closed the window, false if the stream dropped.
        let user_quit = 'stream: loop {
            if mirror.pump() {
                break 'stream true;
            }
            let mut latest = None;
            loop {
                match rx.try_recv() {
                    Ok(au) => {
                        if let Err(e) = dec.decode(&au.data, au.pts_100ns, &mut out) {
                            eprintln!("[decode] {e:?}");
                        }
                        for f in out.drain(..) {
                            latest = Some(f); // older frames drop -> back to the pool
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => break 'stream false,
                }
            }
            if let Some(f) = latest.take() {
                mirror.render_frame(&f.texture, f.subresource, f.width, f.height)?;
            } else {
                std::thread::sleep(Duration::from_millis(1)); // idle
            }
        };

        drop(session); // kill the server + remove the reverse tunnel
        let _ = net.join();

        if user_quit {
            break 'session;
        }
        println!("[transport] stream ended — reconnecting");
        if pump_for(&mut mirror, backoff) {
            break 'session;
        }
        backoff = (backoff * 2).min(cap);
    }

    println!("[ok] live mirror closed");
    Ok(())
}

/// Pump window messages for `dur` so the window stays responsive/closable while
/// idle (e.g. during a reconnect backoff). Returns true if the window closed.
fn pump_for(mirror: &mut Mirror, dur: Duration) -> bool {
    let end = Instant::now() + dur;
    while Instant::now() < end {
        if mirror.pump() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

fn play_capture() -> windows_core::Result<()> {
    let path = find_capture();
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let nals = split_nals(&data);

    let mut mirror = Mirror::new(432, 960, "PrismDesk — Mirror (Phase 1c)")?;
    let mut dec = Decoder::new(mirror.device())?;
    println!("PrismDesk engine · milestone 1c");
    println!("  rendering {path} on {}", mirror.adapter_name());
    println!("  {} NALs · looping playback (~12s, or close the window)", nals.len());

    let start = Instant::now();
    let target_dt = Duration::from_millis(33); // ~30 fps playback pacing
    let mut shown = 0u64;

    'outer: loop {
        let mut out = Vec::new();
        for (i, nal) in nals.iter().enumerate() {
            if mirror.pump() {
                break 'outer;
            }
            dec.decode(nal, (i as i64) * 333_667, &mut out)?;
            for frame in out.drain(..) {
                let t0 = Instant::now();
                mirror.render_frame(&frame.texture, frame.subresource, frame.width, frame.height)?;
                shown += 1;
                drop(frame); // return the surface to the decoder pool
                if let Some(rem) = target_dt.checked_sub(t0.elapsed()) {
                    std::thread::sleep(rem);
                }
                if mirror.pump() {
                    break 'outer;
                }
            }
        }
        dec.finish(&mut out)?;
        for frame in out.drain(..) {
            mirror.render_frame(&frame.texture, frame.subresource, frame.width, frame.height)?;
            shown += 1;
            std::thread::sleep(target_dt);
        }
        // Loop the capture until the window is closed so you can inspect it.
        let _ = start;
        // Re-create the decoder to replay the file cleanly after DRAIN.
        dec = Decoder::new(mirror.device())?;
    }

    println!("[ok] rendered {shown} frames. Window closed cleanly.");
    Ok(())
}

fn blank_demo() -> windows_core::Result<()> {
    let mut mirror = Mirror::new(1280, 720, "PrismDesk — Mirror (1a)")?;
    println!("milestone 1a · animated clear on {}", mirror.adapter_name());
    let start = Instant::now();
    while !mirror.pump() {
        let t = start.elapsed().as_secs_f32();
        let a = (t * 0.7).sin() * 0.5 + 0.5;
        mirror.present([0.03 + 0.10 * a, 0.06 + 0.16 * (1.0 - a), 0.10 + 0.34 * a, 1.0])?;
        if start.elapsed() > Duration::from_secs(8) {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}

fn find_capture() -> String {
    if let Ok(rd) = std::fs::read_dir("captures") {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("h264") {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    panic!("no captures/*.h264 — run `cargo run -p scrcpy-dump` with a device first");
}

/// Split an Annex-B stream into NAL units (each slice keeps its start code).
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(if i > 0 && data[i - 1] == 0 { i - 1 } else { i });
            i += 3;
        } else {
            i += 1;
        }
    }
    (0..starts.len())
        .map(|k| {
            let end = if k + 1 < starts.len() { starts[k + 1] } else { data.len() };
            &data[starts[k]..end]
        })
        .collect()
}
