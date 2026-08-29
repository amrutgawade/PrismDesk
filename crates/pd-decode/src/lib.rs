//! PrismDesk decode layer — hardware H.264 -> NV12 via Media Foundation/NVDEC.
//!
//! Wraps the built-in Microsoft H.264 Video Decoder MFT, bound to a shared
//! ID3D11Device through an IMFDXGIDeviceManager so decoded frames are NV12
//! D3D11 textures that never leave the GPU. Configured for low latency.
//!
//! The MS decoder is a synchronous MFT even in D3D11 mode, so the drive loop is
//! the classic ProcessInput / ProcessOutput drain with STREAM_CHANGE handling —
//! no async-MFT event plumbing needed.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::Once;

use windows::core::{Interface, Result};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

const MF_VERSION: u32 = 0x0002_0070;
const MFSTARTUP_LITE: u32 = 0x1;

static MF_INIT: Once = Once::new();

/// A decoded frame: an NV12 texture (owned by the decoder's D3D11 output pool)
/// plus the array slice to sample. Hold at most a couple at once — retaining
/// many starves the decoder's output pool.
pub struct DecodedFrame {
    pub texture: ID3D11Texture2D,
    pub subresource: u32,
    pub width: u32,
    pub height: u32,
}

pub struct Decoder {
    transform: IMFTransform,
    _mgr: IMFDXGIDeviceManager,
    output_set: bool,
    width: u32,
    height: u32,
}

impl Decoder {
    /// Create a decoder that decodes onto `device` (share the renderer's device).
    pub fn new(device: &ID3D11Device) -> Result<Self> {
        unsafe {
            MF_INIT.call_once(|| {
                let _ = MFStartup(MF_VERSION, MFSTARTUP_LITE);
            });

            let mut token = 0u32;
            let mut mgr: Option<IMFDXGIDeviceManager> = None;
            MFCreateDXGIDeviceManager(&mut token, &mut mgr)?;
            let mgr = mgr.expect("null device manager");
            mgr.ResetDevice(device, token)?;

            let transform = instantiate_h264_decoder()?;

            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            transform.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, mgr.as_raw() as usize)?;

            let intype = MFCreateMediaType()?;
            intype.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            intype.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &intype, 0)?;

            let mut me = Self {
                transform,
                _mgr: mgr,
                output_set: false,
                width: 0,
                height: 0,
            };
            // The MS H.264 decoder offers NV12 output right after the input type
            // is set; it must be selected before ProcessInput. The real frame
            // size is re-read from the STREAM_CHANGE once the SPS is parsed.
            me.set_output_type()?;
            me.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            me.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(me)
        }
    }

    pub fn frame_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Feed one Annex-B access unit (or NAL) and drain any ready frames into `out`.
    pub fn decode(&mut self, data: &[u8], pts_100ns: i64, out: &mut Vec<DecodedFrame>) -> Result<()> {
        unsafe {
            let sample = make_input_sample(data, pts_100ns)?;
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    self.drain(out)?;
                    self.transform.ProcessInput(0, &sample, 0)?;
                }
                Err(e) => return Err(e),
            }
            self.drain(out)?;
        }
        Ok(())
    }

    /// Flush the decoder at end of stream (drains remaining frames).
    pub fn finish(&mut self, out: &mut Vec<DecodedFrame>) -> Result<()> {
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
            self.drain(out)?;
        }
        Ok(())
    }

    unsafe fn drain(&mut self, out: &mut Vec<DecodedFrame>) -> Result<()> {
        loop {
            let mut bufs = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            match self.transform.ProcessOutput(0, &mut bufs, &mut status) {
                Ok(()) => {
                    let sample = ManuallyDrop::take(&mut bufs[0].pSample);
                    let _events = ManuallyDrop::take(&mut bufs[0].pEvents);
                    if let Some(s) = sample {
                        if let Some(frame) = extract_frame(&s)? {
                            out.push(frame);
                        }
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.set_output_type()?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    unsafe fn set_output_type(&mut self) -> Result<()> {
        let mut i = 0u32;
        while let Ok(t) = self.transform.GetOutputAvailableType(0, i) {
            if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                self.transform.SetOutputType(0, &t, 0)?;
                if let Ok(fs) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                    self.width = (fs >> 32) as u32;
                    self.height = (fs & 0xffff_ffff) as u32;
                }
                self.output_set = true;
                return Ok(());
            }
            i += 1;
        }
        Ok(())
    }
}

fn instantiate_h264_decoder() -> Result<IMFTransform> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_HARDWARE
                | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )?;
        if count == 0 || activates.is_null() {
            return Err(windows::core::Error::from(MF_E_TOPO_CODEC_NOT_FOUND));
        }
        let list = std::slice::from_raw_parts(activates, count as usize);
        let activate = list[0].clone().expect("null activate");
        let transform: IMFTransform = activate.ActivateObject()?;
        CoTaskMemFree(Some(activates as *const c_void));
        Ok(transform)
    }
}

unsafe fn make_input_sample(data: &[u8], pts_100ns: i64) -> Result<IMFSample> {
    let sample = MFCreateSample()?;
    let buffer = MFCreateMemoryBuffer(data.len() as u32)?;

    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut max_len = 0u32;
    buffer.Lock(&mut ptr, Some(&mut max_len), None)?;
    std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
    buffer.Unlock()?;
    buffer.SetCurrentLength(data.len() as u32)?;

    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(pts_100ns)?;
    Ok(sample)
}

unsafe fn extract_frame(sample: &IMFSample) -> Result<Option<DecodedFrame>> {
    let buffer = sample.GetBufferByIndex(0)?;
    let dxgi: IMFDXGIBuffer = buffer.cast()?;

    let mut ppv: *mut c_void = std::ptr::null_mut();
    dxgi.GetResource(&ID3D11Texture2D::IID, &mut ppv)?;
    let texture = ID3D11Texture2D::from_raw(ppv);

    let subresource = dxgi.GetSubresourceIndex()?;

    let mut desc = D3D11_TEXTURE2D_DESC::default();
    texture.GetDesc(&mut desc);

    Ok(Some(DecodedFrame {
        texture,
        subresource,
        width: desc.Width,
        height: desc.Height,
    }))
}
