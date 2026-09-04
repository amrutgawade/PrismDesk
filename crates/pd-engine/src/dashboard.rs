//! PrismDesk dashboard — the control plane (egui). Lists connected devices and
//! their quality settings, and launches the native mirror as a child process
//! (`pd-engine --mirror ...`) so the UI stays responsive and each mirror is
//! isolated.
//!
//! Phase A design foundation: Geist typography, Lucide icon font, a dark/light
//! token palette with a persisted toggle. Bundled fonts live in `assets/fonts/`
//! (Geist — OFL-1.1; Lucide — ISC).

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, FontFamily, FontId, RichText, Rounding, Stroke};

// ============================ design tokens ============================

/// A full color set for one theme. Two instances (dark default + light) drive
/// every surface, so the toggle is a real re-theme, not just an egui flip.
#[derive(Clone, Copy)]
struct Palette {
    dark: bool,
    bg: Color32,
    surface: Color32,
    surface2: Color32,
    elevated: Color32,
    border: Color32,
    text: Color32,
    text2: Color32,
    dim: Color32,
    accent: Color32,
    accent_weak: Color32,
    cyan: Color32,
    cyan_weak: Color32,
    live: Color32,
    live_weak: Color32,
    warn: Color32,
    on_accent: Color32,
    extreme: Color32,
    shadow: Color32,
    switch_off: Color32,
}

