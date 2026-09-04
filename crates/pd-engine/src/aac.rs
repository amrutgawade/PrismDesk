//! Media Foundation AAC-LC encoder: 48 kHz stereo s16 PCM -> raw AAC frames
//! (no ADTS), for muxing an audio track into the MP4 recording.

use std::ffi::c_void;
use std::mem::ManuallyDrop;

use windows::core::Result;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;

const MF_VERSION: u32 = 0x0002_0070;
const MFSTARTUP_LITE: u32 = 0x1;

pub struct AacEncoder {
    transform: IMFTransform,
    out_size: usize,
    time_100ns: i64,
}

impl AacEncoder {
    /// 48 kHz stereo, `bitrate` bits/sec (e.g. 128000).
    pub fn new(bitrate: u32) -> Result<AacEncoder> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_LITE).ok();

            let transform = instantiate_aac_encoder()?;

            // Encoders: set OUTPUT type first.
            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            out.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48000)?;
            out.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            out.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, bitrate / 8)?;
            out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?; // raw AAC (no ADTS)
            transform.SetOutputType(0, &out, 0)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            inp.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48000)?;
            inp.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
            inp.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            inp.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
            inp.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192000)?;
            transform.SetInputType(0, &inp, 0)?;

            let info = transform.GetOutputStreamInfo(0)?;
            let out_size = info.cbSize.max(8192) as usize;

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            Ok(AacEncoder { transform, out_size, time_100ns: 0 })
        }
    }

    /// Encode PCM, appending any completed raw AAC frames to `out`.
    pub fn encode(&mut self, pcm: &[u8], out: &mut Vec<Vec<u8>>) {
        unsafe {
            let dur = (pcm.len() as i64 / 4) * 10_000_000 / 48000; // stereo s16 frames
            let sample = match make_pcm_sample(pcm, self.time_100ns, dur) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[aac] make_sample: {e:?}");
                    return;
                }
            };
            self.time_100ns += dur;
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    self.drain(out);
                    let _ = self.transform.ProcessInput(0, &sample, 0);
                }
                Err(e) => {
                    eprintln!("[aac] ProcessInput: {e:?}");
                    return;
                }
            }
            self.drain(out);
        }
    }

    unsafe fn drain(&mut self, out: &mut Vec<Vec<u8>>) {
        loop {
            let sample = match MFCreateSample() {
                Ok(s) => s,
                Err(_) => return,
            };
            let buffer = match MFCreateMemoryBuffer(self.out_size as u32) {
                Ok(b) => b,
                Err(_) => return,
            };
            if sample.AddBuffer(&buffer).is_err() {
                return;
            }
            let mut bufs = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(Some(sample)),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            match self.transform.ProcessOutput(0, &mut bufs, &mut status) {
                Ok(()) => {
                    let s = ManuallyDrop::take(&mut bufs[0].pSample);
                    let _ = ManuallyDrop::take(&mut bufs[0].pEvents);
                    if let Some(s) = s {
                        if let Some(bytes) = read_sample(&s) {
                            if !bytes.is_empty() {
                                out.push(bytes);
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = ManuallyDrop::take(&mut bufs[0].pSample);
                    let _ = ManuallyDrop::take(&mut bufs[0].pEvents);
                    if e.code() != MF_E_TRANSFORM_NEED_MORE_INPUT {
                        eprintln!("[aac] ProcessOutput: {e:?}");
                    }
                    return; // NEED_MORE_INPUT or done
                }
            }
        }
    }
}

fn instantiate_aac_encoder() -> Result<IMFTransform> {
    unsafe {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Audio,
            guidSubtype: MFAudioFormat_PCM,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Audio,
            guidSubtype: MFAudioFormat_AAC,
        };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnumEx(
            MFT_CATEGORY_AUDIO_ENCODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )?;
        if count == 0 || activates.is_null() {
            return Err(windows::core::Error::from(MF_E_TOPO_CODEC_NOT_FOUND));
        }
        let activate = std::slice::from_raw_parts(activates, count as usize)[0]
            .clone()
            .expect("null activate");
        let t: IMFTransform = activate.ActivateObject()?;
        CoTaskMemFree(Some(activates as *const c_void));
        Ok(t)
    }
}

unsafe fn make_pcm_sample(pcm: &[u8], time_100ns: i64, dur_100ns: i64) -> Result<IMFSample> {
    let sample = MFCreateSample()?;
    let buffer = MFCreateMemoryBuffer(pcm.len() as u32)?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    buffer.Lock(&mut ptr, None, None)?;
    std::ptr::copy_nonoverlapping(pcm.as_ptr(), ptr, pcm.len());
    buffer.Unlock()?;
    buffer.SetCurrentLength(pcm.len() as u32)?;
    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(time_100ns)?;
    sample.SetSampleDuration(dur_100ns)?;
    Ok(sample)
}

unsafe fn read_sample(sample: &IMFSample) -> Option<Vec<u8>> {
    let buffer = sample.ConvertToContiguousBuffer().ok()?;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut len = 0u32;
    buffer.Lock(&mut ptr, None, Some(&mut len)).ok()?;
    let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
    let _ = buffer.Unlock();
    Some(bytes)
}
