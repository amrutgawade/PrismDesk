//! Phase-1b test: decode the canned scrcpy capture to NV12 via NVDEC and report
//! how many frames came out. Proves the Media Foundation decode path end-to-end
//! before we wire it to the renderer.
//!
//! Run:  cargo run -p pd-decode --example decode_file
//!       cargo run -p pd-decode --example decode_file -- path\to\file.h264

use pd_decode::Decoder;

fn main() -> windows_core::Result<()> {
    let path = std::env::args().nth(1).unwrap_or_else(find_capture);
    let data = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));

    let (device, _ctx, _adapter, name) = unsafe { pd_render::nvidia_device()? };
    println!("decoding {path} ({} bytes) on {name}", data.len());

    let mut dec = Decoder::new(&device)?;
    let nals = split_nals(&data);

    let mut out = Vec::new();
    let mut frames = 0usize;
    let mut first: Option<(u32, u32)> = None;
    for (i, nal) in nals.iter().enumerate() {
        dec.decode(nal, (i as i64) * 166_667, &mut out)?;
        for f in out.drain(..) {
            if first.is_none() {
                first = Some((f.width, f.height));
            }
            frames += 1;
        }
    }
    dec.finish(&mut out)?;
    for f in out.drain(..) {
        if first.is_none() {
            first = Some((f.width, f.height));
        }
        frames += 1;
    }

    println!(
        "NALs fed: {} · frames decoded: {frames} · first frame: {:?} · negotiated size: {:?}",
        nals.len(),
        first,
        dec.frame_size()
    );
    if frames > 0 {
        println!("[PASS 1b] H.264 -> NV12 hardware decode works. Ready to render (1c).");
        Ok(())
    } else {
        eprintln!("[FAIL 1b] no frames decoded");
        std::process::exit(1);
    }
}

fn find_capture() -> String {
    let dir = std::path::Path::new("captures");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) == Some("h264") {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    panic!("no captures/*.h264 found — run Spike B first (cargo run -p scrcpy-dump)");
}

/// Split an Annex-B stream into NAL units (each slice keeps its start code).
fn split_nals(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push(s);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for k in 0..starts.len() {
        let end = if k + 1 < starts.len() {
            starts[k + 1]
        } else {
            data.len()
        };
        out.push(&data[starts[k]..end]);
    }
    out
}
