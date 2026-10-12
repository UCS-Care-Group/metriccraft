use std::collections::{BTreeSet, HashMap, VecDeque};

use opendash_data::{Preview, Query, Table, default_memory_cap};
use opendash_model::Model;
use serde_json::Value;

use crate::{COMMANDS, Command, Error, LoadReport, Result, Run};

/// Everything that is saved and undoable. Filled in by later milestones (model, report).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// Loaded data. Columns are shared, never copied, so cloning a Document for undo is cheap.
    pub tables: Vec<Table>,
    /// Queries; each one's result is the table of the same name in `tables`.
    pub queries: Vec<Query>,
    /// Queries edited since their table loaded (Stale, CONTEXT.md): the table is still the
    /// previous result until Close & Apply or Refresh.
    pub stale: BTreeSet<String>,
    /// Queries with *Enable load* off: no Table, but they can still be merged from.
    pub unloaded: BTreeSet<String>,
    /// The model properties of the loaded Tables (one per Table, same name).
    pub model: Model,
    /// Lets unit tests observe edits before the document has real content.
    #[cfg(test)]
    pub(crate) probe: i64,
}

/// Undo steps kept; the oldest is dropped beyond this.
pub const UNDO_LIMIT: usize = 100;

/// The running app: the document, its undo history and the command set.
pub struct App {
    doc: Document,
    commands: &'static [Command],
    // ponytail: whole-Document snapshots — cheap because table columns are shared (ADR 0005).
    // Switch to inverse operations if cloning a Document shows up in benchmarks.
    undo: VecDeque<Document>,
    redo: Vec<Document>,
    /// What the last Get Data or Refresh loaded (not part of the Document: not undone).
    last_load: Option<LoadReport>,
    /// The memory cap for loaded data (spec N-4), counted over the document, undo and redo.
    memory_cap: usize,
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
            last_load: None,
            memory_cap: default_memory_cap(),
        }
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

    /// The memory cap for loaded data (spec N-4): half of physical RAM unless set.
    pub fn memory_cap(&self) -> usize {
        self.memory_cap
    }

    /// Sets the memory cap for loaded data (spec N-4); checked by the next load.
    pub fn set_memory_cap(&mut self, bytes: usize) {
        self.memory_cap = bytes;
    }

    /// Bytes of column data held: every distinct buffer of the document, undo and redo
    /// (snapshots share buffers, so each is counted once). What the memory cap is checked against.
    pub fn memory(&self) -> usize {
        Held::new(self.undo.iter().chain(&self.redo).chain([&self.doc])).total
    }

    pub fn commands(&self) -> &'static [Command] {
        self.commands
    }

    pub fn command(&self, id: &str) -> Option<&'static Command> {
        // ponytail: linear scan, ~1.5 ns per command (bench engine/dispatch_unknown). Switch to a
        // sorted slice + binary search once the ribbon has hundreds of commands.
        self.commands.iter().find(|c| c.id == id)
    }

    /// Runs a command by id. The single entry point for UI, control channel, CLI and MCP.
    pub fn run(&mut self, id: &str, params: &Value) -> Result {
        let cmd = self
            .command(id)
            .ok_or_else(|| Error::UnknownCommand(id.into()))?;
        if !(cmd.enabled)(self) {
            return Err(Error::Disabled(id.into()));
        }
        match cmd.run {
            Run::Edit(edit) => {
                let mut next = self.doc.clone();
                edit(&mut next, params)?;
                let prev = std::mem::replace(&mut self.doc, next);
                self.push_undo(prev);
                self.redo.clear();
                Ok(())
            }
            Run::App(act) => act(self, params),
        }
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
        let mut held = Held::new(self.undo.iter().chain(&self.redo).chain([&self.doc]));
        // What dropping every undo step would leave.
        let base = Held::new(self.redo.iter().chain([&self.doc])).total;
        let mut dropped = 0usize;
        let mut room = |undo: &mut VecDeque<Document>, held: &mut Held, need: usize| {
            while held.total.saturating_add(need) > self.memory_cap && held.total > base {
                let Some(oldest) = undo.pop_front() else {
                    break;
                };
                held.remove(&oldest);
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
        self.redo.push(std::mem::replace(&mut self.doc, prev));
        self.last_load = None;
        Ok(())
    }

    pub(crate) fn redo(&mut self) -> Result {
        let next = self
            .redo
            .pop()
            .ok_or_else(|| Error::Disabled("edit.redo".into()))?;
        let prev = std::mem::replace(&mut self.doc, next);
        self.push_undo(prev);
        self.last_load = None;
        Ok(())
    }

    fn push_undo(&mut self, doc: Document) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back(doc);
    }
}

/// Whether `e` is a load stopped by the memory cap.
fn is_memory_cap(e: &Error) -> bool {
    matches!(
        e,
        Error::Data(opendash_data::Error::MemoryCap { .. })
            | Error::Refresh {
                error: opendash_data::Error::MemoryCap { .. },
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
            params: "by: integer — amount to add",
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
        use opendash_data::TableBuilder;

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
            app.undo.iter().all(|d| buffer(d) == original),
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
