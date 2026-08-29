# PrismDesk

Premium Windows app for smooth, lowest-latency Android screen mirroring &
control. Latency and smoothness dominate every other goal.

**Architecture blueprint:** https://claude.ai/code/artifact/f096fde7-d388-4aef-974d-e9722ced5445

## Resolved stack (the spine)

Rust + `windows-rs` engine → speaks the scrcpy protocol to a **pinned fork** of
its Apache-2.0 device server → hardware decode via **Media Foundation → NVDEC
(no FFmpeg)** → one GPU copy → NV12→RGB shader → **flip-model D3D11 swapchain**
on a real HWND (OBS captures via **WGC**) → **VRR-aware** present. Dashboard =
**WebView2** on the **MSVC** toolchain. Audio = Opus → `unsafe-libopus` →
WASAPI. Recording = **MKV** passthrough. Codec: H.264 default, HEVC opt-in,
never AV1 (GTX 1650 has no AV1 decode).

## Toolchain

Phase 0 builds on the **self-contained GNU toolchain** already installed
(bundled linker — no VS Build Tools needed). `windows-rs` reaches Media
Foundation / D3D11 / DXGI / WASAPI with zero C build. We migrate to
`x86_64-pc-windows-msvc` (VS Build Tools 2022 + Win11 SDK) before **Phase 5**
(the WebView2 dashboard), which needs the MSVC linker.

## Status — Phase 0 (de-risk) ✅ COMPLETE

| Spike | What it proves | Status |
|---|---|---|
| **A — `mf-smoke`** | windows-rs MF+D3D11 links on GNU · GTX 1650 pinned · MS H.264 MFT accepts our D3D11 device manager → DXVA/NVDEC decode is real | ✅ **PASS** |
| **B — `scrcpy-dump`** | scrcpy 3.3.1 pushed via adb → real H.264 stream reaches the PC over a USB **`adb reverse`** tunnel (0.94 MB/5s, valid SPS) | ✅ **PASS** |

**USB transport must use `adb reverse`, not forward** — a forward tunnel with the
dummy byte disabled races and delivers 0 bytes; reverse (device dials out) is
clean. Test device: POCO `2311DRK48I`, Android 16, `c2.mtk.avc.encoder`.
A sample capture is saved under `captures/` as a Phase-1 decode fixture.

```bash
# Spike A — no device required:
cargo run -p mf-smoke

# Spike B — connect an Android phone (USB debugging on) first:
cargo run -p scrcpy-dump            # captures ~5s to captures/*.h264
```

## Layout (Phase 0)

```
spikes/mf-smoke      Media Foundation + D3D11 smoke test
spikes/scrcpy-dump   scrcpy socket -> raw H.264 dump
assets/server        pinned scrcpy 3.3.1 server (SHA-256 recorded)
```

The full crate layout (`pd-app`, `pd-engine`, `pd-decode`, `pd-render`,
`pd-audio`, `pd-control`, `pd-record`, `pd-adb`, `pd-proto`, `pd-metrics`)
lands in Phase 1.
