//! scrcpy control-socket message encoding (client -> device), big-endian.
//! v1: mouse as touch injection + wheel as scroll. Keyboard/UHID come later.

use std::io::{Read, Write};
use std::net::TcpStream;

const TYPE_INJECT_TOUCH: u8 = 2;
const TYPE_INJECT_SCROLL: u8 = 3;
const TYPE_SET_CLIPBOARD: u8 = 9;
const POINTER_MOUSE: u64 = 0xFFFF_FFFF_FFFF_FFFF; // scrcpy POINTER_ID_MOUSE
const BUTTON_PRIMARY: u32 = 1; // AMOTION_EVENT_BUTTON_PRIMARY

pub const ACTION_DOWN: u8 = 0;
pub const ACTION_UP: u8 = 1;
pub const ACTION_MOVE: u8 = 2;

fn f2u16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0) as u16
}
fn f2i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// INJECT_TOUCH_EVENT (32 bytes). `x,y` in a frame of size `w,h` (device space).
pub fn touch(action: u8, x: u32, y: u32, w: u32, h: u32, pressed: bool) -> Vec<u8> {
    let mut b = Vec::with_capacity(32);
    b.push(TYPE_INJECT_TOUCH);
    b.push(action);
    b.extend_from_slice(&POINTER_MOUSE.to_be_bytes());
    b.extend_from_slice(&(x as i32).to_be_bytes());
    b.extend_from_slice(&(y as i32).to_be_bytes());
    b.extend_from_slice(&(w as u16).to_be_bytes());
    b.extend_from_slice(&(h as u16).to_be_bytes());
    let pressure = if action == ACTION_UP { 0.0 } else { 1.0 };
    b.extend_from_slice(&f2u16(pressure).to_be_bytes());
    let action_button = if action == ACTION_MOVE { 0 } else { BUTTON_PRIMARY };
    b.extend_from_slice(&action_button.to_be_bytes());
    let buttons = if pressed { BUTTON_PRIMARY } else { 0 };
    b.extend_from_slice(&buttons.to_be_bytes());
    b
}

/// INJECT_SCROLL_EVENT (21 bytes). `vscroll` in [-1,1].
pub fn scroll(x: u32, y: u32, w: u32, h: u32, vscroll: f32) -> Vec<u8> {
    let mut b = Vec::with_capacity(21);
    b.push(TYPE_INJECT_SCROLL);
    b.extend_from_slice(&(x as i32).to_be_bytes());
    b.extend_from_slice(&(y as i32).to_be_bytes());
    b.extend_from_slice(&(w as u16).to_be_bytes());
    b.extend_from_slice(&(h as u16).to_be_bytes());
    b.extend_from_slice(&f2i16(0.0).to_be_bytes()); // hscroll
    b.extend_from_slice(&f2i16(vscroll).to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes()); // buttons
    b
}

/// SET_CLIPBOARD: set the device clipboard to `text`; `paste` also injects a
/// paste into the focused field. sequence=0 (no ack requested).
pub fn set_clipboard(text: &str, paste: bool) -> Vec<u8> {
    let t = text.as_bytes();
    let mut b = Vec::with_capacity(14 + t.len());
    b.push(TYPE_SET_CLIPBOARD);
    b.extend_from_slice(&0u64.to_be_bytes()); // sequence
    b.push(u8::from(paste));
    b.extend_from_slice(&(t.len() as u32).to_be_bytes());
    b.extend_from_slice(t);
    b
}

pub fn send(stream: &mut TcpStream, msg: &[u8]) {
    if let Err(e) = stream.write_all(msg) {
        eprintln!("[control] send err: {e}");
    }
}

/// Read one device->client message. Returns Some(text) for a CLIPBOARD message
/// (device clipboard changed), None for others (ack / uhid output).
pub fn read_device_msg(stream: &mut impl Read) -> std::io::Result<Option<String>> {
    let mut t = [0u8; 1];
    stream.read_exact(&mut t)?;
    match t[0] {
        0 => {
            // CLIPBOARD: u32 length + UTF-8 text
            let mut l = [0u8; 4];
            stream.read_exact(&mut l)?;
            let n = u32::from_be_bytes(l) as usize;
            let mut buf = vec![0u8; n];
            stream.read_exact(&mut buf)?;
            Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
        }
        1 => {
            // ACK_CLIPBOARD: u64 sequence
            let mut s = [0u8; 8];
            stream.read_exact(&mut s)?;
            Ok(None)
        }
        2 => {
            // UHID_OUTPUT: u16 id + u16 size + data
            let mut h = [0u8; 4];
            stream.read_exact(&mut h)?;
            let size = u16::from_be_bytes([h[2], h[3]]) as usize;
            let mut d = vec![0u8; size];
            stream.read_exact(&mut d)?;
            Ok(None)
        }
        _ => Ok(None),
    }
}