impl Palette {
    fn new(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    fn dark() -> Self {
        Self {
            dark: true,
            bg: rgb(0x0e, 0x10, 0x14),
            surface: rgb(0x17, 0x1a, 0x20),
            surface2: rgb(0x20, 0x25, 0x2e),
            elevated: rgb(0x26, 0x2b, 0x35),
            border: Color32::from_white_alpha(20),
            text: rgb(0xe8, 0xeb, 0xf0),
            text2: rgb(0xa2, 0xa8, 0xb3),
            dim: rgb(0x82, 0x8a, 0x98),
            accent: rgb(0x8b, 0x5c, 0xf6),
            accent_weak: rgb(0x24, 0x1d, 0x3a),
            cyan: rgb(0x34, 0xe0, 0xd4),
            cyan_weak: rgb(0x12, 0x33, 0x33),
            live: rgb(0xef, 0x44, 0x44),
            live_weak: rgb(0x35, 0x1c, 0x1c),
            warn: rgb(0xd9, 0xa2, 0x1b),
            on_accent: rgb(0xff, 0xff, 0xff),
            extreme: rgb(0x0a, 0x0c, 0x0f),
            shadow: Color32::from_black_alpha(120),
            switch_off: rgb(0x3a, 0x40, 0x49),
        }
    }

    fn light() -> Self {
        Self {
            dark: false,
            bg: rgb(0xf4, 0xf5, 0xf7),
            surface: rgb(0xff, 0xff, 0xff),
            surface2: rgb(0xf0, 0xf1, 0xf4),
            elevated: rgb(0xff, 0xff, 0xff),
            border: Color32::from_black_alpha(23),
            text: rgb(0x16, 0x18, 0x1d),
            text2: rgb(0x4a, 0x50, 0x5c),
            dim: rgb(0x86, 0x8c, 0x98),
            accent: rgb(0x7c, 0x3a, 0xed),
            accent_weak: rgb(0xf1, 0xec, 0xfe),
            cyan: rgb(0x0e, 0xa5, 0xa2),
            cyan_weak: rgb(0xdc, 0xf4, 0xf2),
            live: rgb(0xdc, 0x26, 0x26),
            live_weak: rgb(0xfd, 0xec, 0xec),
            warn: rgb(0xb4, 0x53, 0x09),
            on_accent: rgb(0xff, 0xff, 0xff),
            extreme: rgb(0xff, 0xff, 0xff),
            shadow: Color32::from_black_alpha(30),
            switch_off: rgb(0xd1, 0xd5, 0xdb),
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

// ============================ icons (Lucide) ============================

/// Lucide glyph codepoints (verified against lucide-static font/lucide.css).
mod ic {
    pub const CAMERA: char = '\u{e064}';
    pub const CIRCLE: char = '\u{e076}';
    pub const SQUARE: char = '\u{e167}';
    pub const VOLUME: char = '\u{e1ab}';
    pub const VOLUME_X: char = '\u{e1ac}';
    pub const PLAY: char = '\u{e13c}';
    pub const POWER: char = '\u{e140}';
    pub const MONITOR: char = '\u{e11d}';
    pub const SMARTPHONE: char = '\u{e163}';
    pub const TABLET: char = '\u{e17e}';
    pub const REFRESH: char = '\u{e145}';
    pub const SUN: char = '\u{e178}';
    pub const MOON: char = '\u{e11e}';
    pub const KEYBOARD: char = '\u{e284}';
    pub const INFO: char = '\u{e0f9}';
    pub const MOUSE: char = '\u{e11f}';
    pub const SLIDERS: char = '\u{e29a}';
    pub const CHECK: char = '\u{e06c}';
}

/// The named egui font family that resolves to lucide.ttf.
fn icon_family() -> FontFamily {
    FontFamily::Name("icons".into())
}

fn med_family() -> FontFamily {
    FontFamily::Name("Geist Medium".into())
}

fn sb_family() -> FontFamily {
    FontFamily::Name("Geist SemiBold".into())
}

/// A single icon glyph as sized, colored text.
fn icon_rt(ch: char, size: f32, color: Color32) -> RichText {
    RichText::new(ch.to_string())
        .font(FontId::new(size, icon_family()))
        .color(color)
}

/// Geist SemiBold text (real weight, not egui's faux-strong).
fn sb(text: &str, size: f32, color: Color32) -> RichText {
    RichText::new(text)
        .font(FontId::new(size, sb_family()))
        .color(color)
}

/// An icon glyph + label as one widget (a LayoutJob mixing the icon and text
/// families), for buttons and tabs.
fn icon_label(ch: char, label: &str, icon_col: Color32, text_col: Color32) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let mut job = LayoutJob::default();
    job.append(
        &ch.to_string(),
        0.0,
        TextFormat {
            font_id: FontId::new(15.0, icon_family()),
            color: icon_col,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        &format!("  {label}"),
        0.0,
        TextFormat {
            font_id: FontId::new(13.5, med_family()),
            color: text_col,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job
}

/// A 32px square icon-only button with a tooltip (header actions).
fn icon_button(ui: &mut egui::Ui, pal: &Palette, ch: char, tip: &str) -> egui::Response {
    let btn = egui::Button::new(icon_rt(ch, 15.0, pal.text2))
        .fill(pal.surface2)
        .stroke(Stroke::new(1.0_f32,pal.border))
        .rounding(Rounding::same(8.0))
        .min_size(egui::vec2(32.0, 30.0));
    ui.add(btn).on_hover_text(tip)
}

/// A rounded pill (badge / chip).
fn pill(ui: &mut egui::Ui, text: RichText, bg: Color32) {
    egui::Frame::default()
        .fill(bg)
        .rounding(Rounding::same(999.0))
        .inner_margin(egui::Margin::symmetric(7.0, 2.0))
        .show(ui, |ui| {
            ui.label(text);
        });
}

// ============================ adb / devices ============================

fn adb_path() -> PathBuf {
    crate::adb_path()
}

#[derive(Clone)]
struct Device {
    serial: String,
    model: String,       // model code, e.g. "2311DRK48I"
    name: String,        // marketing name, e.g. "POCO X6 Pro 5G" (falls back to model)
    is_tablet: bool,
    authorized: bool,
}

const PRESETS: [&str; 3] = ["balanced", "crisp", "lowlatency"];
const PRESET_LABELS: [&str; 3] = ["Balanced", "Crisp", "Low-latency"];

// ============================ persisted config ============================

/// Tiny config persisted to %APPDATA%\PrismDesk\config.txt as `key=value` lines
/// (serde-free). Missing/corrupt file → defaults; unknown keys are ignored.
struct AppConfig {
    dark: bool,
    settings: HashMap<String, DevSettings>,
}

fn config_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    Path::new(&base).join("PrismDesk").join("config.txt")
}

impl AppConfig {
    fn load() -> Self {
        let mut cfg = AppConfig { dark: true, settings: HashMap::new() };
        let text = match std::fs::read_to_string(config_path()) {
            Ok(t) => t,
            Err(_) => return cfg,
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, val)) = line.split_once('=') else { continue };
            let (key, val) = (key.trim(), val.trim());
            if key == "theme" {
                cfg.dark = !val.eq_ignore_ascii_case("light");
            } else if let Some(s) = key.strip_prefix("preset.") {
                if let Ok(idx) = val.parse::<usize>() {
                    cfg.settings.entry(s.to_string()).or_default().preset = idx.min(PRESETS.len() - 1);
                }
            } else if let Some(s) = key.strip_prefix("audio.") {
                cfg.settings.entry(s.to_string()).or_default().audio = val.eq_ignore_ascii_case("true");
            } else if let Some(s) = key.strip_prefix("input.") {
                cfg.settings.entry(s.to_string()).or_default().input = val.eq_ignore_ascii_case("true");
            }
        }
        cfg
    }
}

/// Persist theme + per-device settings. Best-effort and never panics: an atomic
/// temp-file + rename keeps a crash from leaving a half-written config.
fn save_config(dark: bool, settings: &HashMap<String, DevSettings>) {
    let path = config_path();
    let Some(dir) = path.parent() else { return };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let mut out = String::with_capacity(64 + settings.len() * 48);
    out.push_str("# PrismDesk config (managed by the app)\n");
    out.push_str(if dark { "theme=dark\n" } else { "theme=light\n" });
    let mut serials: Vec<&String> = settings.keys().collect();
    serials.sort();
    for s in serials {
        if s.is_empty() || s.contains('=') || s.contains('\n') || s.contains('\r') {
            continue;
        }
        let d = &settings[s];
        out.push_str(&format!("preset.{s}={}\n", d.preset));
        out.push_str(&format!("audio.{s}={}\n", d.audio));
        out.push_str(&format!("input.{s}={}\n", d.input));
    }
    let tmp = dir.join("config.txt.tmp");
    if std::fs::write(&tmp, out.as_bytes()).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, &path);
}

// ============================ app setup ============================

/// The app/taskbar/title-bar icon (256×256 RGBA, generated from the prism mark).
fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/icon/prismdesk-256.rgba").to_vec(),
        width: 256,
        height: 256,
    }
}

pub fn run() -> eframe::Result<()> {
    let cfg = AppConfig::load();
    let dark = cfg.dark;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 600.0])
            .with_min_inner_size([380.0, 480.0])
            .with_icon(app_icon())
            .with_title("PrismDesk"),
        ..Default::default()
    };
    eframe::run_native(
        "PrismDesk",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx, &Palette::new(dark));
            Ok(Box::new(Dashboard::new(cfg)))
        }),
    )
}

