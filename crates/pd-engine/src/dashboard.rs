//! PrismDesk dashboard — the control plane (egui). Lists connected devices and
//! their quality settings, and launches the native mirror as a child process
//! (`pd-engine --mirror ...`) so the UI stays responsive and each mirror is
//! isolated.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use eframe::egui;
use egui::{Color32, RichText, Rounding};

// Dark graphite + a restrained "prism" accent (violet CTA, cyan highlight).
const BG: Color32 = Color32::from_rgb(0x0e, 0x10, 0x14);
const SURFACE: Color32 = Color32::from_rgb(0x17, 0x1a, 0x20);
const SURFACE2: Color32 = Color32::from_rgb(0x20, 0x25, 0x2e);
const TEXT: Color32 = Color32::from_rgb(0xe8, 0xeb, 0xf0);
const DIM: Color32 = Color32::from_rgb(0x8a, 0x93, 0xa2);
const ACCENT: Color32 = Color32::from_rgb(0x8b, 0x5c, 0xf6);
const CYAN: Color32 = Color32::from_rgb(0x34, 0xe0, 0xd4);
const WARN: Color32 = Color32::from_rgb(0xd9, 0xa2, 0x1b);
const STOP: Color32 = Color32::from_rgb(0xc0, 0x39, 0x2b);

fn adb_path() -> PathBuf {
    let bundled = Path::new(r"C:\platform-tools\adb.exe");
    if bundled.exists() {
        bundled.to_path_buf()
    } else {
        PathBuf::from("adb")
    }
}

#[derive(Clone)]
struct Device {
    serial: String,
    model: String,
    authorized: bool,
}

const PRESETS: [&str; 3] = ["balanced", "crisp", "lowlatency"];

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([440.0, 560.0])
            .with_min_inner_size([380.0, 460.0])
            .with_title("PrismDesk"),
        ..Default::default()
    };
    eframe::run_native(
        "PrismDesk",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(Dashboard::new()))
        }),
    )
}

fn setup_style(ctx: &egui::Context) {
    use egui::{FontId, Stroke, Visuals};
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = Color32::from_rgb(0x0a, 0x0c, 0x0f);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.linear_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = CYAN;
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
    v.widgets.noninteractive.bg_fill = SURFACE;
    v.widgets.inactive.bg_fill = SURFACE2;
    v.widgets.inactive.weak_bg_fill = SURFACE2;
    v.widgets.hovered.bg_fill = SURFACE2;
    v.widgets.active.bg_fill = ACCENT;
    v.window_rounding = Rounding::same(10.0);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    use egui::FontFamily::{Monospace, Proportional};
    use egui::TextStyle::{Body, Button, Heading, Monospace as MonoStyle, Small};
    style.text_styles = [
        (Heading, FontId::new(26.0, Proportional)),
        (Body, FontId::new(15.0, Proportional)),
        (Button, FontId::new(15.0, Proportional)),
        (Small, FontId::new(12.0, Proportional)),
        (MonoStyle, FontId::new(13.0, Monospace)),
    ]
    .into();
    ctx.set_style(style);
}

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

struct Dashboard {
    devices: Vec<Device>,
    last_refresh: Instant,
    status: String,
    running: HashMap<String, Child>,       // serial -> live mirror process
    settings: HashMap<String, DevSettings>, // serial -> per-device config
}

