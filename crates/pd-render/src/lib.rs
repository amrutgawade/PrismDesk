//! PrismDesk render layer — the dedicated native mirror window.
//!
//! A real top-level Win32 HWND owning a DXGI flip-model swapchain on the NVIDIA
//! GPU. This is the OBS-capturable surface (WGC). Milestone 1a presents a solid
//! clear color; NV12->RGB video rendering lands in milestone 1c.

use windows::core::{w, Interface, Result};
use windows::Win32::Foundation::{BOOL, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView, ID3D11Texture2D,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIFactory2, IDXGISwapChain1,
    IDXGISwapChain2, DXGI_ADAPTER_DESC1, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::Threading::WaitForSingleObjectEx;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, LoadCursorW, PeekMessageW, PostQuitMessage, RegisterClassExW,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    CW_USEDEFAULT, GWL_STYLE, HWND_TOP, IDC_ARROW, MSG, PM_REMOVE, SWP_FRAMECHANGED,
    SWP_NOOWNERZORDER, SW_SHOW, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW, WS_POPUP,
    WS_VISIBLE,
};

const VK_F11: usize = 0x7A;
const VK_M: usize = 0x4D;
const VK_S: usize = 0x53;
const VK_R: usize = 0x52;
const VK_V: usize = 0x56;

/// Set by the window proc on F11; consumed by `pump` to toggle fullscreen.
static TOGGLE_FS: AtomicBool = AtomicBool::new(false);
/// Set by the window proc on M; consumed by the engine to toggle audio mute.
static TOGGLE_MUTE: AtomicBool = AtomicBool::new(false);
/// Set by the window proc on S; consumed by the engine to take a screenshot.
static TOGGLE_SHOT: AtomicBool = AtomicBool::new(false);
/// Set by the window proc on R; consumed by the engine to toggle recording.
static TOGGLE_REC: AtomicBool = AtomicBool::new(false);
/// Set by the window proc on V; consumed by the engine to paste PC clipboard.
static TOGGLE_PASTE: AtomicBool = AtomicBool::new(false);

/// A raw mouse event in window client pixels. kind: 0=move 1=down 2=up 3=wheel.
#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub kind: u8,
    pub x: i32,
    pub y: i32,
    pub wheel: i32,
}
/// Mouse events queued by the window proc, drained by the engine each frame.
static INPUT: Mutex<Vec<MouseEvent>> = Mutex::new(Vec::new());
/// Last mouse client position (for wheel events, whose coords are screen-space).
static LAST_MOUSE: Mutex<(i32, i32)> = Mutex::new((0, 0));

mod video;
use video::Video;

const VENDOR_NVIDIA: u32 = 0x10DE;

/// The dedicated mirror window + its D3D11 device and flip-model swapchain.
pub struct Mirror {
    hwnd: HWND,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    swapchain: IDXGISwapChain1,
    rtv: Option<ID3D11RenderTargetView>,
    size: (u32, u32),
    adapter_name: String,
    waitable: HANDLE,
    video: Option<Video>,
    fullscreen: bool,
    saved: Option<(isize, RECT)>,
    vid_size: (u32, u32),
}

