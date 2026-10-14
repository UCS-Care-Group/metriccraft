//! OpenDash BI desktop app: the engine on its worker thread, the egui window on eframe.
//!
//! `opendash [PROJECT]` opens a Project folder or `.odash` Package.

use std::path::PathBuf;

use opendash_engine::App;
use opendash_ui_egui::strings::Str;
use opendash_ui_egui::worker::Engine;
use opendash_ui_egui::{FileAsk, FileHook, Prefs, Shell};
use serde_json::json;
use std::sync::mpsc::{Receiver, Sender, channel};

/// The per-user preferences file (ADR 0008): `$XDG_CONFIG_HOME/opendash/prefs.json` (or
/// `~/.config/…`) on Linux, `%APPDATA%\opendash` on Windows, `~/Library/Application Support/…` on
/// macOS.
fn prefs_path() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    #[cfg(windows)]
    let base = var("APPDATA");
    #[cfg(target_os = "macos")]
    let base = var("HOME").map(|h| h.join("Library").join("Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")));
    Some(base?.join("opendash").join("prefs.json"))
}

fn load_prefs() -> Prefs {
    prefs_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .map_or_else(Prefs::default, |v| Prefs::from_json(&v))
}

fn save_prefs(prefs: Prefs) {
    let Some(path) = prefs_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, prefs.to_json().to_string());
}

/// File dialogs the shell asked for, shown from the frame that owns the window (so they open in
/// front of it) and waited for on a thread, so the window keeps drawing.
struct Dialogs {
    queue: Receiver<(FileAsk, Sender<Option<PathBuf>>)>,
    ctx: egui::Context,
}

fn file_dialogs(ctx: &egui::Context) -> (FileHook, Dialogs) {
    let (tx, queue) = channel();
    let hook: FileHook = Box::new(move |ask| {
        let (answer, rx) = channel();
        // If the window is gone the dropped sender reads as "cancelled".
        let _ = tx.send((ask, answer));
        rx
    });
    (
        hook,
        Dialogs {
            queue,
            ctx: ctx.clone(),
        },
    )
}

impl Dialogs {
    fn show(&self, frame: &eframe::Frame) {
        while let Ok((ask, answer)) = self.queue.try_recv() {
            let filter = Str::OpenDashFiles.text();
            let dialog = rfd::AsyncFileDialog::new().set_parent(frame);
            let picking: std::pin::Pin<
                Box<dyn std::future::Future<Output = Option<rfd::FileHandle>> + Send>,
            > = match ask {
                FileAsk::Open => {
                    Box::pin(dialog.add_filter(filter, &["odash", "json"]).pick_file())
                }
                FileAsk::SaveAs { name } => Box::pin(dialog.set_file_name(name).save_file()),
            };
            let ctx = self.ctx.clone();
            std::thread::spawn(move || {
                let path = pollster::block_on(picking).map(|h| h.path().to_path_buf());
                let _ = answer.send(path);
                ctx.request_repaint();
            });
        }
    }
}

/// The window, the preferences last written, and the file dialogs.
struct Desktop(Shell, Prefs, Dialogs);

impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.0.logic(ctx);
        // Written when they change: `on_exit`'s signature depends on the renderer feature.
        if self.0.prefs() != self.1 {
            self.1 = self.0.prefs();
            save_prefs(self.1);
        }
        let closing = ctx.input(|i| i.viewport().close_requested());
        if closing && !self.0.close_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if self.0.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.0.ui(ui);
        self.2.show(frame);
    }
}

fn main() -> eframe::Result {
    let open = std::env::args_os().nth(1).map(PathBuf::from);
    let prefs = load_prefs();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(Str::AppName.text())
            .with_app_id("org.opendash.OpenDash")
            .with_inner_size([1400.0, 900.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        Str::AppName.text(),
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let mut engine = Engine::new(App::new(), move || ctx.request_repaint());
            if let Some(path) = open {
                engine.run("file.open", json!({"path": path.display().to_string()}));
            }
            let (hook, dialogs) = file_dialogs(&cc.egui_ctx);
            let shell = Shell::new(engine, prefs, Engine::recoverable()).with_file_dialogs(hook);
            shell.setup(&cc.egui_ctx);
            Ok(Box::new(Desktop(shell, prefs, dialogs)))
        }),
    )
}
