//! MP4 passthrough recording: mux the incoming H.264 Annex-B access units into
//! an MP4 (no re-encode) via the pure-Rust `mp4` crate. Video-only for v1.
//! Buffer-one-ahead so each sample gets a correct VFR duration.

use std::fs::File;
use std::io::BufWriter;

use bytes::Bytes;
use mp4::{AvcConfig, FourCC, MediaConfig, Mp4Config, Mp4Sample, Mp4Writer, TrackConfig, TrackType};

const TIMESCALE: u32 = 1000; // milliseconds

pub struct Recorder {
    writer: Option<Mp4Writer<BufWriter<File>>>,
    width: u16,
    height: u16,
    first_pts_us: i64,
    pending: Option<(Vec<u8>, i64, bool)>, // AVCC bytes, pts_us, is_key
    path: String,
    frames: u64,
}

impl Recorder {
    pub fn new(path: String, width: u16, height: u16) -> Self {
        Self {
            writer: None,
            width,
            height,
            first_pts_us: -1,
            pending: None,
            path,
            frames: 0,
        }
    }

    /// Feed one video access unit (`is_config` = SPS/PPS packet).
    pub fn feed(&mut self, data: &[u8], pts_us: i64, is_key: bool, is_config: bool) {
        if is_config {
            if self.writer.is_none() {
                if let Some((sps, pps)) = split_sps_pps(data) {
                    if let Ok(w) = self.init_writer(&sps, &pps) {
                        self.writer = Some(w);
                    }
                }
            }
            return;
        }
        if self.writer.is_none() {
            return; // wait for the config (SPS/PPS) first
        }
        if self.first_pts_us < 0 {
            self.first_pts_us = pts_us;
        }
        let avcc = annexb_to_avcc(data);
        if avcc.is_empty() {
            return;
        }
        if let Some((bytes, ppts, pkey)) = self.pending.take() {
            self.write_sample(bytes, ppts, pkey, pts_us);
        }
        self.pending = Some((avcc, pts_us, is_key));
    }

    fn write_sample(&mut self, avcc: Vec<u8>, pts_us: i64, is_key: bool, next_pts_us: i64) {
        let start = ((pts_us - self.first_pts_us).max(0) / 1000) as u64;
        let dur = (((next_pts_us - pts_us).max(1000)) / 1000) as u32;
        let sample = Mp4Sample {
            start_time: start,
            duration: dur,
            rendering_offset: 0,
            is_sync: is_key,
            bytes: Bytes::from(avcc),
        };
        if let Some(w) = &mut self.writer {
            if w.write_sample(1, &sample).is_ok() {
                self.frames += 1;
            }
        }
    }

    fn init_writer(&self, sps: &[u8], pps: &[u8]) -> std::io::Result<Mp4Writer<BufWriter<File>>> {
        let file = File::create(&self.path)?;
        let cfg = Mp4Config {
            major_brand: FourCC::from(*b"isom"),
            minor_version: 512,
            compatible_brands: vec![
                FourCC::from(*b"isom"),
                FourCC::from(*b"iso2"),
                FourCC::from(*b"avc1"),
                FourCC::from(*b"mp41"),
            ],
            timescale: TIMESCALE,
        };
        let io_err = |e: mp4::Error| std::io::Error::new(std::io::ErrorKind::Other, e.to_string());
        let mut w = Mp4Writer::write_start(BufWriter::new(file), &cfg).map_err(io_err)?;
        let track = TrackConfig {
            track_type: TrackType::Video,
            timescale: TIMESCALE,
            language: "und".to_string(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: self.width,
                height: self.height,
                seq_param_set: sps.to_vec(),
                pic_param_set: pps.to_vec(),
            }),
        };
        w.add_track(&track).map_err(io_err)?;
        Ok(w)
    }

    /// Finish and flush. Returns (path, frame count).
    pub fn finish(mut self) -> Option<(String, u64)> {
        if let Some((bytes, ppts, pkey)) = self.pending.take() {
            self.write_sample(bytes, ppts, pkey, ppts + 33_000);
        }
        if let Some(mut w) = self.writer.take() {
            let _ = w.write_end();
            Some((self.path, self.frames))
        } else {
            None
        }
    }
}

/// NAL bodies (start codes stripped) from an Annex-B buffer.
fn nal_bodies(data: &[u8]) -> Vec<&[u8]> {
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
    let mut out = Vec::with_capacity(starts.len());
    for k in 0..starts.len() {
        let seg_end = if k + 1 < starts.len() { starts[k + 1] } else { data.len() };
        let seg = &data[starts[k]..seg_end];
        let body = if seg.len() >= 4 && seg[..4] == [0, 0, 0, 1] {
            &seg[4..]
        } else if seg.len() >= 3 && seg[..3] == [0, 0, 1] {
            &seg[3..]
        } else {
            seg
        };
        if !body.is_empty() {
            out.push(body);
        }
    }
    out
}

fn annexb_to_avcc(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 16);
    for nal in nal_bodies(data) {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

fn split_sps_pps(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut sps = None;
    let mut pps = None;
    for nal in nal_bodies(data) {
        match nal[0] & 0x1f {
            7 => sps = Some(nal.to_vec()),
            8 => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    Some((sps?, pps?))
}