/// Register Geist (proportional + weights + mono) and the Lucide icon font.
fn setup_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions};
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "Geist".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/Geist-Regular.ttf")),
    );
    fonts.font_data.insert(
        "Geist Medium".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/Geist-Medium.ttf")),
    );
    fonts.font_data.insert(
        "Geist SemiBold".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/Geist-SemiBold.ttf")),
    );
    fonts.font_data.insert(
        "Geist Bold".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/Geist-Bold.ttf")),
    );
    fonts.font_data.insert(
        "Geist Mono".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/GeistMono-Regular.ttf")),
    );
    fonts.font_data.insert(
        "icons".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/lucide.ttf")),
    );

    // Defaults (keep egui's built-ins as fallback for missing glyphs).
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Geist".to_owned());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "Geist Mono".to_owned());

    // Named families for explicit weights and the icon glyphs.
    fonts
        .families
        .insert(FontFamily::Name("Geist Medium".into()), vec!["Geist Medium".to_owned()]);
    fonts
        .families
        .insert(FontFamily::Name("Geist SemiBold".into()), vec!["Geist SemiBold".to_owned()]);
    fonts
        .families
        .insert(FontFamily::Name("Geist Bold".into()), vec!["Geist Bold".to_owned()]);
    fonts
        .families
        .insert(FontFamily::Name("icons".into()), vec!["icons".to_owned()]);

    ctx.set_fonts(fonts);
}

/// Apply the palette to egui visuals + set the Geist text scale. Called at
/// startup and again whenever the theme toggles.
fn setup_style(ctx: &egui::Context, pal: &Palette) {
    use egui::Visuals;
    let mut v = if pal.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = pal.bg;
    v.window_fill = pal.surface;
    v.extreme_bg_color = pal.extreme;
    v.override_text_color = Some(pal.text);
    v.selection.bg_fill = pal.accent.linear_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0_f32,pal.accent);
    v.hyperlink_color = pal.cyan;
    v.widgets.noninteractive.bg_fill = pal.surface;
    v.widgets.inactive.bg_fill = pal.surface2;
    v.widgets.inactive.weak_bg_fill = pal.surface2;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32,pal.text2);
    v.widgets.hovered.bg_fill = pal.elevated;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0_f32,pal.text);
    v.widgets.active.bg_fill = pal.accent;
    let r = Rounding::same(8.0);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = r;
    }
    v.window_rounding = Rounding::same(12.0);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(11.0, 7.0);
    use egui::TextStyle::{Body, Button, Heading, Monospace as Mono, Small};
    style.text_styles = [
        (Heading, FontId::new(19.0, sb_family())),
        (Body, FontId::new(14.0, FontFamily::Proportional)),
        (Button, FontId::new(13.5, med_family())),
        (Small, FontId::new(11.5, FontFamily::Proportional)),
        (Mono, FontId::new(12.0, FontFamily::Monospace)),
    ]
    .into();
    ctx.set_style(style);
}

