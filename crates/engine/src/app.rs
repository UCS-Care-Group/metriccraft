use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use metriccraft_data::{Preview, Query, Table, default_memory_cap};
use metriccraft_model::Model;
use serde_json::Value;

use crate::{COMMANDS, Command, Error, LoadReport, Result, Run};

/// Most commands in one Batch (hostile input: a Batch is a command's params).
pub const MAX_BATCH: usize = 10_000;

/// Who ran a command (glossary: Origin), shown on undo labels and in the status bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Ui,
    Control,
    Cli,
    Mcp,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ui => "UI",
            Self::Control => "Control channel",
            Self::Cli => "CLI",
            Self::Mcp => "MCP",
        }
    }

    /// An undo label: the command's own label from the UI, prefixed with the Origin otherwise
    /// (`MCP — Rename column`).
    fn label(self, what: &str) -> String {
        match self {
            Self::Ui => what.into(),
            other => format!("{} — {what}", other.name()),
        }
    }
}

/// An undo or redo entry: the Document to go back to and what the change was.
#[derive(Clone)]
struct Step {
    doc: Document,
    label: String,
}

/// Everything that is saved and undoable. Filled in by later milestones (model, report).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// Loaded data. Columns are shared, never copied, so cloning a Document for undo is cheap.
    pub tables: Vec<Table>,
    /// Queries; each one's result is the table of the same name in `tables`.
    pub queries: Vec<Query>,
    /// Queries edited since their table loaded (Stale, glossary): the table is still the
    /// previous result until Close & Apply or Refresh.
    pub stale: BTreeSet<String>,
    /// Queries with *Enable load* off: no Table, but they can still be merged from.
    pub unloaded: BTreeSet<String>,
    /// The model properties of the loaded Tables (one per Table, same name).
    pub model: Model,
    /// How each Table's data was loaded (its Query's M hash, Source fingerprints and time), by
    /// Table name: the Data cache's metadata (`metriccraft_file::cache`), undone with the data.
    pub provenance: BTreeMap<String, metriccraft_file::cache::Provenance>,
    /// The Calculated columns whose DAX failed when last computed (they hold BLANKs), in Model
    /// order: derived from the Model and the data (`expression.rs`), never saved.
    pub formula_errors: Vec<metriccraft_model::Error>,
    /// The report: Pages of Visuals (`report.rs`).
    pub report: crate::report::Report,
    /// Model view layout: where each Table's box sits (`diagram.rs`).
    pub diagram: crate::diagram::Diagram,
    /// Lets unit tests observe edits before the document has real content.
    #[cfg(test)]
    pub(crate) probe: i64,
}

/// Undo steps kept; the oldest is dropped beyond this.
pub const UNDO_LIMIT: usize = 100;

