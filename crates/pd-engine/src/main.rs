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

mod aac;
mod control;
mod dashboard;
mod record;
mod transport;

/// Live capture/quality configuration (the product's resolution/fps/quality
/// controls). Defaults = the verified "Balanced" preset: 1600 long-edge source
/// (supersamples a 1080p panel), H.264 (lowest latency), 20 Mbps, 60 fps.
struct Config {
    max_size: u32,
    bitrate: u32,
    fps: u32,
    codec: String,
    audio: bool,
    serial: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_size: 1600,
            bitrate: 20_000_000,
            fps: 60,
            codec: "h264".into(),
            audio: true,
            serial: None,
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str);
    let r: Result<(), Box<dyn std::error::Error>> = match mode {
        Some("--mirror") => live_mirror(parse_config(&args)).map_err(Into::into),
        Some("blank") => blank_demo().map_err(Into::into),
        Some("file") => play_capture().map_err(Into::into),
        _ => dashboard::run().map_err(Into::into), // default = dashboard (control plane)
    };
    if let Err(e) = r {
        // A spawned mirror has no console, so surface fatal errors in a dialog.
        if mode == Some("--mirror") {
            show_error(&format!("PrismDesk couldn't start the mirror:\n\n{e}"));
        }
        eprintln!("[error] {e}");
        std::process::exit(1);
    }
}

/// Show a modal message box (used so a spawned mirror never fails silently).
fn show_error(msg: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    unsafe {
        let _ = MessageBoxW(
            None,
            &HSTRING::from(msg),
            &HSTRING::from("PrismDesk"),
            MB_OK | MB_ICONWARNING,
        );
    }
}