#[derive(Clone)]
struct DevSettings {
    preset: usize,
    audio: bool,
    input: bool,
}
impl Default for DevSettings {
    fn default() -> Self {
        Self { preset: 0, audio: true, input: true }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Devices,
    Shortcuts,
    About,
}

/// A live mirror child process plus its control channel and runtime state.
/// Commands are pushed to `tx`; a per-mirror writer thread forwards them over a
/// localhost TCP socket to the child (reliable, ordered — unlike stdin, which
/// dropped commands when the child was a busy GUI-spawned process). The mirror
/// reports its recording/mute state back on the same socket, so `recording` and
/// `muted` are authoritative and reflect in-window Ctrl+R/M toggles too.
struct Proc {
    child: Child,
    tx: mpsc::Sender<String>,
    recording: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
}
impl Proc {
    /// Queue a command for the mirror. Returns false only if the writer thread
    /// (and thus the control channel) is gone.
    fn send(&self, cmd: &str) -> bool {
        self.tx.send(cmd.to_string()).is_ok()
    }
}

struct Dashboard {
    devices: Vec<Device>,
    last_refresh: Instant,
    status: String,
    running: HashMap<String, Proc>,         // serial -> live mirror process
    settings: HashMap<String, DevSettings>, // serial -> per-device config
    tab: Tab,
    egui_ctx: Option<egui::Context>, // for repainting when a mirror reports status
    dark: bool,
    pal: Palette,
    dev_info: HashMap<String, (String, bool)>, // serial -> (marketing name, is_tablet)
    status_at: Instant,                        // when `status` last changed (toast fade)
    last_status: String,                       // to detect status changes
    rec_since: HashMap<String, Instant>,       // serial -> recording start (timer)
    hovered_card: Option<String>,              // serial hovered last frame (elevation)
}

impl Dashboard {
    fn new(cfg: AppConfig) -> Self {
        let dark = cfg.dark;
        let mut d = Self {
            devices: Vec::new(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            status: String::new(),
            running: HashMap::new(),
            settings: cfg.settings,
            tab: Tab::Devices,
            egui_ctx: None,
            dark,
            pal: Palette::new(dark),
            dev_info: HashMap::new(),
            status_at: Instant::now(),
            last_status: String::new(),
            rec_since: HashMap::new(),
            hovered_card: None,
        };
        d.refresh();
        d
    }

    fn refresh(&mut self) {
        let mut devices = list_devices();
        // Resolve friendly names once per device (cached); patch each Device.
        for d in &mut devices {
            if !d.authorized {
                continue;
            }
            let info = self
                .dev_info
                .entry(d.serial.clone())
                .or_insert_with(|| resolve_device_info(&d.serial, &d.model));
            d.name = info.0.clone();
            d.is_tablet = info.1;
        }
        self.devices = devices;
        // Drop mirrors whose window the user closed (child exited).
        self.running
            .retain(|_, p| matches!(p.child.try_wait(), Ok(None)));
    }

    fn start_mirror(&mut self, serial: &str) {
        let preset = self.settings.entry(serial.to_string()).or_default();
        let (audio, input, preset_idx) = (preset.audio, preset.input, preset.preset);

        // Bind a localhost control listener BEFORE spawning; the child dials back
        // to this port for its command channel.
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(l) => l,
            Err(e) => {
                self.status = format!("Failed to open control port: {e}");
                return;
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                self.status = format!("Failed to read control port: {e}");
                return;
            }
        };

        use std::os::windows::process::CommandExt;
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pd-engine"));
        let mut cmd = Command::new(exe);
        cmd.creation_flags(crate::CREATE_NO_WINDOW);
        cmd.arg("--mirror")
            .arg("--serial")
            .arg(serial)
            .arg("--preset")
            .arg(PRESETS[preset_idx])
            .arg("--ctrl-port")
            .arg(port.to_string());
        if !audio {
            cmd.arg("--no-audio");
        }
        if !input {
            cmd.arg("--no-control");
        }
        // Diagnostics are opt-in: if the dashboard was launched with
        // PRISMDESK_DEBUG set, children inherit it and trace the control channel.
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                let (tx, rx) = mpsc::channel::<String>();
                let recording = Arc::new(AtomicBool::new(false));
                let muted = Arc::new(AtomicBool::new(false));
                spawn_control_io(
                    listener,
                    rx,
                    recording.clone(),
                    muted.clone(),
                    self.egui_ctx.clone(),
                    pid,
                );
                crate::debug_log("dash", &format!("spawned {serial} pid={pid} ctrl_port={port}"));
                self.running.insert(
                    serial.to_string(),
                    Proc { child, tx, recording, muted },
                );
                self.status = format!("Mirroring · {serial}");
            }
            Err(e) => self.status = format!("Failed to start: {e}"),
        }
    }

    fn stop_mirror(&mut self, serial: &str) {
        if let Some(mut p) = self.running.remove(serial) {
            // Ask for a clean shutdown (flushes any active recording); if the
            // control channel is already gone, fall back to killing the process.
            if !p.send("stop") {
                let _ = p.child.kill();
            }
            self.status = format!("Stopped · {serial}");
        }
    }

    fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.dark = !self.dark;
        self.pal = Palette::new(self.dark);
        setup_style(ctx, &self.pal);
    }

    /// Draw one device card (shadow, live rail, hover elevation) and its
    /// contents. Returns (settings changed, is-hovered).
    fn render_card(&mut self, ui: &mut egui::Ui, d: &Device, pal: &Palette) -> (bool, bool) {
        let live = self.running.contains_key(&d.serial);
        let hovered = self.hovered_card.as_deref() == Some(d.serial.as_str());
        let mut changed = false;
        let mut is_hovered = false;
        ui.push_id(&d.serial, |ui| {
            let stroke_col = if live {
                lerp_color(pal.border, pal.accent, 0.55)
            } else {
                pal.border
            };
            // Lift the shadow a touch on hover.
            let (dy, blur, spread) = if hovered {
                (5.0, 22.0, -3.0)
            } else {
                (3.0, 16.0, -4.0)
            };
            let resp = egui::Frame::default()
                .fill(pal.surface)
                .stroke(Stroke::new(1.0_f32,stroke_col))
                .rounding(Rounding::same(12.0))
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, dy),
                    blur,
                    spread,
                    color: pal.shadow,
                })
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    changed |= device_card(ui, pal, d, self);
                });
            if live {
                let r = resp.response.rect;
                let rail = egui::Rect::from_min_max(
                    egui::pos2(r.left(), r.top() + 12.0),
                    egui::pos2(r.left() + 3.0, r.bottom() - 12.0),
                );
                ui.painter().rect_filled(rail, Rounding::same(2.0), pal.accent);
            }
            if ui.rect_contains_pointer(resp.response.rect) {
                is_hovered = true;
            }
        });
        (changed, is_hovered)
    }
}

/// Accept one mirror's control connection (bounded wait), then run the two-way
/// channel: forward queued commands to it as `cmd\n` lines, and read `rec/mute`
/// status lines back into the shared flags (repainting the UI so the buttons
/// track the mirror's true state, including its in-window Ctrl+R/M toggles).
/// Commands queued before the child connects are buffered and delivered on
/// connect, so none are lost. Ends when the command sender is dropped (mirror
/// stopped/reaped) or the socket breaks.
fn spawn_control_io(
    listener: TcpListener,
    rx: mpsc::Receiver<String>,
    recording: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    ctx: Option<egui::Context>,
    pid: u32,
) {
    std::thread::spawn(move || {
        use std::io::Write;
        listener.set_nonblocking(true).ok();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut connected = None;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((s, _)) => {
                    connected = Some(s);
                    break;
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(_) => break,
            }
        }
        let mut stream = match connected {
            Some(s) => {
                s.set_nonblocking(false).ok();
                crate::debug_log("dash", &format!("control: accepted mirror pid={pid}"));
                s
            }
            None => {
                crate::debug_log("dash", &format!("control: mirror pid={pid} never connected"));
                return;
            }
        };

        // Status reader: mirror -> dashboard (recording/mute), on a clone so it
        // can read while this thread writes.
        if let Ok(rs) = stream.try_clone() {
            std::thread::spawn(move || {
                use std::io::BufRead;
                let mut r = std::io::BufReader::new(rs);
                let mut line = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            let changed = match parts.as_slice() {
                                ["rec", v] => {
                                    recording.store(*v == "1", Ordering::Relaxed);
                                    true
                                }
                                ["mute", v] => {
                                    muted.store(*v == "1", Ordering::Relaxed);
                                    true
                                }
                                _ => false,
                            };
                            if changed {
                                crate::debug_log("dash", &format!("status: {} pid={pid}", line.trim()));
                                if let Some(c) = &ctx {
                                    c.request_repaint();
                                }
                            }
                        }
                    }
                }
            });
        }

        // Command writer: dashboard -> mirror.
        for cmd in rx {
            let line = format!("{cmd}\n");
            if stream.write_all(line.as_bytes()).and_then(|_| stream.flush()).is_err() {
                crate::debug_log("dash", &format!("control: write failed pid={pid}"));
                break;
            }
        }
        crate::debug_log("dash", &format!("control: writer ended pid={pid}"));
    });
}

