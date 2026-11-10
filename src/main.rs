#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod excel;
mod transfer;
#[cfg(test)]
mod ui_test;

use eframe::egui::{self, Color32, RichText};
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use transfer::{EXTENSIONS, INDEX, Outcome, part_count, size_limit};

const GREEN: Color32 = Color32::from_rgb(15, 99, 73);
const INK: Color32 = Color32::from_rgb(29, 48, 41);
const MUTED: Color32 = Color32::from_rgb(75, 93, 85);
const PAPER: Color32 = Color32::from_rgb(248, 250, 248);

#[derive(Clone, Copy, PartialEq, Default)]
enum Mode {
    #[default]
    Excel,
    Split,
    Join,
}

struct Selection {
    path: PathBuf,
    name: String,
    bytes: u64,
    parts: Option<usize>,
}

struct Job {
    worker: JoinHandle<io::Result<Outcome>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
}

struct App {
    mode: Mode,
    selection: Option<Selection>,
    dest: Option<PathBuf>,
    size: String,
    unit: u64,
    job: Option<Job>,
    result: Option<Outcome>,
    message: Option<String>,
    closing: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            mode: Mode::Excel,
            selection: None,
            dest: None,
            size: "15".into(),
            unit: 1_000_000,
            job: None,
            result: None,
            message: None,
            closing: false,
        }
    }
}

fn parts_label(n: u64) -> String {
    if n == 1 {
        "1 parte".to_owned()
    } else {
        format!("{n} partes")
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.2} MB", bytes as f64 / 1_000_000.0).replace('.', ",")
}

fn setup(ctx: &egui::Context) {
    let mut style = egui::Style {
        visuals: egui::Visuals::light(),
        ..Default::default()
    };
    style.visuals.override_text_color = Some(INK);
    style.visuals.selection.bg_fill = Color32::from_rgb(186, 222, 203);
    style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1_f32, GREEN);
    style.spacing.item_spacing = egui::vec2(8., 7.);
    style.spacing.button_padding = egui::vec2(12., 7.);
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(15.));
    ctx.set_style(style);
    if let Some(root) = std::env::var_os("WINDIR") {
        let folder = PathBuf::from(root).join("Fonts");
        for font in [
            "Inter-Regular-slnt=0.ttf",
            "Roboto-Regular.ttf",
            "segoeui.ttf",
        ] {
            if let Ok(data) = fs::read(folder.join(font)) {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("interfaz".into(), egui::FontData::from_owned(data).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "interfaz".into());
                ctx.set_fonts(fonts);
                break;
            }
        }
    }
}

impl App {
    fn reset(&mut self, mode: Mode) {
        self.mode = mode;
        self.selection = None;
        self.dest = None;
        self.result = None;
        self.message = None;
    }

    fn select(&mut self, path: PathBuf) {
        self.result = None;
        self.selection = None;
        self.message = None;
        let selection = (|| -> io::Result<Selection> {
            let (name, bytes, parts) = match self.mode {
                Mode::Excel | Mode::Split => {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let ext = path.extension().unwrap_or_default().to_string_lossy();
                    if self.mode == Mode::Excel
                        && !excel::EXTENSIONS
                            .iter()
                            .any(|s| ext.eq_ignore_ascii_case(s))
                    {
                        return Err(io::Error::other(
                            "Para crear libros pequeños, guarda el original como .xlsx en Excel y vuelve a elegirlo.",
                        ));
                    }
                    if !EXTENSIONS.iter().any(|s| ext.eq_ignore_ascii_case(s)) {
                        return Err(io::Error::other(
                            "Elige un libro de Excel, por ejemplo un archivo .xlsx.",
                        ));
                    }
                    (name, fs::metadata(&path)?.len(), None)
                }
                Mode::Join => {
                    let index = transfer::read_index(&path)?;
                    (index.archivo, index.bytes, Some(index.partes.len()))
                }
            };
            Ok(Selection {
                path,
                name,
                bytes,
                parts,
            })
        })();
        match selection {
            Ok(file) => {
                self.dest = file.path.parent().map(Path::to_path_buf);
                self.selection = Some(file);
            }
            Err(e) => {
                self.dest = None;
                self.message = Some(e.to_string());
            }
        }
    }

    fn choose_file(&mut self, frame: &eframe::Frame) {
        let dialog = rfd::FileDialog::new().set_parent(frame);
        let dialog = match self.mode {
            Mode::Excel => dialog
                .set_title("Elegir libro para dividir por filas")
                .add_filter("Libros de Excel", excel::EXTENSIONS),
            Mode::Split => dialog
                .set_title("Seleccionar un libro de Excel")
                .add_filter("Libros de Excel", EXTENSIONS),
            Mode::Join => dialog
                .set_title("Seleccionar Unir.json")
                .add_filter("Índice de las partes", &["json"]),
        };
        if let Some(path) = dialog.pick_file() {
            self.select(path);
        }
    }

