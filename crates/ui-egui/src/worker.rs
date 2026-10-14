//! The engine behind the window (ADR 0008): one worker thread owns the [`App`]; the UI sends it
//! commands and reads only the [`Snapshot`] it publishes after each one, so a long Get Data or
//! Refresh never freezes the window. On wasm (no threads, spec N-5) the same interface runs
//! commands inline.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use opendash_engine::{App, Command, Document, Location, RecoveryInfo};
use opendash_visuals::Scene;
use opendash_visuals::layout::Context;
use serde_json::Value;

/// What the window shows of the engine: a copy taken after the last command.
#[derive(Clone, Default)]
pub struct Snapshot {
    pub doc: Document,
    /// Whether each command (in [`opendash_engine::COMMANDS`] order) can run now.
    pub enabled: Vec<bool>,
    pub dirty: bool,
    pub location: Option<Location>,
    /// The command running now, if any.
    pub busy: Option<&'static str>,
    /// The current Page's id and its Visuals' Scenes, drawn after the last command.
    pub scenes: Arc<(String, Vec<(String, Scene)>)>,
    /// Bumped on every publish.
    pub seq: u64,
}

impl Snapshot {
    fn of(app: &mut App, seq: u64) -> Self {
        Snapshot {
            doc: app.document().clone(),
            enabled: app.commands().iter().map(|c| (c.enabled)(app)).collect(),
            dirty: app.is_dirty(),
            location: app.location().cloned(),
            busy: None,
            scenes: Arc::new(draw(app)),
            seq,
        }
    }

    /// Whether command `id` can run now.
    pub fn enabled(&self, commands: &[Command], id: &str) -> bool {
        commands
            .iter()
            .position(|c| c.id == id)
            .and_then(|i| self.enabled.get(i).copied())
            .unwrap_or(false)
    }
}

/// The current Page drawn with the default Report theme.
// ponytail: en-US formatting until Projects carry a Culture (spec R-4).
fn draw(app: &mut App) -> (String, Vec<(String, Scene)>) {
    let Some(page) = crate::views::current_page(app.document()).map(|p| p.id.clone()) else {
        return Default::default();
    };
    let theme = opendash_visuals::Theme::light();
    let cx = Context {
        theme: &theme,
        locale: &opendash_core::EN_US,
    };
    let scenes = app.page_scene(&page, &cx).unwrap_or_default();
    (page, scenes)
}

/// How a command ended, for the toasts and status bar.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// It ran; with the engine's notice, if any.
    Done {
        id: String,
        notice: Option<String>,
    },
    Failed {
        id: String,
        error: String,
    },
}

enum Job {
    Run(String, Value),
    /// A click in a Visual at its logical `x`, `y` (`App::click`).
    Click {
        page: String,
        visual: String,
        x: f32,
        y: f32,
        add: bool,
    },
    /// Writes a recovery copy if autosave is due (`now`: ms since 1970).
    Autosave(u64),
}

struct Shared {
    snap: Arc<Snapshot>,
    events: Vec<Event>,
}

/// Locks `m`, carrying on past a poisoned lock (the worker never panics; if it did, the last
/// snapshot is still the best thing to show).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn run(app: &mut App, shared: &Mutex<Shared>, seq: &mut u64, job: Job) {
    let event = match job {
        Job::Run(id, params) => match app.run(&id, &params) {
            Ok(()) => Event::Done {
                notice: app.notice().map(str::to_string),
                id,
            },
            Err(e) => Event::Failed {
                id,
                error: e.to_string(),
            },
        },
        Job::Click {
            page,
            visual,
            x,
            y,
            add,
        } => match app.click(&page, &visual, x, y, add) {
            Ok(()) => Event::Done {
                id: "selection.select".into(),
                notice: None,
            },
            Err(e) => Event::Failed {
                id: "selection.select".into(),
                error: e.to_string(),
            },
        },
        Job::Autosave(now) => {
            if app.autosave_due(now)
                && let Some(dir) = opendash_engine::recovery_dir()
                && let Err(e) = app.write_recovery(&dir, now)
            {
                Event::Failed {
                    id: "autosave".into(),
                    error: e.to_string(),
                }
            } else {
                return;
            }
        }
    };
    *seq = seq.wrapping_add(1);
    let mut s = lock(shared);
    s.snap = Arc::new(Snapshot::of(app, *seq));
    s.events.push(event);
}

