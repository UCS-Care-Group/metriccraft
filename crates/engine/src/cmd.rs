use serde_json::Value;

use crate::load;
use crate::query::{self, has_queries};
use crate::{App, Document, Result};

/// What a command acts on.
#[derive(Clone, Copy)]
pub enum Run {
    /// Edits the document. Runs against a copy that replaces the document only on success, so a
    /// failing edit changes nothing; successful edits are recorded for undo.
    Edit(fn(&mut Document, &Value) -> Result),
    /// Acts on the app itself (undo/redo; later selection and view state). Not recorded.
    App(fn(&mut App, &Value) -> Result),
}

/// A user-visible action. Ids follow Power BI's ribbon (`home.getData`) plus primitives.
#[derive(Clone, Copy)]
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    /// Ribbon location, e.g. `"Home > Queries"`; empty if the command isn't on the ribbon.
    pub ribbon: &'static str,
    pub shortcut: Option<&'static str>,
    /// Params, one per line as `name: type — meaning`; empty if none. Empty params must run with
    /// defaults or return [`crate::Error::InvalidParams`], never open a dialog.
    pub params: &'static str,
    pub enabled: fn(&App) -> bool,
    pub run: Run,
}

/// Every command, in menu order.
pub static COMMANDS: &[Command] = &[
    Command {
        id: "edit.undo",
        label: "Undo",
        ribbon: "",
        shortcut: Some("Ctrl+Z"),
        params: "",
        enabled: App::can_undo,
        run: Run::App(|app, _| app.undo()),
    },
    Command {
        id: "edit.redo",
        label: "Redo",
        ribbon: "",
        shortcut: Some("Ctrl+Y"),
        params: "",
        enabled: App::can_redo,
        run: Run::App(|app, _| app.redo()),
    },
    // Get Data and Refresh (spec S-1, S-2); both undoable, both report what they loaded.
    Command {
        id: "home.getData",
        label: "Get Data",
        ribbon: "Home > Data",
        shortcut: None,
        params: "kind: text — csv, parquet or xlsx\n\
                 path: text — the file\n\
                 name: text — the Query and table name (default: the file name without its \
                 extension, or the sheet/table)\n\
                 culture: text — reads numbers and dates, e.g. en-GB (default: en-US)\n\
                 delimiter: text — CSV: one character, \\t for tab (default: detected)\n\
                 encoding: text — CSV: utf-8, windows-1252, utf-16le… (default: detected)\n\
                 item: text — XLSX: the sheet or table to read (required)\n\
                 itemKind: text — XLSX: sheet or table (default: sheet)",
        enabled: |_| true,
        run: Run::App(load::get_data),
    },
    Command {
        id: "home.refresh",
        label: "Refresh",
        ribbon: "Home > Queries",
        shortcut: None,
        params: "",
        enabled: has_queries,
        run: Run::App(load::refresh),
    },
    // Applied Steps (spec Q-1), with the Power Query Editor's ribbon names. Each appends a step to
    // `query` and re-runs it, replacing the Query's loaded table.
    Command {
        id: "transform.useFirstRowAsHeaders",
        label: "Use First Row as Headers",
        ribbon: "Transform > Table",
        shortcut: None,
        params: "query: text — the Query",
        enabled: has_queries,
        run: Run::Edit(query::use_first_row_as_headers),
    },
    Command {
        id: "transform.changeType",
        label: "Data Type",
        ribbon: "Transform > Any Column",
        shortcut: None,
        params: "query: text — the Query\n\
                 columns: list of text — the columns to change\n\
                 type: text — text, decimal, whole, date, datetime or boolean",
        enabled: has_queries,
        run: Run::Edit(query::change_type),
    },
    Command {
        id: "transform.renameColumn",
        label: "Rename",
        ribbon: "Transform > Any Column",
        shortcut: Some("F2"),
        params: "query: text — the Query\n\
                 column: text — the column to rename\n\
                 name: text — its new name, unique in the table",
        enabled: has_queries,
        run: Run::Edit(query::rename_column),
    },
    Command {
        id: "home.removeColumns",
        label: "Remove Columns",
        ribbon: "Home > Manage Columns",
        shortcut: Some("Delete"),
        params: "query: text — the Query\n\
                 columns: list of text — the columns to remove (at least one must remain)",
        enabled: has_queries,
        run: Run::Edit(query::remove_columns),
    },
    Command {
        id: "transform.reorderColumns",
        label: "Move",
        ribbon: "Transform > Any Column",
        shortcut: None,
        params: "query: text — the Query\n\
                 columns: list of text — these columns, in this order, fill the positions they \
                 hold; the others stay put",
        enabled: has_queries,
        run: Run::Edit(query::reorder_columns),
    },
    Command {
        id: "query.deleteStep",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query\n\
                 step: integer — the step's position in Applied Steps, 0 = first",
        enabled: has_queries,
        run: Run::Edit(query::delete_step),
    },
];
