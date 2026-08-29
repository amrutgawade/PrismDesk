//! PrismDesk engine — Phase 1 core mirror.
//!
//! Milestone 1a: open the dedicated native mirror window on the NVIDIA GPU and
//! run a flip-model present loop. Presents an animated "prism" clear color so we
//! can confirm the window + swapchain + present path visually. The decode ->
//! NV12 -> shader render replaces the clear in milestones 1b/1c.
//!
//! Run:  cargo run -p pd-engine            (auto-closes after ~8s)
//!       cargo run -p pd-engine -- hold     (stays open until you close it)

use std::time::{Duration, Instant};

use pd_render::Mirror;

fn main() -> windows_core::Result<()> {
    let hold = std::env::args().nth(1).as_deref() == Some("hold");

    let mut mirror = Mirror::new(1280, 720, "PrismDesk — Mirror (Phase 1)")?;
    println!("PrismDesk engine · milestone 1a");
    println!("  mirror window created on GPU: {}", mirror.adapter_name());
    println!("  flip-model swapchain presenting{}", if hold { " (close the window to exit)" } else { " (~8s)" });

    let start = Instant::now();
    let mut frames = 0u64;
    loop {
        if mirror.pump() {
            break;
        }

        // Animated prism gradient (cyan -> violet), just to prove present works.
        let t = start.elapsed().as_secs_f32();
        let a = (t * 0.7).sin() * 0.5 + 0.5;
        let clear = [
            0.03 + 0.10 * a,        // r
            0.06 + 0.16 * (1.0 - a), // g
            0.10 + 0.34 * a,        // b
            1.0,
        ];
        mirror.present(clear)?;
        frames += 1;

        if !hold && start.elapsed() > Duration::from_secs(8) {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }

    let secs = start.elapsed().as_secs_f32().max(0.001);
    println!(
        "[ok] presented {frames} frames in {:.1}s ({:.0} fps). Window closed cleanly.",
        secs,
        frames as f32 / secs
    );
    Ok(())
}
