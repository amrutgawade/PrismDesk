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

fn main() -> windows_core::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("blank") => blank_demo(),
        _ => play_capture(),
    }
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
        if start.elapsed() > Duration::from_secs(12) {
            break;
        }
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