impl eframe::App for Dashboard {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.egui_ctx.is_none() {
            self.egui_ctx = Some(ctx.clone()); // for repaint on async mirror status
        }
        if self.last_refresh.elapsed() > Duration::from_secs(2) {
            self.refresh();
            self.last_refresh = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));

        // Stamp when the status text last changed, for the toast fade.
        if self.status != self.last_status {
            self.last_status = self.status.clone();
            self.status_at = Instant::now();
        }

        let pal = self.pal;
        let mut theme_clicked = false;
        let mut config_changed = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(pal.bg).inner_margin(egui::Margin::same(18.0)))
            .show(ctx, |ui| {
                // Cap width on very wide windows — but never grow past the actual
                // window (set_max_width would otherwise stretch content off-screen
                // when the window is narrow).
                ui.set_max_width(ui.available_width().min(880.0));
                // ---- header ------------------------------------------------
                ui.horizontal(|ui| {
                    ui.label(RichText::new("◆").size(15.0).color(pal.accent));
                    ui.add_space(1.0);
                    ui.label(sb("PrismDesk", 18.0, pal.text));
                    ui.add_space(2.0);
                    pill(
                        ui,
                        RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                            .small()
                            .color(pal.accent),
                        pal.accent_weak,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (tico, ttip) = if self.dark {
                            (ic::SUN, "Switch to light theme")
                        } else {
                            (ic::MOON, "Switch to dark theme")
                        };
                        if icon_button(ui, &pal, tico, ttip).clicked() {
                            theme_clicked = true;
                        }
                        if icon_button(ui, &pal, ic::REFRESH, "Refresh devices").clicked() {
                            self.refresh();
                            self.last_refresh = Instant::now();
                        }
                        let n = self.devices.len();
                        ui.label(
                            RichText::new(format!("{n} device{}", if n == 1 { "" } else { "s" }))
                                .small()
                                .color(pal.dim),
                        );
                    });
                });

                accent_rule(ui, &pal);
                ui.add_space(10.0);

                // ---- tabs --------------------------------------------------
                ui.horizontal(|ui| {
                    for (tab, ch, name) in [
                        (Tab::Devices, ic::MONITOR, "Devices"),
                        (Tab::Shortcuts, ic::KEYBOARD, "Shortcuts"),
                        (Tab::About, ic::INFO, "About"),
                    ] {
                        let sel = self.tab == tab;
                        let col = if sel { pal.accent } else { pal.dim };
                        let job = icon_label(ch, name, col, col);
                        if ui.add(egui::Button::new(job).frame(false)).clicked() {
                            self.tab = tab;
                        }
                    }
                });
                ui.add_space(12.0);

                // Header + tabs stay pinned; the tab content scrolls so nothing
                // is cut off on a short window.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                if self.tab == Tab::Shortcuts {
                    shortcuts_view(ui, &pal);
                    return;
                }
                if self.tab == Tab::About {
                    about_view(ui, &pal);
                    return;
                }

                // ---- devices ----------------------------------------------
                ui.label(RichText::new("DEVICES").small().color(pal.dim).strong());
                ui.add_space(8.0);

                let devices = self.devices.clone();
                if devices.is_empty() {
                    empty_state(ui, &pal);
                }

                // Responsive: two columns only when there's room AND ≥2 devices.
                let cols = if ui.available_width() >= 680.0 && devices.len() >= 2 {
                    2
                } else {
                    1
                };
                let mut next_hovered = None;
                for chunk in devices.chunks(cols) {
                    if cols == 1 {
                        let d = &chunk[0];
                        let (ch, hov) = self.render_card(ui, d, &pal);
                        config_changed |= ch;
                        if hov {
                            next_hovered = Some(d.serial.clone());
                        }
                    } else {
                        ui.columns(cols, |columns| {
                            for (i, d) in chunk.iter().enumerate() {
                                let (ch, hov) = self.render_card(&mut columns[i], d, &pal);
                                config_changed |= ch;
                                if hov {
                                    next_hovered = Some(d.serial.clone());
                                }
                            }
                        });
                    }
                    ui.add_space(10.0);
                }
                self.hovered_card = next_hovered;
                    });
            });

        if theme_clicked {
            self.toggle_theme(ctx);
            config_changed = true;
        }
        if config_changed {
            save_config(self.dark, &self.settings);
        }

        // ---- toast (auto-fading status), floating bottom-center ----
        if !self.status.is_empty() {
            let elapsed = self.status_at.elapsed().as_secs_f32();
            let alpha = if elapsed < 3.0 {
                1.0
            } else {
                (1.0 - (elapsed - 3.0) / 0.6).clamp(0.0, 1.0)
            };
            if alpha > 0.01 {
                ctx.request_repaint(); // animate the fade
                let fade = |c: Color32| c.linear_multiply(alpha);
                egui::Area::new(egui::Id::new("pd_toast"))
                    .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::default()
                            .fill(fade(pal.surface2))
                            .stroke(Stroke::new(1.0_f32,fade(pal.border)))
                            .rounding(Rounding::same(10.0))
                            .shadow(egui::epaint::Shadow {
                                offset: egui::vec2(0.0, 4.0),
                                blur: 18.0,
                                spread: -4.0,
                                color: fade(pal.shadow),
                            })
                            .inner_margin(egui::Margin::symmetric(13.0, 9.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(icon_rt(ic::CHECK, 14.0, fade(pal.cyan)));
                                    ui.add_space(2.0);
                                    ui.label(RichText::new(&self.status).color(fade(pal.text)));
                                });
                            });
                    });
            }
        }
    }
}