/// The UI's handle on the engine.
pub struct Engine {
    shared: Arc<Mutex<Shared>>,
    commands: &'static [Command],
    #[cfg(not(target_arch = "wasm32"))]
    jobs: std::sync::mpsc::Sender<Job>,
    #[cfg(target_arch = "wasm32")]
    app: App,
    #[cfg(target_arch = "wasm32")]
    seq: u64,
}

impl Engine {
    /// Starts the engine on `app`; `repaint` wakes the window when a command finishes.
    pub fn new(mut app: App, repaint: impl Fn() + Send + 'static) -> Self {
        let commands = app.commands();
        let shared = Arc::new(Mutex::new(Shared {
            snap: Arc::new(Snapshot::of(&mut app, 0)),
            events: Vec::new(),
        }));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (jobs, rx) = std::sync::mpsc::channel::<Job>();
            let worker = Arc::clone(&shared);
            // ponytail: an unnamed detached thread; it ends when the window drops `jobs`.
            std::thread::spawn(move || {
                let mut seq = 0;
                while let Ok(job) = rx.recv() {
                    if let Job::Run(id, _) = &job {
                        let busy = app.command(id).map(|c| c.id);
                        let mut s = lock(&worker);
                        let mut snap = (*s.snap).clone();
                        snap.busy = busy;
                        s.snap = Arc::new(snap);
                        drop(s);
                        repaint();
                    }
                    run(&mut app, &worker, &mut seq, job);
                    repaint();
                }
                // The window closed: remove this session's recovery copy.
                let _ = app.close();
            });
            Engine {
                shared,
                commands,
                jobs,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = repaint;
            Engine {
                shared,
                commands,
                app,
                seq: 0,
            }
        }
    }

    /// Queues command `id` with `params`.
    pub fn run(&mut self, id: &str, params: Value) {
        let job = Job::Run(id.into(), params);
        #[cfg(not(target_arch = "wasm32"))]
        if self.jobs.send(job).is_err() {
            lock(&self.shared).events.push(Event::Failed {
                id: id.into(),
                error: "the engine stopped".into(),
            });
        }
        #[cfg(target_arch = "wasm32")]
        run(&mut self.app, &self.shared, &mut self.seq, job);
    }

    /// A click in Visual `visual` at its logical `x`, `y`; `add` is Ctrl (cross-filter).
    pub fn click(&mut self, page: &str, visual: &str, x: f32, y: f32, add: bool) {
        let job = Job::Click {
            page: page.into(),
            visual: visual.into(),
            x,
            y,
            add,
        };
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self.jobs.send(job);
        #[cfg(target_arch = "wasm32")]
        run(&mut self.app, &self.shared, &mut self.seq, job);
    }

    /// Asks for a recovery copy if one is due.
    pub fn autosave(&mut self, now_ms: u64) {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = self.jobs.send(Job::Autosave(now_ms));
        #[cfg(target_arch = "wasm32")]
        run(
            &mut self.app,
            &self.shared,
            &mut self.seq,
            Job::Autosave(now_ms),
        );
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&lock(&self.shared).snap)
    }

    /// The events since the last call.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut lock(&self.shared).events)
    }

    pub fn commands(&self) -> &'static [Command] {
        self.commands
    }

    /// Recovery copies left by sessions that didn't close (none on the web).
    pub fn recoverable() -> (Option<PathBuf>, Vec<RecoveryInfo>) {
        let dir = opendash_engine::recovery_dir();
        let list = dir.as_deref().map(App::recoverable).unwrap_or_default();
        (dir, list)
    }
}
