//! PrismDesk render layer — the dedicated native mirror window.
//!
//! A real top-level Win32 HWND owning a DXGI flip-model swapchain on the NVIDIA
//! GPU. This is the OBS-capturable surface (WGC). Milestone 1a presents a solid
//! clear color; NV12->RGB video rendering lands in milestone 1c.

use windows::core::{w, Result};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
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
    DXGI_ADAPTER_DESC1, DXGI_PRESENT, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, LoadCursorW,
    PeekMessageW, PostQuitMessage, RegisterClassExW, ShowWindow, TranslateMessage, CS_HREDRAW,
    CS_VREDRAW, CW_USEDEFAULT, IDC_ARROW, MSG, PM_REMOVE, SW_SHOW, WINDOW_EX_STYLE, WM_CLOSE,
    WM_DESTROY, WM_QUIT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

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
}

impl Mirror {
    /// Create the window and swapchain. `size` is the initial client size.
    pub fn new(width: u32, height: u32, title: &str) -> Result<Self> {
        let hwnd = unsafe { create_window(width, height, title)? };
        let (device, context, adapter, adapter_name) = unsafe { create_device_on_nvidia()? };
        let swapchain = unsafe { create_swapchain(&device, &adapter, hwnd)? };

        let mut me = Self {
            hwnd,
            device,
            context,
            swapchain,
            rtv: None,
            size: (width.max(1), height.max(1)),
            adapter_name,
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

    /// Pump queued window messages. Returns true when the window is closing.
    pub fn pump(&self) -> bool {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return true;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        false
    }

    /// Clear the back buffer and present. (Milestone 1c replaces the clear with
    /// an NV12->RGB draw.)
    pub fn present(&mut self, clear: [f32; 4]) -> Result<()> {
        unsafe {
            self.maybe_resize()?;
            if let Some(rtv) = &self.rtv {
                self.context.ClearRenderTargetView(rtv, &clear);
            }
            // Vsync present for milestone 1a; waitable + tearing/VRR arrive in 1e.
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
            .ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))?;
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
/// spacedesk). Returns (device, context, adapter, adapter name).
unsafe fn create_device_on_nvidia(
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
        Flags: 0,
    };
    factory.CreateSwapChainForHwnd(device, hwnd, &desc, None, None)
}

fn wchars_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}