    fn start(&mut self, limit: u64) {
        let (Some(file), Some(dest)) = (&self.selection, self.dest.clone()) else {
            return;
        };
        let source = file.path.clone();
        let mode = self.mode;
        self.result = None;
        self.message = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU32::new(0));
        let stop = Arc::clone(&cancel);
        let meter = Arc::clone(&progress);
        let worker = thread::spawn(move || {
            let progress = |done: u64, total: u64| {
                let value = (u128::from(done) * 1000 / u128::from(total.max(1))) as u32;
                meter.store(value, Ordering::Relaxed);
            };
            match mode {
                Mode::Excel => excel::split(&source, &dest, limit, &stop, progress),
                Mode::Split => transfer::split(&source, &dest, limit, &stop, progress),
                Mode::Join => transfer::join(&source, &dest, &stop, progress),
            }
        });
        self.job = Some(Job {
            worker,
            cancel,
            progress,
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(job) = &self.job {
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                job.cancel.store(true, Ordering::Relaxed);
                self.closing = true;
            }
            if job.worker.is_finished() {
                let result = self.job.take().unwrap().worker.join();
                match result {
                    Ok(Ok(outcome)) => self.result = Some(outcome),
                    Ok(Err(err)) => self.message = Some(err.to_string()),
                    Err(_) => self.message = Some("Se interrumpió la operación.".into()),
                }
                if self.closing {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(80));
            }
        }
    }