impl Mirror {
    /// Create the window and swapchain. `size` is the initial client size.
    pub fn new(width: u32, height: u32, title: &str) -> Result<Self> {
        // Per-Monitor-V2 DPI awareness: back-buffer sized in physical pixels, so
        // Windows never bitmap-stretches (blurs) the window under >100% scaling.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let hwnd = unsafe { create_window(width, height, title)? };
        let (device, context, adapter, adapter_name) = unsafe { nvidia_device()? };
        let swapchain = unsafe { create_swapchain(&device, &adapter, hwnd)? };
        // Frame-latency-1 waitable swapchain: block on this before rendering so
        // the CPU can't queue ahead — minimum present latency while staying vsync.
        let waitable = unsafe {
            let sc2: IDXGISwapChain2 = swapchain.cast()?;
            sc2.SetMaximumFrameLatency(1)?;
            sc2.GetFrameLatencyWaitableObject()
        };

        let mut me = Self {
            hwnd,
            device,
            context,
            swapchain,
            rtv: None,
            size: (width.max(1), height.max(1)),
            adapter_name,
            waitable,
            video: None,
            fullscreen: false,
            saved: None,
            vid_size: (0, 0),
        };
        unsafe { me.create_rtv()? };
        unsafe { ShowWindow(hwnd, SW_SHOW).ok().ok() };
        Ok(me)
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// True once if the user pressed M since the last check (audio mute toggle).
    pub fn mute_toggled(&self) -> bool {
        TOGGLE_MUTE.swap(false, Ordering::Relaxed)
    }

    /// True once if the user pressed S since the last check (screenshot).
    pub fn shot_requested(&self) -> bool {
        TOGGLE_SHOT.swap(false, Ordering::Relaxed)
    }

    /// True once if the user pressed R since the last check (toggle recording).
    pub fn rec_toggled(&self) -> bool {
        TOGGLE_REC.swap(false, Ordering::Relaxed)
    }

    /// True once if the user pressed V since the last check (paste PC clipboard).
    pub fn paste_requested(&self) -> bool {
        TOGGLE_PASTE.swap(false, Ordering::Relaxed)
    }

    /// Drain queued mouse events (raw window client coordinates).
    pub fn drain_input(&self) -> Vec<MouseEvent> {
        INPUT.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
    }

    /// Map a window client point to device coordinates within the video content
    /// (accounts for letterbox). Returns (x, y, frame_w, frame_h) or None if the
    /// point is outside the video area.
    pub fn map_client_to_video(&self, mx: i32, my: i32) -> Option<(u32, u32, u32, u32)> {
        let (vw, vh) = self.vid_size;
        if vw == 0 || vh == 0 {
            return None;
        }
        let (ww, wh) = (self.size.0 as f32, self.size.1 as f32);
        let (vwf, vhf) = (vw as f32, vh as f32);
        let scale = (ww / vwf).min(wh / vhf);
        let (dw, dh) = (vwf * scale, vhf * scale);
        let ox = (ww - dw) * 0.5;
        let oy = (wh - dh) * 0.5;
        let cx = mx as f32 - ox;
        let cy = my as f32 - oy;
        if cx < 0.0 || cy < 0.0 || cx >= dw || cy >= dh {
            return None;
        }
        let vx = (cx / scale).clamp(0.0, vwf - 1.0) as u32;
        let vy = (cy / scale).clamp(0.0, vhf - 1.0) as u32;
        Some((vx, vy, vw, vh))
    }

    /// Save the current frame at native video resolution to a PNG. Returns false
    /// if no frame has been rendered yet.
    pub fn screenshot(&mut self, path: &str) -> Result<bool> {
        let device = self.device.clone();
        let ctx = self.context.clone();
        let shot = match &self.video {
            Some(v) => unsafe { v.render_offscreen(&device, &ctx)? },
            None => None,
        };
        match shot {
            Some((w, h, bgra)) => {
                write_png_bgra(path, w, h, &bgra)
                    .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The D3D11 device this window renders with. The decoder must share it so
    /// decoded NV12 textures can be sampled without leaving the GPU.
    pub fn device(&self) -> &ID3D11Device {
        &self.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.context
    }

    /// Current decoded video frame size (0,0 before the first frame).
    pub fn video_size(&self) -> (u32, u32) {
        self.vid_size
    }

    /// Pump queued window messages. Returns true when the window is closing.
    pub fn pump(&mut self) -> bool {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return true;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if TOGGLE_FS.swap(false, Ordering::Relaxed) {
                self.toggle_fullscreen();
            }
        }
        false
    }

    /// Toggle borderless fullscreen (F11). Flip-model borderless keeps DWM
    /// composition + OBS WGC working, unlike exclusive fullscreen.
    unsafe fn toggle_fullscreen(&mut self) {
        if !self.fullscreen {
            let style = GetWindowLongPtrW(self.hwnd, GWL_STYLE);
            let mut rect = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut rect);
            let mon = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                self.saved = Some((style, rect));
                SetWindowLongPtrW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as isize);
                let rc = mi.rcMonitor;
                let _ = SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    rc.left,
                    rc.top,
                    rc.right - rc.left,
                    rc.bottom - rc.top,
                    SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
                );
                self.fullscreen = true;
            }
        } else if let Some((style, rect)) = self.saved.take() {
            SetWindowLongPtrW(self.hwnd, GWL_STYLE, style);
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
            );
            self.fullscreen = false;
        }
    }

    /// Clear the back buffer and present. (Milestone 1c replaces the clear with
    /// an NV12->RGB draw.)
    pub fn present(&mut self, clear: [f32; 4]) -> Result<()> {
        unsafe {
            WaitForSingleObjectEx(self.waitable, 1000, BOOL(0));
            self.maybe_resize()?;
            if let Some(rtv) = &self.rtv {
                self.context.ClearRenderTargetView(rtv, &clear);
            }
            // Vsync present for milestone 1a; waitable + tearing/VRR arrive in 1e.
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    /// Render one decoded NV12 frame (letterboxed) and present. `src` is the
    /// decoder's NV12 texture, `src_sub` its array slice, `(vw,vh)` its size.
    pub fn render_frame(
        &mut self,
        src: &ID3D11Texture2D,
        src_sub: u32,
        vw: u32,
        vh: u32,
    ) -> Result<()> {
        self.vid_size = (vw, vh);
        unsafe {
            WaitForSingleObjectEx(self.waitable, 1000, BOOL(0));
            self.maybe_resize()?;
            let device = self.device.clone();
            let context = self.context.clone();
            let rtv = match &self.rtv {
                Some(r) => r.clone(),
                None => return Ok(()),
            };
            if self.video.is_none() {
                self.video = Some(Video::new(&device)?);
            }
            let size = self.size;
            self.video
                .as_mut()
                .unwrap()
                .draw(&device, &context, &rtv, size, src, src_sub, (vw, vh))?;
            self.swapchain.Present(1, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }

    unsafe fn create_rtv(&mut self) -> Result<()> {
        let backbuffer: ID3D11Texture2D = self.swapchain.GetBuffer(0)?;
        let mut rtv: Option<ID3D11RenderTargetView> = None;
        self.device
            .CreateRenderTargetView(&backbuffer, None, Some(&mut rtv))?;
        self.rtv = rtv;
        Ok(())
    }

    unsafe fn maybe_resize(&mut self) -> Result<()> {
        let mut rect = RECT::default();
        GetClientRect(self.hwnd, &mut rect)?;
        let w = (rect.right - rect.left).max(1) as u32;
        let h = (rect.bottom - rect.top).max(1) as u32;
        if (w, h) == self.size {
            return Ok(());
        }
        // Flip-model: release all back-buffer references before ResizeBuffers.
        self.rtv = None;
        self.context.ClearState();
        self.swapchain
            .ResizeBuffers(
                0,
                w,
                h,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT,
            )?;
        self.size = (w, h);
        self.create_rtv()?;
        Ok(())
    }
}

impl Drop for Mirror {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wparam.0 == VK_F11 {
                    TOGGLE_FS.store(true, Ordering::Relaxed);
                } else if wparam.0 == VK_M {
                    TOGGLE_MUTE.store(true, Ordering::Relaxed);
                } else if wparam.0 == VK_S {
                    TOGGLE_SHOT.store(true, Ordering::Relaxed);
                } else if wparam.0 == VK_R {
                    TOGGLE_REC.store(true, Ordering::Relaxed);
                } else if wparam.0 == VK_V {
                    TOGGLE_PASTE.store(true, Ordering::Relaxed);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                if let Ok(mut p) = LAST_MOUSE.lock() {
                    *p = (x, y);
                }
                let kind = match msg {
                    WM_LBUTTONDOWN => 1,
                    WM_LBUTTONUP => 2,
                    _ => 0,
                };
                push_input(MouseEvent { kind, x, y, wheel: 0 });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                // Wheel coords are screen-space; reuse the last client position.
                let (x, y) = LAST_MOUSE.lock().map(|p| *p).unwrap_or((0, 0));
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                push_input(MouseEvent { kind: 3, x, y, wheel: delta });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn create_window(width: u32, height: u32, title: &str) -> Result<HWND> {
    let instance = GetModuleHandleW(None)?;
    let class_name = w!("PrismDeskMirror");

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: HINSTANCE(instance.0),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassExW(&wc); // idempotent per class name; ignore "already registered"

    let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class_name,
        windows::core::PCWSTR(title_w.as_ptr()),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        width as i32,
        height as i32,
        None,
        None,
        HINSTANCE(instance.0),
        None,
    )?;
    Ok(hwnd)
}

/// Create a D3D11 device pinned to the NVIDIA discrete GPU (not the AMD iGPU /
/// spacedesk). Returns (device, context, adapter, adapter name). Public so the
/// decoder and other subsystems can share one NVIDIA device.
pub unsafe fn nvidia_device(
) -> Result<(ID3D11Device, ID3D11DeviceContext, IDXGIAdapter1, String)> {
    let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
    let mut chosen: Option<(IDXGIAdapter1, String)> = None;
    let mut fallback: Option<(IDXGIAdapter1, String)> = None;
    let mut i = 0u32;
    while let Ok(adapter) = factory.EnumAdapters1(i) {
        let desc: DXGI_ADAPTER_DESC1 = adapter.GetDesc1()?;
        let name = wchars_to_string(&desc.Description);
        if desc.VendorId == VENDOR_NVIDIA && chosen.is_none() {
            chosen = Some((adapter, name));
        } else if fallback.is_none() && desc.DedicatedVideoMemory > 0 {
            fallback = Some((adapter, name));
        }
        i += 1;
    }
    let (adapter, name) = chosen
        .or(fallback)
        .expect("no suitable GPU adapter found");

    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut fl = D3D_FEATURE_LEVEL::default();
    D3D11CreateDevice(
        &adapter,
        D3D_DRIVER_TYPE_UNKNOWN,
        windows::Win32::Foundation::HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
        Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&mut device),
        Some(&mut fl),
        Some(&mut context),
    )?;
    Ok((device.unwrap(), context.unwrap(), adapter, name))
}

unsafe fn create_swapchain(
    device: &ID3D11Device,
    adapter: &IDXGIAdapter1,
    hwnd: HWND,
) -> Result<IDXGISwapChain1> {
    let factory: IDXGIFactory2 = adapter.GetParent()?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: 0, // 0 => use the HWND client size
        Height: 0,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT.0 as u32,
    };
    factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None)
}

fn wchars_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

fn push_input(e: MouseEvent) {
    if let Ok(mut q) = INPUT.lock() {
        if q.len() < 4096 {
            q.push(e);
        }
    }
}

fn write_png_bgra(path: &str, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut rgba = vec![0u8; bgra.len()];
    for (i, px) in bgra.chunks_exact(4).enumerate() {
        let o = i * 4;
        rgba[o] = px[2]; // R <- B
        rgba[o + 1] = px[1]; // G
        rgba[o + 2] = px[0]; // B <- R
        rgba[o + 3] = 255; // A (opaque)
    }
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc
        .write_header()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    wr.write_image_data(&rgba)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    Ok(())
}