impl Dashboard {
    fn new() -> Self {
        let mut d = Self {
            devices: Vec::new(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            status: String::new(),
            running: HashMap::new(),
            settings: HashMap::new(),
        };
        d.refresh();
        d
    }

    fn refresh(&mut self) {
        self.devices = list_devices();
        // Drop mirrors whose window the user closed (child exited).
        self.running
            .retain(|_, c| matches!(c.try_wait(), Ok(None)));
    }

    fn start_mirror(&mut self, serial: &str) {
        let s = self.settings.entry(serial.to_string()).or_default();
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pd-engine"));
        let mut cmd = Command::new(exe);
        cmd.arg("--mirror")
            .arg("--serial")
            .arg(serial)
            .arg("--preset")
            .arg(PRESETS[s.preset]);
        if !s.audio {
            cmd.arg("--no-audio");
        }
        if !s.input {
            cmd.arg("--no-control");
        }
        match cmd.spawn() {
            Ok(child) => {
                self.running.insert(serial.to_string(), child);
                self.status = format!("Mirroring · {serial}");
            }
            Err(e) => self.status = format!("Failed to start: {e}"),
        }
    }

    fn stop_mirror(&mut self, serial: &str) {
        if let Some(mut c) = self.running.remove(serial) {
            let _ = c.kill();
            self.status = format!("Stopped · {serial}");
        }
    }
}

impl eframe::App for Dashboard {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() > Duration::from_secs(2) {
            self.refresh();
            self.last_refresh = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(BG).inner_margin(egui::Margin::same(18.0)))
            .show(ctx, |ui| {
                // Header / wordmark
                ui.horizontal(|ui| {
                    ui.label(RichText::new("◆").size(20.0).color(CYAN));
                    ui.add_space(2.0);
                    ui.label(RichText::new("Prism").size(26.0).strong().color(ACCENT));
                    ui.add_space(-4.0);
                    ui.label(RichText::new("Desk").size(26.0).strong().color(TEXT));
                });
                ui.label(RichText::new("Android screen mirroring").color(DIM));
                accent_rule(ui);
                ui.add_space(12.0);
                ui.label(RichText::new("DEVICES").small().color(DIM).strong());
                ui.add_space(6.0);

                let devices = self.devices.clone();
                if devices.is_empty() {
                    egui::Frame::default()
                        .fill(SURFACE)
                        .rounding(Rounding::same(10.0))
                        .inner_margin(egui::Margin::same(16.0))
                        .show(ui, |ui| {
                            ui.label(RichText::new("No device found").color(TEXT).strong());
                            ui.label(
                                RichText::new("Connect a phone via USB with debugging enabled.")
                                    .small()
                                    .color(DIM),
                            );
                        });
                }

                for d in &devices {
                    egui::Frame::default()
                        .fill(SURFACE)
                        .rounding(Rounding::same(10.0))
                        .inner_margin(egui::Margin::same(14.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&d.model).size(16.0).strong().color(TEXT));
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(" USB ")
                                                .small()
                                                .color(BG)
                                                .background_color(CYAN),
                                        );
                                        ui.label(RichText::new(&d.serial).small().color(DIM));
                                    });
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if !d.authorized {
                                            ui.label(RichText::new("unauthorized").color(WARN));
                                        } else if self.running.contains_key(&d.serial) {
                                            let btn = egui::Button::new(
                                                RichText::new("Stop").color(Color32::WHITE).strong(),
                                            )
                                            .fill(STOP)
                                            .rounding(Rounding::same(8.0))
                                            .min_size(egui::vec2(112.0, 34.0));
                                            if ui.add(btn).clicked() {
                                                self.stop_mirror(&d.serial);
                                            }
                                        } else {
                                            let btn = egui::Button::new(
                                                RichText::new("Start Mirror")
                                                    .color(Color32::WHITE)
                                                    .strong(),
                                            )
                                            .fill(ACCENT)
                                            .rounding(Rounding::same(8.0))
                                            .min_size(egui::vec2(112.0, 34.0));
                                            if ui.add(btn).clicked() {
                                                self.start_mirror(&d.serial);
                                            }
                                        }
                                    },
                                );
                            });
                            if d.authorized {
                                ui.add_space(8.0);
                                let running = self.running.contains_key(&d.serial);
                                let s = self.settings.entry(d.serial.clone()).or_default();
                                ui.add_enabled_ui(!running, |ui| {
                                    ui.horizontal(|ui| {
                                        egui::ComboBox::from_id_salt(("p", d.serial.as_str()))
                                            .selected_text(PRESETS[s.preset])
                                            .width(120.0)
                                            .show_ui(ui, |ui| {
                                                for (i, p) in PRESETS.iter().enumerate() {
                                                    ui.selectable_value(&mut s.preset, i, *p);
                                                }
                                            });
                                        ui.checkbox(&mut s.audio, RichText::new("Audio").color(TEXT));
                                        ui.checkbox(&mut s.input, RichText::new("Input").color(TEXT));
                                    });
                                });
                            }
                        });
                    ui.add_space(8.0);
                }

                if !self.status.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(&self.status).small().color(CYAN));
                }
            });
    }
}

/// A thin cyan→violet accent divider.
fn accent_rule(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 2.0), egui::Sense::hover());
    let painter = ui.painter();
    let steps = 48;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let col = lerp_color(CYAN, ACCENT, (t0 + t1) * 0.5);
        let x0 = rect.left() + rect.width() * t0;
        let x1 = rect.left() + rect.width() * t1;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            0.0,
            col,
        );
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn list_devices() -> Vec<Device> {
    let out = match Command::new(adb_path()).args(["devices", "-l"]).output() {
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
            serial,
            model,
            authorized,
        });
    }
    devices
}