    fn inputs(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        let split = self.mode != Mode::Join;
        ui.label(
            RichText::new(if split {
                "Archivo de Excel"
            } else {
                "Índice de las partes"
            })
            .strong(),
        );
        ui.horizontal(|ui| {
            if ui
                .button(if split {
                    "Elegir Excel…"
                } else {
                    "Elegir Unir.json…"
                })
                .clicked()
            {
                self.choose_file(frame);
            }
            if let Some(file) = &self.selection {
                ui.add(egui::Label::new(&file.name).truncate())
                    .on_hover_text(file.path.display().to_string());
            } else {
                ui.label(
                    RichText::new(if split {
                        "También puedes arrastrarlo aquí."
                    } else {
                        "Guarda el índice y las partes en la misma carpeta."
                    })
                    .color(MUTED),
                );
            }
        });
        ui.add_space(9.);
        if split {
            ui.label(RichText::new("Tamaño máximo por parte").strong());
            ui.horizontal(|ui| {
                let change = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.size)
                            .desired_width(70.)
                            .hint_text("15"),
                    )
                    .changed();
                let prev = self.unit;
                egui::ComboBox::from_id_salt("unit")
                    .selected_text(if self.unit == 1_000_000 { "MB" } else { "MiB" })
                    .width(70.)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.unit, 1_000_000, "MB");
                        ui.selectable_value(&mut self.unit, 1_048_576, "MiB");
                    });
                if change || prev != self.unit {
                    self.result = None;
                    self.message = None;
                }
                ui.label(
                    RichText::new(if self.unit == 1_000_000 {
                        "1 MB = 1 000 000 bytes"
                    } else {
                        "1 MiB = 1 048 576 bytes"
                    })
                    .size(13.)
                    .color(MUTED),
                );
            });
            ui.add_space(9.);
        }
        ui.label(RichText::new("Carpeta de destino").strong());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.selection.is_some(), egui::Button::new("Cambiar…"))
                .clicked()
            {
                let mut dialog = rfd::FileDialog::new()
                    .set_parent(frame)
                    .set_title("Carpeta de destino");
                if let Some(path) = &self.dest {
                    dialog = dialog.set_directory(path);
                }
                if let Some(path) = dialog.pick_folder() {
                    self.dest = Some(path);
                    self.result = None;
                    self.message = None;
                }
            }
            let label = self
                .dest
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| {
                    if split {
                        "La carpeta del Excel"
                    } else {
                        "La carpeta del índice"
                    }
                    .into()
                });
            ui.add(egui::Label::new(RichText::new(&label).size(13.).color(MUTED)).truncate())
                .on_hover_text(&label);
        });
    }

    fn action(&mut self, ui: &mut egui::Ui) {
        let split = self.mode != Mode::Join;
        let limit = if split {
            size_limit(&self.size, self.unit)
        } else {
            Ok(1)
        };
        let preview = match (&self.selection, &limit) {
            (Some(file), Ok(_)) if self.mode == Mode::Excel => Ok(format!(
                "{} de origen   ·   Salida en .xlsx",
                mb(file.bytes)
            )),
            (Some(file), Ok(limit)) => {
                let count = file
                    .parts
                    .map(|n| Ok(n as u64))
                    .unwrap_or_else(|| part_count(file.bytes, *limit));
                count.map(|n| {
                    format!(
                        "{}   ·   {}{}",
                        mb(file.bytes),
                        parts_label(n),
                        if split {
                            " + Unir.json"
                        } else {
                            " para reconstruir"
                        }
                    )
                })
            }
            (_, Err(e)) => Err(io::Error::other(e.to_string())),
            _ => Ok(String::new()),
        };
        match &preview {
            Ok(text) if !text.is_empty() => {
                ui.label(RichText::new(text).color(GREEN).strong());
            }
            Err(error) => {
                ui.colored_label(Color32::DARK_RED, error.to_string());
            }
            _ => {}
        }
        ui.add_space(3.);
        if let Some(job) = &self.job {
            let value = job.progress.load(Ordering::Relaxed) as f32 / 1000.;
            ui.add(egui::ProgressBar::new(value).show_percentage().fill(GREEN));
            if ui.button("Cancelar").clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
        } else {
            let button = egui::Button::new(
                RichText::new(match self.mode {
                    Mode::Excel => "Crear archivos .xlsx",
                    Mode::Split => "Dividir archivo",
                    Mode::Join => "Reconstruir Excel",
                })
                .color(Color32::WHITE)
                .strong(),
            )
            .fill(GREEN)
            .min_size(egui::vec2(170., 38.))
            .corner_radius(6);
            if ui
                .add_enabled(self.selection.is_some() && preview.is_ok(), button)
                .clicked()
            {
                self.start(limit.unwrap_or(1));
            }
        }
        if let Some(msg) = &self.message {
            ui.colored_label(Color32::from_rgb(154, 50, 34), msg);
        }
        if let Some(outcome) = &self.result {
            ui.add_space(5.);
            let message = if self.mode == Mode::Excel {
                format!(
                    "Listo. Puedes abrir y enviar cada .xlsx por separado ({}).",
                    parts_label(outcome.parts)
                )
            } else if split {
                format!(
                    "Listo. Envía los archivos .bin ({}) junto con {INDEX}.",
                    parts_label(outcome.parts)
                )
            } else {
                "Excel reconstruido. La huella SHA-256 coincide con la del original.".into()
            };
            ui.label(RichText::new(message).color(GREEN).strong());
            ui.label(RichText::new(outcome.path.display().to_string()).size(13.));
            if ui.button("Abrir carpeta de resultado").clicked() {
                let folder = if split {
                    outcome.path.as_path()
                } else {
                    outcome.path.parent().unwrap()
                };
                if let Err(e) = std::process::Command::new("explorer.exe")
                    .arg(folder)
                    .spawn()
                {
                    self.message = Some(e.to_string());
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self.job.is_none() {
            if let Some(path) =
                ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()))
            {
                self.reset(
                    if path
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                    {
                        Mode::Join
                    } else if self.mode == Mode::Split {
                        Mode::Split
                    } else {
                        Mode::Excel
                    },
                );
                self.select(path);
            }
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(PAPER).inner_margin(24))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.heading(
                            RichText::new("Partir Excel")
                                .size(32.)
                                .strong()
                                .color(GREEN),
                        );
                        ui.add_space(5.);
                        ui.add_enabled_ui(self.job.is_none(), |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for (mode, name) in [(Mode::Excel, "Dividir en Excel"), (Mode::Split, "Crear .bin"), (Mode::Join, "Unir .bin")]
                                {
                                    let active = self.mode == mode;
                                    let fill = if active { INK } else { Color32::WHITE };
                                    let text = RichText::new(name).color(if active {
                                        Color32::WHITE
                                    } else {
                                        INK
                                    });
                                    if ui
                                        .add(
                                            egui::Button::new(text)
                                                .min_size(egui::vec2(105., 33.))
                                                .fill(fill)
                                                .corner_radius(6),
                                        )
                                        .clicked()
                                        && !active
                                    {
                                        self.reset(mode);
                                    }
                                }
                            });
                        });
                        ui.add_space(6.);
                        ui.label(
                            RichText::new(match self.mode {
                                Mode::Excel => "Distribuye las filas de cada hoja visible en libros .xlsx. Cada archivo repite el encabezado y muestra las fórmulas como valores guardados.",
                                Mode::Split => "Divide el libro en archivos .bin para reconstruirlo después.",
                                Mode::Join => "Reconstruye el libro original a partir de las partes recibidas.",
                            })
                            .color(MUTED),
                        );
                        ui.add_space(9.);
                        egui::Frame::new()
                            .fill(Color32::WHITE)
                            .stroke(egui::Stroke::new(1_f32, Color32::from_rgb(211, 222, 215)))
                            .inner_margin(17)
                            .corner_radius(9)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.add_enabled_ui(self.job.is_none(), |ui| self.inputs(ui, frame));
                            });
                        ui.add_space(9.);
                        self.action(ui);
                    });
            });
    }
}

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([640., 610.])
            .with_min_inner_size([460., 420.]),
        ..Default::default()
    };
    if let Err(err) = eframe::run_native(
        "Partir Excel",
        options,
        Box::new(|cc| {
            setup(&cc.egui_ctx);
            Ok(Box::new(App::default()))
        }),
    ) {
        rfd::MessageDialog::new()
            .set_title("Partir Excel")
            .set_level(rfd::MessageLevel::Error)
            .set_description(format!("No se pudo iniciar la ventana. {err}"))
            .show();
    }
}