/// Render one device card's contents. Returns true if a persisted setting
/// changed (so the caller writes config). Split out to keep `update` readable.
fn device_card(ui: &mut egui::Ui, pal: &Palette, d: &Device, app: &mut Dashboard) -> bool {
    let mut changed = false;
    let running = app.running.contains_key(&d.serial);

    ui.horizontal(|ui| {
        // Device-type icon (phone / tablet).
        egui::Frame::default()
            .fill(pal.surface2)
            .stroke(Stroke::new(1.0_f32,pal.border))
            .rounding(Rounding::same(9.0))
            .inner_margin(egui::Margin::same(7.0))
            .show(ui, |ui| {
                let ch = if d.is_tablet { ic::TABLET } else { ic::SMARTPHONE };
                ui.label(icon_rt(ch, 18.0, pal.text2));
            });
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if running {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(7.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, pal.cyan);
                }
                ui.label(sb(&d.name, 15.5, pal.text));
            });
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                pill(
                    ui,
                    RichText::new("USB").small().color(pal.cyan),
                    pal.cyan_weak,
                );
                ui.label(RichText::new(&d.model).monospace().small().color(pal.dim))
                    .on_hover_text(&d.serial);
            });
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !d.authorized {
                ui.label(RichText::new("unauthorized").color(pal.warn));
            } else if running {
                let btn = egui::Button::new(icon_label(ic::POWER, "Stop", pal.live, pal.live))
                    .fill(pal.live_weak)
                    .stroke(Stroke::new(1.0_f32,pal.live))
                    .rounding(Rounding::same(8.0))
                    .min_size(egui::vec2(96.0, 34.0));
                if ui.add(btn).clicked() {
                    app.stop_mirror(&d.serial);
                }
            } else {
                let btn = egui::Button::new(icon_label(
                    ic::PLAY,
                    "Start Mirror",
                    pal.on_accent,
                    pal.on_accent,
                ))
                .fill(pal.accent)
                .rounding(Rounding::same(8.0))
                .min_size(egui::vec2(128.0, 34.0));
                if ui.add(btn).clicked() {
                    app.start_mirror(&d.serial);
                }
            }
        });
    });

    if !d.authorized {
        return changed;
    }

    ui.add_space(10.0);
    if running {
        // Live: dashboard-driven capture controls.
        let audio_on = app.settings.get(&d.serial).map(|s| s.audio).unwrap_or(true);
        ui.horizontal(|ui| {
            if action_btn(ui, pal, ic::CAMERA, "Snapshot", pal.surface2, pal.text2).clicked() {
                let ok = app.running.get(&d.serial).map(|p| p.send("snapshot")).unwrap_or(false);
                app.status = if ok {
                    format!("Snapshot · {}", d.model)
                } else {
                    format!("Snapshot failed · {} not reachable", d.model)
                };
            }
            // Label + icon reflect the mirror's reported state (status channel),
            // so they stay right even for in-window Ctrl+R toggles. While
            // recording, the button counts up (mm:ss).
            let rec = app.running.get(&d.serial).map(|p| p.recording.load(Ordering::Relaxed)).unwrap_or(false);
            let rlabel;
            let (rch, rfill, rcol) = if rec {
                let start = *app.rec_since.entry(d.serial.clone()).or_insert_with(Instant::now);
                let secs = start.elapsed().as_secs();
                rlabel = format!("{}:{:02}", secs / 60, secs % 60);
                (ic::SQUARE, pal.live_weak, pal.live)
            } else {
                app.rec_since.remove(&d.serial);
                rlabel = "Record".to_string();
                (ic::CIRCLE, pal.surface2, pal.text2)
            };
            if action_btn(ui, pal, rch, &rlabel, rfill, rcol).clicked() {
                let ok = app.running.get(&d.serial).map(|p| p.send("record")).unwrap_or(false);
                app.status = if ok {
                    format!("{} · {}", if rec { "Stopping recording" } else { "Recording" }, d.model)
                } else {
                    format!("Record failed · {} not reachable", d.model)
                };
            }
            if audio_on {
                let muted = app.running.get(&d.serial).map(|p| p.muted.load(Ordering::Relaxed)).unwrap_or(false);
                let (mch, mlabel) = if muted { (ic::VOLUME_X, "Unmute") } else { (ic::VOLUME, "Mute") };
                if action_btn(ui, pal, mch, mlabel, pal.surface2, pal.text2).clicked() {
                    let ok = app.running.get(&d.serial).map(|p| p.send("mute")).unwrap_or(false);
                    app.status = if ok {
                        format!("{} · {}", if muted { "Unmuting" } else { "Muting" }, d.model)
                    } else {
                        format!("Mute failed · {} not reachable", d.model)
                    };
                }
            }
        });
    } else {
        // Idle: per-device quality/config.
        let s = app.settings.entry(d.serial.clone()).or_default();
        ui.horizontal(|ui| {
            ui.label(icon_rt(ic::SLIDERS, 14.0, pal.dim));
            ui.add_space(2.0);
            changed |= segmented_preset(ui, pal, &mut s.preset);
        });
        ui.add_space(9.0);
        ui.horizontal(|ui| {
            changed |= toggle_switch(ui, pal, &mut s.audio, ic::VOLUME, "Audio", "audio");
            ui.add_space(14.0);
            changed |= toggle_switch(ui, pal, &mut s.input, ic::MOUSE, "Input", "input");
        });
    }
    changed
}