fn parse_config(args: &[String]) -> Config {
    let mut c = Config::default();
    // Presets first so explicit flags can still override them.
    if let Some(p) = flag_value(args, "--preset") {
        match p.as_str() {
            "crisp" | "reading" => {
                c = Config {
                    max_size: 1920,
                    bitrate: 28_000_000,
                    fps: 60,
                    codec: "h265".into(),
                    audio: c.audio,
                    serial: c.serial.clone(),
                }
            }
            "lowlatency" | "low" => {
                c = Config {
                    max_size: 1366,
                    bitrate: 15_000_000,
                    fps: 90,
                    codec: "h264".into(),
                    audio: c.audio,
                    serial: c.serial.clone(),
                }
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
    if let Some(v) = flag_value(args, "--serial") {
        c.serial = Some(v);
    }
    if args.iter().any(|a| a == "--no-audio") {
        c.audio = false;
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
        "  {} · {} · {}Mbps · {}fps · mouse+keyboard · F11 fs · Ctrl+M/S/R/V · close",
        cfg.codec, cfg.max_size, cfg.bitrate / 1_000_000, cfg.fps
    );

    // Audio output (kept alive for the whole session); disabled if no endpoint.
    let player = pd_audio::Player::new();
    let sink = match &player {
        Ok(p) => {
            println!("[audio] {}", p.info());
            Some(p.sink())
        }
        Err(e) => {
            eprintln!("[audio] disabled: {e}");
            None
        }
    };
    let want_audio = cfg.audio && sink.is_some();

    let mut backoff = Duration::from_millis(200);
    let cap = Duration::from_secs(5);
    let mut codec = cfg.codec.clone(); // may downgrade h265 -> h264 if HEVC decode is absent
    let mut recorder: Option<record::Recorder> = None; // persists across reconnects
    // Rolling current-GOP buffer so recording can start instantly from the last
    // keyframe (this device's keyframes are ~10 s apart).
    let mut gop_config: Option<Vec<u8>> = None;
    let mut gop: Vec<(Vec<u8>, i64, bool)> = Vec::new(); // (data, pts_us, is_key)
    let rec_audio: std::sync::Arc<std::sync::Mutex<Vec<u8>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); // PCM tap for recording

    'session: loop {
        // Bring up a session; on failure (device unplugged/sleeping) retry with
        // bounded backoff while the window stays responsive.
        let (session, stream, audio_stream, control_stream) = match transport::start(
            cfg.max_size,
            cfg.bitrate,
            cfg.fps,
            &codec,
            want_audio,
            true, // control (mouse) on
            cfg.serial.clone(),
        ) {
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

        // Audio producer: raw PCM off the audio socket -> sink; ends on close.
        let ra = rec_audio.clone();
        let audio_net = match (audio_stream, sink.clone()) {
            (Some(mut a), Some(sk)) => Some(std::thread::spawn(move || loop {
                match transport::read_frame(&mut a) {
                    Ok(au) => {
                        sk.push_pcm_s16(&au.data);
                        if let Ok(mut b) = ra.lock() {
                            if b.len() < 48000 * 2 * 2 {
                                b.extend_from_slice(&au.data); // ~1 s cap
                            }
                        }
                    }
                    Err(_) => break,
                }
            })),
            _ => None,
        };

        let mut dec = match Decoder::new(mirror.device(), &codec) {
            Ok(d) => d,
            Err(e) => {
                // Tear down this session; the codec may not be decodable here.
                drop(session);
                let _ = net.join();
                if let Some(h) = audio_net {
                    let _ = h.join();
                }
                let hevc = codec.eq_ignore_ascii_case("h265") || codec.eq_ignore_ascii_case("hevc");
                if hevc {
                    show_error(
                        "HEVC (Crisp preset) can't be decoded on this PC.\n\nInstall \"HEVC Video Extensions\" from the Microsoft Store to use Crisp, or use the Balanced / Low-latency preset.\n\nFalling back to H.264 now.",
                    );
                    codec = "h264".to_string();
                } else {
                    show_error(&format!("Failed to start the video decoder:\n\n{e}"));
                    break 'session;
                }
                if pump_for(&mut mirror, backoff) {
                    break 'session;
                }
                continue 'session;
            }
        };
        let mut out = Vec::new();
        let mut control = control_stream;
        let mut mouse_down = false;
        // Clipboard: device -> PC arrives on the control socket (read on a clone).
        let clip_in: std::sync::Arc<std::sync::Mutex<Option<String>>> =
            std::sync::Arc::new(std::sync::Mutex::new(None));
        let clip_reader = control.as_ref().and_then(|c| c.try_clone().ok()).map(|mut rc| {
            let ci = clip_in.clone();
            std::thread::spawn(move || loop {
                match control::read_device_msg(&mut rc) {
                    Ok(Some(t)) => {
                        if let Ok(mut g) = ci.lock() {
                            *g = Some(t);
                        }
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            })
        });

        // Returns true if the user closed the window, false if the stream dropped.
        let user_quit = 'stream: loop {
            if mirror.pump() {
                break 'stream true;
            }
            if mirror.mute_toggled() {
                if let Some(sk) = &sink {
                    println!("[audio] {}", if sk.toggle_mute() { "muted" } else { "unmuted" });
                }
            }
            if mirror.shot_requested() {
                let path = screenshot_path();
                match mirror.screenshot(&path) {
                    Ok(true) => println!("[shot] saved {path}"),
                    Ok(false) => eprintln!("[shot] no frame yet"),
                    Err(e) => eprintln!("[shot] {e:?}"),
                }
            }
            if mirror.rec_toggled() {
                if let Some(r) = recorder.take() {
                    match r.finish() {
                        Some((p, n)) => println!("[rec] saved {p} ({n} frames)"),
                        None => eprintln!("[rec] nothing recorded"),
                    }
                } else {
                    let (w, h) = mirror.video_size();
                    if let (Some(cfg), true) = (&gop_config, w > 0 && h > 0 && !gop.is_empty()) {
                        let mut r = record::Recorder::new(rec_path(), w as u16, h as u16);
                        r.feed(cfg, 0, true, true); // SPS/PPS
                        for (d, pts, key) in &gop {
                            r.feed(d, *pts, *key, false); // replay current GOP (starts at keyframe)
                        }
                        recorder = Some(r);
                        println!("[rec] recording... press Ctrl+R to stop");
                    } else {
                        eprintln!("[rec] no keyframe buffered yet, try again in a moment");
                    }
                }
            }
            // Clipboard PC -> device (V), and device -> PC (auto).
            if mirror.paste_requested() {
                if let Some(ctl) = &mut control {
                    if let Ok(t) = clipboard_win::get_clipboard_string() {
                        control::send(ctl, &control::set_clipboard(&t, true));
                        println!("[clip] PC -> device ({} chars, pasted)", t.len());
                    }
                }
            }
            if let Ok(mut g) = clip_in.lock() {
                if let Some(t) = g.take() {
                    let _ = clipboard_win::set_clipboard_string(&t);
                    println!("[clip] device -> PC ({} chars)", t.len());
                }
            }
            // Keyboard -> device (typed text + special keys).
            if let Some(ctl) = &mut control {
                let text = mirror.drain_text();
                if !text.is_empty() {
                    control::send(ctl, &control::inject_text(&text));
                }
                for kc in mirror.drain_keys() {
                    control::send(ctl, &control::inject_keycode(control::KEY_DOWN, kc, 0, 0));
                    control::send(ctl, &control::inject_keycode(control::KEY_UP, kc, 0, 0));
                }
            }
            // Drain captured PCM into the recorder (dropped when not recording).
            {
                let pcm = rec_audio
                    .lock()
                    .ok()
                    .map(|mut b| std::mem::take(&mut *b))
                    .unwrap_or_default();
                if !pcm.is_empty() {
                    if let Some(r) = &mut recorder {
                        r.feed_audio(&pcm);
                    }
                }
            }
            // Mouse -> device touch/scroll injection.
            if let Some(ctl) = &mut control {
                for e in mirror.drain_input() {
                    match e.kind {
                        1 => {
                            if let Some((x, y, w, h)) = mirror.map_client_to_video(e.x, e.y) {
                                mouse_down = true;
                                control::send(ctl, &control::touch(control::ACTION_DOWN, x, y, w, h, true));
                            }
                        }
                        2 => {
                            if let Some((x, y, w, h)) = mirror.map_client_to_video(e.x, e.y) {
                                control::send(ctl, &control::touch(control::ACTION_UP, x, y, w, h, false));
                            }
                            mouse_down = false;
                        }
                        0 if mouse_down => {
                            if let Some((x, y, w, h)) = mirror.map_client_to_video(e.x, e.y) {
                                control::send(ctl, &control::touch(control::ACTION_MOVE, x, y, w, h, true));
                            }
                        }
                        3 => {
                            if let Some((x, y, w, h)) = mirror.map_client_to_video(e.x, e.y) {
                                let v = if e.wheel > 0 { 1.0 } else { -1.0 };
                                control::send(ctl, &control::scroll(x, y, w, h, v));
                            }
                        }
                        _ => {}
                    }
                }
            }
            let mut latest = None;
            loop {
                match rx.try_recv() {
                    Ok(au) => {
                        // Maintain the rolling GOP (reset on config/keyframe).
                        if au.is_config {
                            gop_config = Some(au.data.clone());
                            gop.clear();
                        } else {
                            if au.is_key {
                                gop.clear();
                            }
                            if gop.len() < 1200 {
                                gop.push((au.data.clone(), au.pts_100ns / 10, au.is_key));
                            }
                        }
                        if let Some(r) = &mut recorder {
                            r.feed(&au.data, au.pts_100ns / 10, au.is_key, au.is_config);
                        }
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
        if let Some(h) = audio_net {
            let _ = h.join();
        }
        if let Some(h) = clip_reader {
            let _ = h.join();
        }

        if user_quit {
            break 'session;
        }
        println!("[transport] stream ended — reconnecting");
        if pump_for(&mut mirror, backoff) {
            break 'session;
        }
        backoff = (backoff * 2).min(cap);
    }

    if let Some(r) = recorder.take() {
        if let Some((p, n)) = r.finish() {
            println!("[rec] saved {p} ({n} frames)");
        }
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
    let mut dec = Decoder::new(mirror.device(), "h264")?;
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
        dec = Decoder::new(mirror.device(), "h264")?;
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

fn rec_path() -> String {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    let dir = format!("{base}\\Videos\\PrismDesk");
    let _ = std::fs::create_dir_all(&dir);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{dir}\\prismdesk-{ms}.mp4")
}

fn screenshot_path() -> String {
    let base = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
    let dir = format!("{base}\\Pictures\\PrismDesk");
    let _ = std::fs::create_dir_all(&dir);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{dir}\\prismdesk-{ms}.png")
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