/// The running app: the document, its undo history and the command set.
pub struct App {
    pub(crate) doc: Document,
    commands: &'static [Command],
    // ponytail: whole-Document snapshots — cheap because table columns are shared.
    // Switch to inverse operations if cloning a Document shows up in benchmarks.
    undo: VecDeque<Step>,
    redo: Vec<Step>,
    /// The undo label of the command running now (see [`Origin::label`]).
    label: String,
    /// Inside a Batch: edits record no undo step of their own; the Batch records one.
    batching: bool,
    /// What the last Get Data or Refresh loaded (not part of the Document: not undone).
    last_load: Option<LoadReport>,
    /// The memory cap for loaded data, counted over the document, undo and redo.
    memory_cap: usize,
    /// A warning from the last command, which still succeeded (e.g. a Relationship created
    /// inactive). Not part of the Document.
    notice: Option<String>,
    /// Where the document was opened from or last saved to, and what its files held beyond it
    /// (`file.rs`). Not part of the Document: not undone.
    pub(crate) file: crate::file::FileState,
    /// Bumped on every change to the Document (edit, load, undo, redo, open): what dirty
    /// tracking compares (`recovery.rs`).
    pub(crate) version: u64,
    /// `version` when the Document last matched its files (opened, new or saved); `None` after a
    /// recovery, which must be saved.
    pub(crate) clean: Option<u64>,
    /// Autosave recovery (`recovery.rs`): the interval option and this app's recovery copy.
    pub(crate) autosave: crate::recovery::Autosave,
    /// Session-only report state (`selection.rs`): data-point Selections, the visual-query
    /// cache and the last drawn Scenes. Not part of the Document: not saved, not undone.
    pub(crate) session: crate::selection::Session,
    /// Stops a running Page drawing (`App::cancel_flag`).
    pub(crate) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Rows read and the cancel switch of the load running (Get Data, Refresh, Close & Apply).
    pub(crate) progress: std::sync::Arc<metriccraft_data::Progress>,
    /// What the last command moved, for `merge` (see [`merge_key`]).
    last_move: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::with_commands(COMMANDS)
    }
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// An app with a custom command set (tests, embedding).
    pub fn with_commands(commands: &'static [Command]) -> Self {
        Self {
            doc: Document::default(),
            commands,
            undo: VecDeque::new(),
            redo: Vec::new(),
            label: String::new(),
            batching: false,
            last_load: None,
            memory_cap: default_memory_cap(),
            notice: None,
            file: crate::file::FileState::default(),
            version: 0,
            clean: Some(0),
            autosave: crate::recovery::Autosave::default(),
            session: crate::selection::Session::default(),
            cancel: std::sync::Arc::default(),
            progress: std::sync::Arc::default(),
            last_move: None,
        }
    }

    /// Replaces the document (File > Open, File > New): not undoable, as in Power BI; clears
    /// the undo and redo history and the last load report.
    pub(crate) fn replace(&mut self, doc: Document) {
        self.doc = doc;
        self.undo.clear();
        self.redo.clear();
        self.last_load = None;
        self.session.forget_selections();
        self.changed();
        self.clean = Some(self.version);
    }

    /// The Document changed (see `version`): stale session state goes too.
    fn changed(&mut self) {
        self.version = self.version.wrapping_add(1);
        self.session.prune(&self.doc);
    }

    /// Whether the Document has changed since it was opened or last saved (a recovered one is
    /// dirty until saved). Undoing back to the saved state still counts as a change.
    pub fn is_dirty(&self) -> bool {
        self.clean != Some(self.version)
    }

    /// Applies `f` to the Document and every undo and redo step, recording nothing: for changes
    /// to the environment rather than the Document (a remapped source path), which undo must
    /// not bring back.
    pub(crate) fn every_document(&mut self, mut f: impl FnMut(&mut Document)) {
        f(&mut self.doc);
        self.undo.iter_mut().for_each(|s| f(&mut s.doc));
        self.redo.iter_mut().for_each(|s| f(&mut s.doc));
    }

    pub fn document(&self) -> &Document {
        &self.doc
    }

    /// Starts from `doc` (tests, until files and Get Data exist), with an empty undo history.
    pub fn with_document(doc: Document) -> Self {
        Self {
            doc,
            ..Self::default()
        }
    }

    /// The memory cap for loaded data: half of physical RAM unless set.
    pub fn memory_cap(&self) -> usize {
        self.memory_cap
    }

    /// Sets the memory cap for loaded data; checked by the next load.
    pub fn set_memory_cap(&mut self, bytes: usize) {
        self.memory_cap = bytes;
    }

    /// Bytes of column data held: every distinct buffer of the document, undo and redo
    /// (snapshots share buffers, so each is counted once). What the memory cap is checked against.
    pub fn memory(&self) -> usize {
        Held::new(steps(&self.undo, &self.redo).chain([&self.doc])).total
    }

    pub fn commands(&self) -> &'static [Command] {
        self.commands
    }

    pub fn command(&self, id: &str) -> Option<&'static Command> {
        // ponytail: linear scan; `==` rejects on length first, ~1 ns per command (bench
        // engine/dispatch_unknown). Binary search measured slower at 36 commands (shared
        // `modeling.` prefixes); use a HashMap once the ribbon has hundreds.
        self.commands.iter().find(|c| c.id == id)
    }

    /// Runs a command by id from the UI: [`App::run_as`] with [`Origin::Ui`].
    pub fn run(&mut self, id: &str, params: &Value) -> Result<Value> {
        self.run_as(Origin::Ui, id, params)
    }

    /// Runs a command by id. The single entry point for UI, Control channel, CLI and MCP.
    /// Returns a Read command's result, a Batch's results, and `null` for everything else.
    pub fn run_as(&mut self, origin: Origin, id: &str, params: &Value) -> Result<Value> {
        let cmd = self
            .command(id)
            .ok_or_else(|| Error::UnknownCommand(id.into()))?;
        self.notice = None;
        if !(cmd.enabled)(self) {
            return Err(Error::Disabled(id.into()));
        }
        crate::param::validate(cmd.params, params).map_err(|message| Error::InvalidParams {
            command: id.into(),
            message,
        })?;
        match cmd.run {
            Run::Read(read) => return read(self, params),
            Run::Batch => return self.batch(origin, params),
            Run::Edit(_) | Run::App(_) => {}
        }
        self.label = origin.label(cmd.label);
        let moved = merge_key(id, params);
        let merge = moved.is_some()
            && moved == self.last_move
            && params.get("merge").and_then(Value::as_bool) == Some(true);
        self.last_move = None;
        let undo_before = self.undo.len();
        match cmd.run {
            Run::Edit(edit) => self.edit_noting(|doc| edit(doc, params).map(|()| None))?,
            Run::App(act) => act(self, params)?,
            Run::Read(_) | Run::Batch => {}
        }
        // A merged move replaces the previous one in the history: undo goes back to before both.
        if merge && self.undo.len() > undo_before {
            self.undo.pop_back();
        }
        self.last_move = moved;
        Ok(Value::Null)
    }

    /// Runs a Batch (glossary): `commands`, a list of `{"run": id, "params": …}`, all or
    /// nothing, as one undo step labelled `label` (default: how many changes). Fails before
    /// running anything on a step that can't be batched; a failing step undoes the ones before it.
    fn batch(&mut self, origin: Origin, params: &Value) -> Result<Value> {
        let invalid = |message: String| Error::InvalidParams {
            command: "batch".into(),
            message,
        };
        let list = params
            .get("commands")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("`commands` must be a list of {\"run\", \"params\"}".into()))?;
        if list.len() > MAX_BATCH {
            return Err(invalid(format!("more than {MAX_BATCH} commands")));
        }
        let mut steps = Vec::with_capacity(list.len());
        for (i, step) in list.iter().enumerate() {
            let id = step
                .get("run")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("command {}: `run` must be a command id", i + 1)))?;
            let cmd = self
                .command(id)
                .ok_or_else(|| invalid(format!("command {}: unknown command `{id}`", i + 1)))?;
            // These replace the document, write files or move through the undo history.
            if matches!(cmd.run, Run::Batch) || id.starts_with("file.") || id.starts_with("edit.") {
                return Err(invalid(format!(
                    "command {}: `{id}` can't run in a batch",
                    i + 1
                )));
            }
            steps.push((cmd, step.get("params").unwrap_or(&Value::Null)));
        }
        let before = self.doc.clone();
        let saved = (
            self.undo.clone(),
            self.redo.clone(),
            self.last_load.clone(),
            self.version,
            self.clean,
            self.last_move.clone(),
        );
        self.batching = true;
        let mut results = Vec::with_capacity(steps.len());
        let mut edits = 0usize;
        for (i, (cmd, p)) in steps.into_iter().enumerate() {
            match self.run_as(origin, cmd.id, p) {
                Ok(v) => {
                    if !matches!(cmd.run, Run::Read(_)) {
                        edits = edits.saturating_add(1);
                    }
                    results.push(v);
                }
                Err(e) => {
                    self.batching = false;
                    self.doc = before;
                    (
                        self.undo,
                        self.redo,
                        self.last_load,
                        self.version,
                        self.clean,
                        self.last_move,
                    ) = saved;
                    self.session.prune(&self.doc);
                    return Err(Error::Batch {
                        index: i,
                        command: cmd.id.into(),
                        error: Box::new(e),
                    });
                }
            }
        }
        self.batching = false;
        self.last_move = None;
        if self.version != saved.3 {
            let what = match params.get("label").and_then(Value::as_str) {
                Some(label) if !label.is_empty() => label.to_string(),
                _ if edits == 1 => "1 change".into(),
                _ => format!("{edits} changes"),
            };
            self.label = origin.label(&what);
            self.push_undo(before);
        }
        Ok(Value::Array(results))
    }

    /// What Undo would undo (`MCP — 30 changes`), if anything.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.back().map(|s| s.label.as_str())
    }

    /// What Redo would redo, if anything.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Runs `edit` like a [`Run::Edit`] (on a copy, recorded for undo); its notice, if any,
    /// becomes [`App::notice`].
    pub(crate) fn edit_noting(
        &mut self,
        edit: impl FnOnce(&mut Document) -> Result<Option<String>>,
    ) -> Result {
        let mut next = self.doc.clone();
        let notice = edit(&mut next)?;
        // A Model change (a formula, a Relationship, a rename, a type) may change what the
        // Calculated columns hold: they are computed again (`expression.rs`).
        if next.model != self.doc.model {
            let limit = self.memory_cap.saturating_sub(self.memory());
            crate::expression::recompute(&mut next, limit)?;
        }
        let prev = std::mem::replace(&mut self.doc, next);
        self.push_undo(prev);
        self.redo.clear();
        self.notice = notice;
        self.changed();
        Ok(())
    }

    /// Sets the notice of a command that succeeded without an edit.
    pub(crate) fn note(&mut self, notice: &str) {
        self.notice = Some(notice.into());
    }

    /// A warning from the last command, which still succeeded: e.g. a Relationship created
    /// inactive because its pair already has an active one, or what auto-detect found.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Runs a loading edit (Get Data, Refresh, Close & Apply) like a [`Run::Edit`]: on a copy
    /// that replaces the document only on success, recorded for undo. Keeps its report.
    ///
    /// `edit` gets the bytes it may load: the cap less everything held (see [`App::memory`]).
    /// To make room, the oldest undo steps are dropped while they hold data the document and
    /// redo don't: first until `estimate` (the new data's expected size, e.g. the tables it
    /// replaces) fits, then, if the load still hits the cap, all of them and it runs once more.
    /// Dropped steps stay dropped even if the load then fails; the report counts them.
    pub(crate) fn edit_loading(
        &mut self,
        estimate: usize,
        edit: impl Fn(&mut Document, usize) -> Result<LoadReport>,
    ) -> Result {
        let mut held = Held::new(steps(&self.undo, &self.redo).chain([&self.doc]));
        // What dropping every undo step would leave.
        let base = Held::new(self.redo.iter().map(|s| &s.doc).chain([&self.doc])).total;
        let mut dropped = 0usize;
        let mut room = |undo: &mut VecDeque<Step>, held: &mut Held, need: usize| {
            while held.total.saturating_add(need) > self.memory_cap && held.total > base {
                let Some(oldest) = undo.pop_front() else {
                    break;
                };
                held.remove(&oldest.doc);
                dropped = dropped.saturating_add(1);
            }
        };
        room(&mut self.undo, &mut held, estimate);
        let report = loop {
            let mut next = self.doc.clone();
            match edit(&mut next, self.memory_cap.saturating_sub(held.total)) {
                Ok(report) => break (next, report),
                Err(e) if is_memory_cap(&e) && held.total > base => {
                    room(&mut self.undo, &mut held, usize::MAX);
                }
                Err(e) => return Err(e),
            }
        };
        let (next, mut report) = report;
        report.undo_dropped = dropped;
        let prev = std::mem::replace(&mut self.doc, next);
        self.push_undo(prev);
        self.redo.clear();
        self.last_load = Some(report);
        self.changed();
        Ok(())
    }

    /// What the last Get Data or Refresh loaded: rows and Cell error counts per table. Cleared
    /// by undo and redo (the report describes a load that is no longer the current state).
    pub fn last_load(&self) -> Option<&LoadReport> {
        self.last_load.as_ref()
    }

    /// The Preview of `query` at Applied Step `step` (0 = first), or of its Source for `None`:
    /// the first 1,000 Source rows, with Cell errors per cell. Derived, not stored.
    pub fn preview(&self, query: &str, step: Option<usize>) -> Result<Preview> {
        crate::load::preview(&self.doc, query, step, self.memory_cap)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn undo(&mut self) -> Result {
        let prev = self
            .undo
            .pop_back()
            .ok_or_else(|| Error::Disabled("edit.undo".into()))?;
        let current = self.doc.report.current.clone();
        let doc = std::mem::replace(&mut self.doc, prev.doc);
        self.redo.push(Step {
            doc,
            label: prev.label,
        });
        self.keep_current(current);
        self.last_load = None;
        self.changed();
        Ok(())
    }

    pub(crate) fn redo(&mut self) -> Result {
        let next = self
            .redo
            .pop()
            .ok_or_else(|| Error::Disabled("edit.redo".into()))?;
        let current = self.doc.report.current.clone();
        let prev = std::mem::replace(&mut self.doc, next.doc);
        self.label = next.label;
        self.push_undo(prev);
        self.keep_current(current);
        self.last_load = None;
        self.changed();
        Ok(())
    }

    /// Undo and redo don't move the user to another Page: the current Page stays, if it still
    /// exists.
    fn keep_current(&mut self, current: Option<String>) {
        if let Some(id) = current
            && self.doc.report.pages.iter().any(|p| p.id == id)
        {
            self.doc.report.current = Some(id);
        }
    }

    fn push_undo(&mut self, doc: Document) {
        if self.batching {
            return;
        }
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.pop_front();
        }
        let label = self.label.clone();
        self.undo.push_back(Step { doc, label });
    }
}