/// A shadcn-style segmented control for the quality preset. Returns true on change.
fn segmented_preset(ui: &mut egui::Ui, pal: &Palette, sel: &mut usize) -> bool {
    let mut changed = false;
    egui::Frame::default()
        .fill(pal.surface2)
        .stroke(Stroke::new(1.0_f32,pal.border))
        .rounding(Rounding::same(9.0))
        .inner_margin(egui::Margin::same(3.0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (i, label) in PRESET_LABELS.iter().enumerate() {
                    let on = *sel == i;
                    let (fill, txt) = if on {
                        (pal.elevated, pal.text)
                    } else {
                        (Color32::TRANSPARENT, pal.dim)
                    };
                    let btn = egui::Button::new(RichText::new(*label).size(12.0).color(txt))
                        .fill(fill)
                        .rounding(Rounding::same(6.0))
                        .min_size(egui::vec2(0.0, 24.0));
                    if ui.add(btn).clicked() && !on {
                        *sel = i;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// An iOS-style toggle switch with an icon + label. Returns true on change.
fn toggle_switch(
    ui: &mut egui::Ui,
    pal: &Palette,
    on: &mut bool,
    ch: char,
    label: &str,
    id_salt: &str,
) -> bool {
    // Explicit, serial-scoped id so identical switches across device cards never
    // collide (an auto-id collision made the 2nd card's toggle unresponsive).
    let id = ui.id().with(id_salt);
    let row = ui
        .horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(32.0, 18.0), egui::Sense::hover());
            let track = if *on { pal.accent } else { pal.switch_off };
            ui.painter().rect_filled(rect, Rounding::same(9.0), track);
            let kx = if *on { rect.right() - 9.0 } else { rect.left() + 9.0 };
            ui.painter().circle_filled(egui::pos2(kx, rect.center().y), 6.5, pal.on_accent);
            ui.add_space(7.0);
            ui.label(icon_rt(ch, 13.0, pal.dim));
            ui.add_space(1.0);
            ui.label(RichText::new(label).color(pal.text2));
        })
        .response;
    // The whole row is the click target.
    let resp = ui
        .interact(row.rect, id, egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *on = !*on;
        true
    } else {
        false
    }
}

// ============================ shared views ============================

/// A thin cyan→violet accent divider.
fn accent_rule(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(10.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 2.0), egui::Sense::hover());
    let painter = ui.painter();
    let steps = 48;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let col = lerp_color(pal.cyan, pal.accent, (t0 + t1) * 0.5);
        let x0 = rect.left() + rect.width() * t0;
        let x1 = rect.left() + rect.width() * t1;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            0.0,
            col,
        );
    }
}

fn empty_state(ui: &mut egui::Ui, pal: &Palette) {
    egui::Frame::default()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32,pal.border))
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::same(18.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon_rt(ic::MONITOR, 22.0, pal.dim));
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.label(sb("No device found", 14.0, pal.text));
                    ui.label(
                        RichText::new("Connect a phone via USB with debugging enabled.")
                            .small()
                            .color(pal.dim),
                    );
                });
            });
        });
}

/// A capture-action pill button (icon + label).
fn action_btn(
    ui: &mut egui::Ui,
    pal: &Palette,
    ch: char,
    label: &str,
    fill: Color32,
    text_col: Color32,
) -> egui::Response {
    ui.add(
        egui::Button::new(icon_label(ch, label, text_col, text_col))
            .fill(fill)
            .stroke(Stroke::new(1.0_f32,pal.border))
            .rounding(Rounding::same(8.0))
            .min_size(egui::vec2(0.0, 32.0)),
    )
}

/// A rounded surface card wrapping arbitrary content (Shortcuts/About).
fn card(ui: &mut egui::Ui, pal: &Palette, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::default()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32,pal.border))
        .rounding(Rounding::same(12.0))
        .inner_margin(egui::Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() - 32.0);
            add(ui);
        });
    ui.add_space(10.0);
}

/// One "Key  —  meaning" row inside a shortcuts card.
fn key_row(ui: &mut egui::Ui, pal: &Palette, keys: &str, what: &str) {
    ui.horizontal(|ui| {
        pill(
            ui,
            RichText::new(keys).monospace().small().color(pal.text),
            pal.surface2,
        );
        ui.add_space(4.0);
        ui.label(RichText::new(what).color(pal.dim));
    });
    ui.add_space(5.0);
}

