//! scrcpy control-socket message encoding (client -> device), big-endian.
//! v1: mouse as touch injection + wheel as scroll. Keyboard/UHID come later.

use std::io::Write;
use std::net::TcpStream;

const TYPE_INJECT_TOUCH: u8 = 2;
const TYPE_INJECT_SCROLL: u8 = 3;
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

pub fn send(stream: &mut TcpStream, msg: &[u8]) {
    if let Err(e) = stream.write_all(msg) {
        eprintln!("[control] send err: {e}");
    }
}