/// The Documents of the undo and redo steps.
fn steps<'a>(undo: &'a VecDeque<Step>, redo: &'a [Step]) -> impl Iterator<Item = &'a Document> {
    undo.iter().chain(redo).map(|s| &s.doc)
}

/// What a move command moves (`model.moveTable`, `visual.move`, `visual.resize`), so that a
/// following move of the same object with `merge` joins it in one undo step.
fn merge_key(id: &str, params: &Value) -> Option<String> {
    let s = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or_default();
    match id {
        "model.moveTable" => Some(format!("{id}\n{}", metriccraft_model::name_key(s("table")))),
        "visual.move" | "visual.resize" => Some(format!("{id}\n{}\n{}", s("page"), s("visual"))),
        _ => None,
    }
}

/// Whether `e` is a load stopped by the memory cap.
fn is_memory_cap(e: &Error) -> bool {
    matches!(
        e,
        Error::Data(metriccraft_data::Error::MemoryCap { .. })
            | Error::Refresh {
                error: metriccraft_data::Error::MemoryCap { .. },
                ..
            }
    )
}

/// The distinct column buffers held by some Documents: by address, its bytes and how many of
/// the Documents hold it. O(buffers) to build; removing a Document is O(its buffers).
struct Held {
    buffers: HashMap<usize, (usize, usize)>,
    total: usize,
}