fn shortcuts_view(ui: &mut egui::Ui, pal: &Palette) {
    ui.label(RichText::new("KEYBOARD & MOUSE").small().color(pal.dim).strong());
    ui.add_space(6.0);
    card(ui, pal, |ui| {
        ui.label(sb("In the mirror window", 14.0, pal.text));
        ui.add_space(8.0);
        key_row(ui, pal, "Left click / drag", "Tap & swipe on the device");
        key_row(ui, pal, "Mouse wheel", "Scroll");
        key_row(ui, pal, "Right click", "Back");
        key_row(ui, pal, "Type", "Send text to the focused field");
        key_row(ui, pal, "Backspace / Enter / Tab / arrows", "Sent as key events");
    });
    card(ui, pal, |ui| {
        ui.label(sb("Hotkeys", 14.0, pal.text));
        ui.add_space(8.0);
        key_row(ui, pal, "F11", "Toggle borderless fullscreen");
        key_row(ui, pal, "Ctrl + M", "Mute / unmute device audio");
        key_row(ui, pal, "Ctrl + S", "Save a screenshot (PNG)");
        key_row(ui, pal, "Ctrl + R", "Start / stop recording (MP4)");
        key_row(ui, pal, "Ctrl + V", "Paste PC clipboard to the device");
    });
    ui.label(
        RichText::new("Screenshots and recordings are written next to the app executable.")
            .small()
            .color(pal.dim),
    );
}

fn about_view(ui: &mut egui::Ui, pal: &Palette) {
    ui.label(RichText::new("ABOUT").small().color(pal.dim).strong());
    ui.add_space(6.0);
    card(ui, pal, |ui| {
        ui.horizontal(|ui| {
            ui.label(sb("PrismDesk", 20.0, pal.text));
            ui.add_space(2.0);
            pill(
                ui,
                RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                    .small()
                    .color(pal.accent),
                pal.accent_weak,
            );
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Low-latency Android screen mirroring & control for Windows.")
                .color(pal.dim),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "Hardware H.264/H.265 decode via Media Foundation (NVDEC), \
                 flip-model D3D11 presentation, and a scrcpy-compatible transport.",
            )
            .small()
            .color(pal.dim),
        );
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Designed & built by").small().color(pal.dim));
            ui.add_space(-1.0);
            ui.hyperlink_to(sb("Amrut Gawade", 13.0, pal.accent), "https://amrut.is-a.dev/")
                .on_hover_text("amrut.is-a.dev");
        });
    });
    card(ui, pal, |ui| {
        ui.label(sb("Open-source components", 14.0, pal.text));
        ui.add_space(8.0);
        for (name, lic) in [
            ("scrcpy protocol (Genymobile)", "Apache-2.0"),
            ("egui / eframe", "MIT / Apache-2.0"),
            ("windows-rs", "MIT / Apache-2.0"),
            ("cpal", "Apache-2.0"),
            ("mp4-rust", "MIT"),
            ("clipboard-win", "MIT / Apache-2.0"),
            ("Geist font", "OFL-1.1"),
            ("Lucide icons", "ISC"),
        ] {
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).color(pal.text2));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(lic).small().color(pal.dim));
                });
            });
            ui.add_space(2.0);
        }
    });
    ui.label(
        RichText::new("Uses adb from the Android SDK platform-tools. Not affiliated with Google or Genymobile.")
            .small()
            .color(pal.dim),
    );
}

fn list_devices() -> Vec<Device> {
    use std::os::windows::process::CommandExt;
    let out = match Command::new(adb_path())
        .creation_flags(crate::CREATE_NO_WINDOW)
        .args(["devices", "-l"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut devices = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = match parts.next() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let state = parts.next().unwrap_or("");
        let authorized = state == "device";
        let model = line
            .split_whitespace()
            .find_map(|t| t.strip_prefix("model:"))
            .map(|m| m.replace('_', " "))
            .unwrap_or_else(|| "Android device".to_string());
        devices.push(Device {
            name: model.clone(), // patched to the marketing name once resolved
            serial,
            model,
            is_tablet: false,
            authorized,
        });
    }
    devices
}

/// Resolve a device's friendly marketing name + whether it's a tablet via a
/// single `adb shell` round-trip (several getprop reads in order). Falls back to
/// manufacturer + model, then the bare model code. Only call for authorized
/// devices; the result is cached per serial so this runs once per device.
fn resolve_device_info(serial: &str, model: &str) -> (String, bool) {
    // Order matters: first non-empty marketing prop wins.
    let script = "getprop ro.product.marketname; \
                  getprop ro.vendor.oplus.market.name; \
                  getprop ro.product.realme.marketname; \
                  getprop ro.oppo.market.name; \
                  getprop ro.config.marketing_name; \
                  getprop ro.product.vendor.marketname; \
                  getprop ro.product.odm.marketname; \
                  getprop ro.product.manufacturer; \
                  getprop ro.build.characteristics";
    use std::os::windows::process::CommandExt;
    let out = Command::new(adb_path())
        .creation_flags(crate::CREATE_NO_WINDOW)
        .args(["-s", serial, "shell", script])
        .output();
    let text = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).into_owned(),
        Err(_) => String::new(),
    };
    let lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
    let name = lines
        .iter()
        .take(7)
        .find(|l| !l.is_empty())
        .cloned()
        .unwrap_or_else(|| {
            let manuf = lines.get(7).cloned().unwrap_or_default();
            if !manuf.is_empty() && !manuf.eq_ignore_ascii_case("unknown") {
                format!("{manuf} {model}")
            } else {
                model.to_string()
            }
        });
    let is_tablet = lines.get(8).map(|c| c.contains("tablet")).unwrap_or(false);
    (name, is_tablet)
}
