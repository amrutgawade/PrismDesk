//! PrismDesk dashboard — the control plane (egui). Lists connected devices and
//! their quality settings, and launches the native mirror as a child process
//! (`pd-engine --mirror ...`) so the UI stays responsive and each mirror is
//! isolated.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use eframe::egui;

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
        Box::new(|_cc| Ok(Box::new(Dashboard::new()))),
    )
}

struct Dashboard {
    devices: Vec<Device>,
    last_refresh: Instant,
    preset: usize,
    audio: bool,
    status: String,
}

impl Dashboard {
    fn new() -> Self {
        let mut d = Self {
            devices: Vec::new(),
            last_refresh: Instant::now() - Duration::from_secs(10),
            preset: 0,
            audio: true,
            status: String::new(),
        };
        d.refresh();
        d
    }

    fn refresh(&mut self) {
        self.devices = list_devices();
    }

    fn start_mirror(&mut self, serial: &str) {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("pd-engine"));
        let mut cmd = Command::new(exe);
        cmd.arg("--mirror")
            .arg("--serial")
            .arg(serial)
            .arg("--preset")
            .arg(PRESETS[self.preset]);
        if !self.audio {
            cmd.arg("--no-audio");
        }
        match cmd.spawn() {
            Ok(_) => self.status = format!("Started mirror for {serial}"),
            Err(e) => self.status = format!("Failed to start: {e}"),
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

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(6.0);
            ui.heading("PrismDesk");
            ui.label("Android screen mirroring");
            ui.separator();

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("Quality:");
                egui::ComboBox::from_id_salt("preset")
                    .selected_text(PRESETS[self.preset])
                    .show_ui(ui, |ui| {
                        for (i, p) in PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut self.preset, i, *p);
                        }
                    });
                ui.checkbox(&mut self.audio, "Audio");
            });

            ui.add_space(10.0);
            ui.label("Devices");
            ui.add_space(4.0);

            if self.devices.is_empty() {
                ui.weak("No device found. Connect via USB with debugging enabled.");
            }

            let devices = self.devices.clone();
            for d in &devices {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.strong(&d.model);
                            ui.weak(&d.serial);
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if d.authorized {
                                if ui.button("Start Mirror").clicked() {
                                    self.start_mirror(&d.serial);
                                }
                            } else {
                                ui.weak("unauthorized");
                            }
                        });
                    });
                });
            }

            if !self.status.is_empty() {
                ui.add_space(8.0);
                ui.separator();
                ui.weak(&self.status);
            }
        });
    }
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