impl Held {
    fn new<'a>(docs: impl IntoIterator<Item = &'a Document>) -> Self {
        let mut held = Self {
            buffers: HashMap::new(),
            total: 0,
        };
        for doc in docs {
            for (addr, bytes) in distinct(doc) {
                let entry = held.buffers.entry(addr).or_insert((bytes, 0));
                if entry.1 == 0 {
                    held.total = held.total.saturating_add(bytes);
                }
                entry.1 = entry.1.saturating_add(1);
            }
        }
        held
    }

    /// `doc` is no longer held: frees the buffers only it held.
    fn remove(&mut self, doc: &Document) {
        for (addr, _) in distinct(doc) {
            if let Some(entry) = self.buffers.get_mut(&addr) {
                entry.1 = entry.1.saturating_sub(1);
                if entry.1 == 0 {
                    self.total = self.total.saturating_sub(entry.0);
                    self.buffers.remove(&addr);
                }
            }
        }
    }
}

/// `doc`'s column buffers, each once.
fn distinct(doc: &Document) -> HashMap<usize, usize> {
    doc.tables.iter().flat_map(Table::buffers).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn add(doc: &mut Document, params: &Value) -> Result {
        let by = params["by"].as_i64().ok_or_else(|| Error::InvalidParams {
            command: "test.add".into(),
            message: "`by` must be an integer".into(),
        })?;
        doc.probe = doc
            .probe
            .checked_add(by)
            .ok_or_else(|| Error::InvalidParams {
                command: "test.add".into(),
                message: "overflow".into(),
            })?;
        Ok(())
    }

    static TEST_COMMANDS: &[Command] = &[
        COMMANDS[0],
        COMMANDS[1],
        Command {
            id: "test.add",
            label: "Add",
            ribbon: "",
            shortcut: None,
            params: &[crate::param::req(
                "by",
                crate::param::Kind::Integer,
                "amount to add",
            )],
            enabled: |_| true,
            run: Run::Edit(add),
        },
    ];

    fn app() -> App {
        App::with_commands(TEST_COMMANDS)
    }

    fn probe(app: &App) -> i64 {
        app.document().probe
    }

    #[test]
    fn every_command_survives_empty_params() {
        // Each command against an app that has it: the real set, then the test set.
        let real = COMMANDS.iter().map(|c| (c, App::new as fn() -> App));
        let test = TEST_COMMANDS.iter().map(|c| (c, app as fn() -> App));
        for (cmd, make) in real.chain(test) {
            for params in [Value::Null, json!({})] {
                let mut app = make();
                assert!(app.command(cmd.id).is_some(), "{} is known", cmd.id);
                let before = app.document().clone();
                // Ok or Err are both fine; a panic fails the test. An error must change nothing.
                if app.run(cmd.id, &params).is_err() {
                    assert_eq!(
                        app.document(),
                        &before,
                        "{} changed the doc on error",
                        cmd.id
                    );
                }
            }
        }
    }

    #[test]
    fn command_ids_are_unique() {
        let mut ids: Vec<_> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), COMMANDS.len());
    }

    #[test]
    fn undo_and_redo_round_trip() {
        let mut app = app();
        app.run("test.add", &json!({"by": 2})).unwrap();
        app.run("test.add", &json!({"by": 3})).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(probe(&app), 2);
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(probe(&app), 0);
        app.run("edit.redo", &Value::Null).unwrap();
        app.run("edit.redo", &Value::Null).unwrap();
        assert_eq!(probe(&app), 5);
    }

    #[test]
    fn failed_edit_changes_nothing() {
        let mut app = app();
        app.run("test.add", &json!({"by": i64::MAX})).unwrap();
        assert!(app.run("test.add", &json!({"by": 1})).is_err());
        assert_eq!(probe(&app), i64::MAX);
        app.run("edit.undo", &Value::Null).unwrap();
        assert!(!app.can_undo(), "failed edit must not add an undo step");
    }

    #[test]
    fn new_edit_clears_redo() {
        let mut app = app();
        app.run("test.add", &json!({"by": 1})).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        app.run("test.add", &json!({"by": 7})).unwrap();
        assert_eq!(
            app.run("edit.redo", &Value::Null),
            Err(Error::Disabled("edit.redo".into()))
        );
    }

    #[test]
    fn undo_history_is_capped() {
        let mut app = app();
        for _ in 0..UNDO_LIMIT + 5 {
            app.run("test.add", &json!({"by": 1})).unwrap();
        }
        while app.can_undo() {
            app.run("edit.undo", &Value::Null).unwrap();
        }
        assert_eq!(probe(&app), 5, "the oldest 5 steps were dropped");
    }

    #[test]
    fn undo_snapshots_share_table_columns() {
        use std::sync::Arc;

        use arrow_array::{Array, Int64Array, RecordBatch};
        use metriccraft_data::TableBuilder;

        let rows: i64 = 10_000_000;
        let col = Int64Array::from_iter_values((0..rows).map(|i| i % 100));
        let batch = RecordBatch::try_from_iter([("Qty", Arc::new(col) as _)]).unwrap();
        let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        drop(batch);
        let table = b.finish().unwrap();
        let buffer =
            |doc: &Document| doc.tables[0].batch().column(0).to_data().buffers()[0].as_ptr();

        let mut app = app();
        app.doc.tables.push(table);
        for _ in 0..UNDO_LIMIT {
            app.run("test.add", &json!({"by": 1})).unwrap();
        }
        assert_eq!(app.undo.len(), UNDO_LIMIT);
        let original = buffer(&app.doc);
        assert!(
            app.undo.iter().all(|s| buffer(&s.doc) == original),
            "every snapshot points at the same column buffer: no extra column memory"
        );
        assert_eq!(app.doc.tables[0].rows(), 10_000_000);
        assert!(
            app.doc.tables[0].memory() < 11 << 20,
            "stored as Int8: ~10 MB"
        );
    }

    #[test]
    fn unknown_and_disabled_commands_error() {
        let mut app = app();
        assert_eq!(
            app.run("nope", &Value::Null),
            Err(Error::UnknownCommand("nope".into()))
        );
        assert_eq!(
            app.run("edit.undo", &Value::Null),
            Err(Error::Disabled("edit.undo".into()))
        );
    }
}
