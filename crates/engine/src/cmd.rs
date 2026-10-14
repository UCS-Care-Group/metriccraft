use serde_json::Value;

use crate::expression;
use crate::file;
use crate::load;
use crate::model::{self, has_tables};
use crate::query::{self, has_queries};
use crate::recovery;
use crate::relationship;
use crate::rename;
use crate::report::{self, has_pages, has_visuals};
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
    // The File menu (ADR 0003, spec F-1). Open and New replace the document: not undoable.
    Command {
        id: "file.new",
        label: "New",
        ribbon: "File",
        shortcut: Some("Ctrl+N"),
        params: "",
        enabled: |_| true,
        run: Run::App(file::new),
    },
    Command {
        id: "file.open",
        label: "Open",
        ribbon: "File",
        shortcut: Some("Ctrl+O"),
        params: file::OPEN_PARAMS,
        enabled: |_| true,
        run: Run::App(file::open),
    },
    Command {
        id: "file.save",
        label: "Save",
        ribbon: "File",
        shortcut: Some("Ctrl+S"),
        params: file::SAVE_PARAMS,
        enabled: |_| true,
        run: Run::App(file::save),
    },
    Command {
        id: "file.saveAs",
        label: "Save as",
        ribbon: "File",
        shortcut: None,
        params: file::SAVE_AS_PARAMS,
        enabled: |_| true,
        run: Run::App(file::save_as),
    },
    // Local overrides (`file.rs`): where a Source file is on this machine.
    Command {
        id: "file.remapSource",
        label: "Remap source path",
        // Like Data source settings > Change Source, but for this user only: not on the ribbon.
        ribbon: "",
        shortcut: None,
        params: file::REMAP_PARAMS,
        enabled: |_| true,
        run: Run::App(file::remap_source),
    },
    // Autosave recovery (ADR 0003; `recovery.rs`): offered at launch, as Power BI's AutoRecovery.
    Command {
        id: "file.recover",
        label: "Recover",
        ribbon: "",
        shortcut: None,
        params: recovery::RECOVER_PARAMS,
        enabled: |_| true,
        run: Run::App(recovery::recover),
    },
    Command {
        id: "file.discardRecovery",
        label: "Discard",
        ribbon: "",
        shortcut: None,
        params: recovery::DISCARD_PARAMS,
        enabled: |_| true,
        run: Run::App(recovery::discard),
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
                 encoding: text — CSV: utf-8, windows-1252, utf-16le or utf-16be (default: detected)\n\
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
    Command {
        id: "home.closeApply",
        label: "Close & Apply",
        ribbon: "Power Query Editor: Home > Close",
        shortcut: None,
        params: "",
        enabled: load::has_stale,
        run: Run::App(load::close_apply),
    },
    // Applied Steps (spec Q-1), with the Power Query Editor's ribbon names. Each appends a step to
    // `query`, checks it on the Preview rows and marks the table Stale until Close & Apply.
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
                 type: text — text, decimal, fixedDecimal, whole, date, datetime or boolean",
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
        id: "transform.filterRows",
        label: "Filter Rows",
        // Power Query's column-header filter menu; not on the ribbon.
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query\n\
                 conditions: list of {column: text, op: text, value} — op is equals, notEquals, \
                 greater, greaterOrEqual, less, lessOrEqual, contains, beginsWith or endsWith; \
                 value is a number, text, logical or null (equals null = is null); text for a \
                 date or datetime column is read as one\n\
                 combine: text — and (default) or or",
        enabled: has_queries,
        run: Run::Edit(query::filter_rows),
    },
    Command {
        id: "home.mergeQueries",
        label: "Merge Queries",
        ribbon: "Home > Combine",
        shortcut: None,
        params: "query: text — the Query to merge into\n\
                 keys: list of text — its key columns\n\
                 with: text — the other Query\n\
                 withKeys: list of text — the other Query's key columns, paired with keys in order\n\
                 kind: text — leftOuter (default), inner or fullOuter\n\
                 column: text — the merged column's name (default: the other Query's name)\n\
                 expand: list of text — the other Query's columns to add (default: all)\n\
                 prefix: logical — name added columns `column.name` (default true)",
        enabled: has_queries,
        run: Run::Edit(query::merge_queries),
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
    Command {
        id: "query.enableLoad",
        label: "Enable load",
        // Power Query's Queries pane, right-click a query.
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query\n\
                 enabled: logical — load it into the model (default: the opposite of now)",
        enabled: has_queries,
        run: Run::Edit(model::enable_load),
    },
    // Power Query's Queries pane, right-click a query (spec D-4, C-1; `rename.rs`). A Query and
    // its Table share their name: renaming or deleting one does both.
    Command {
        id: "query.rename",
        label: "Rename",
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query (and its Table)\n\
                 name: text — its new name, unique among Queries ignoring case; rewrites \
                 Relationships and other Queries' merges (same as modeling.renameTable)",
        enabled: has_queries,
        run: Run::App(rename::rename_query),
    },
    Command {
        id: "query.delete",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query; its Table and Relationships go too (refused while \
                 another Query merges it)",
        enabled: has_queries,
        run: Run::Edit(rename::delete_query),
    },
    Command {
        id: "query.duplicate",
        label: "Duplicate",
        ribbon: "",
        shortcut: None,
        params: "query: text — the Query to copy, as `<query> (2)` (the next free number); \
                 loaded at the next Close & Apply",
        enabled: has_queries,
        run: Run::Edit(rename::duplicate_query),
    },
    // The model (spec D-1): Column tools and the Properties pane.
    Command {
        id: "columnTools.dataType",
        label: "Data type",
        ribbon: "Column tools > Formatting",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — the Column\n\
                 type: text — decimal, fixedDecimal, whole, datetime, date, text or boolean; \
                 converts the loaded data (a value that doesn't convert fails the change), the \
                 Query is untouched",
        enabled: has_tables,
        run: Run::Edit(model::set_data_type),
    },
    Command {
        id: "columnTools.format",
        label: "Format",
        ribbon: "Column tools > Formatting",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — the Column or Measure\n\
                 formatString: text — a format string, e.g. #,0.00 (empty: the default)",
        enabled: has_tables,
        run: Run::Edit(model::set_format),
    },
    Command {
        id: "columnTools.summarization",
        label: "Summarization",
        ribbon: "Column tools > Properties",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — the Column\n\
                 summarizeBy: text — none, sum, average, min, max, count or distinctCount \
                 (sum and average need a number column)",
        enabled: has_tables,
        run: Run::Edit(model::set_summarize_by),
    },
    Command {
        id: "columnTools.sortByColumn",
        label: "Sort by column",
        ribbon: "Column tools > Sort",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — the Column to sort\n\
                 by: text or null — another column of the Table with one value per value of \
                 this one; null sorts by the column itself",
        enabled: has_tables,
        run: Run::Edit(model::set_sort_by),
    },
    Command {
        id: "model.setHidden",
        label: "Is hidden",
        // The Properties pane, or right-click > Hide in report view.
        ribbon: "",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — one of its Columns or Measures (default: the Table itself)\n\
                 hidden: logical — default true",
        enabled: has_tables,
        run: Run::Edit(model::set_hidden),
    },
    Command {
        id: "model.setDescription",
        label: "Description",
        // The Properties pane.
        ribbon: "",
        shortcut: None,
        params: "table: text — the Table\n\
                 column: text — one of its Columns or Measures (default: the Table itself)\n\
                 description: text — empty clears it",
        enabled: has_tables,
        run: Run::Edit(model::set_description),
    },
    // The Fields pane / Data view: right-click > Rename or Delete from model (spec D-4).
    Command {
        id: "modeling.renameTable",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: "table: text — the Table (and its Query)\n\
                 name: text — its new name, unique among Queries ignoring case; rewrites \
                 Relationships and other Queries' merges (same as query.rename)",
        enabled: has_tables,
        run: Run::App(rename::rename_table),
    },
    Command {
        id: "modeling.renameColumn",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: "table: text — the Table\n\
                 column: text — the Column\n\
                 name: text — its new name, unique in the Table ignoring case; adds a Renamed \
                 Columns step to the Query and rewrites sort by, Relationships and other \
                 Queries' merges",
        enabled: has_tables,
        run: Run::App(rename::rename_column),
    },
    Command {
        id: "modeling.deleteTable",
        label: "Delete from model",
        ribbon: "",
        shortcut: None,
        params: "table: text — the Table; its Query and Relationships go too (refused while \
                 another Query merges it)",
        enabled: has_tables,
        run: Run::Edit(rename::delete_table),
    },
    // Measures and Calculated columns (spec D-3; `expression.rs`): Modeling > Calculations,
    // the formula bar, and the Fields pane's right-click > Rename / Delete from model.
    Command {
        id: "modeling.newMeasure",
        label: "New measure",
        ribbon: "Modeling > Calculations",
        shortcut: None,
        params: expression::NEW_MEASURE_PARAMS,
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::new_measure),
    },
    Command {
        id: "modeling.newColumn",
        label: "New column",
        ribbon: "Modeling > Calculations",
        shortcut: None,
        params: expression::NEW_COLUMN_PARAMS,
        enabled: has_tables,
        run: Run::Edit(expression::new_column),
    },
    Command {
        id: "modeling.editExpression",
        label: "Edit formula",
        // The formula bar.
        ribbon: "",
        shortcut: None,
        params: expression::EDIT_PARAMS,
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::edit_expression),
    },
    Command {
        id: "modeling.delete",
        label: "Delete from model",
        ribbon: "",
        shortcut: Some("Delete"),
        params: expression::DELETE_PARAMS,
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::delete),
    },
    Command {
        id: "modeling.renameMeasure",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: expression::RENAME_MEASURE_PARAMS,
        enabled: expression::has_model_tables,
        run: Run::App(expression::rename_measure),
    },
    // Relationships (spec D-2): Modeling > Manage relationships.
    Command {
        id: "modeling.newRelationship",
        label: "New relationship",
        ribbon: "Modeling > Relationships",
        shortcut: None,
        params: relationship::NEW_PARAMS,
        enabled: has_tables,
        run: Run::App(relationship::new_relationship),
    },
    Command {
        id: "modeling.editRelationship",
        label: "Edit relationship",
        // Manage relationships > Edit.
        ribbon: "",
        shortcut: None,
        params: relationship::EDIT_PARAMS,
        enabled: has_tables,
        run: Run::App(relationship::edit_relationship),
    },
    Command {
        id: "modeling.deleteRelationship",
        label: "Delete relationship",
        // Manage relationships > Delete.
        ribbon: "",
        shortcut: None,
        params: relationship::DELETE_PARAMS,
        enabled: has_tables,
        run: Run::Edit(relationship::delete_relationship),
    },
    Command {
        id: "modeling.setRelationshipActive",
        label: "Active",
        // Manage relationships: the Active check box.
        ribbon: "",
        shortcut: None,
        params: relationship::ACTIVE_PARAMS,
        enabled: has_tables,
        run: Run::Edit(relationship::set_active),
    },
    Command {
        id: "modeling.autodetectRelationships",
        label: "Autodetect",
        // Manage relationships > Autodetect.
        ribbon: "",
        shortcut: None,
        params: "",
        enabled: has_tables,
        run: Run::App(relationship::autodetect),
    },
    // The report (spec R-1; `report.rs`): Pages and Visuals, all undoable.
    Command {
        id: "insert.newPage",
        label: "New page",
        ribbon: "Insert > Pages",
        shortcut: None,
        params: report::NEW_PAGE_PARAMS,
        enabled: |_| true,
        run: Run::Edit(report::new_page),
    },
    // The page tabs: right-click a Page.
    Command {
        id: "page.delete",
        label: "Delete page",
        ribbon: "",
        shortcut: None,
        params: report::PAGE_PARAMS,
        enabled: has_pages,
        run: Run::Edit(report::delete_page),
    },
    Command {
        id: "page.rename",
        label: "Rename page",
        ribbon: "",
        shortcut: None,
        params: report::RENAME_PAGE_PARAMS,
        enabled: has_pages,
        run: Run::Edit(report::rename_page),
    },
    // Dragging a page tab.
    Command {
        id: "page.move",
        label: "Move page",
        ribbon: "",
        shortcut: None,
        params: report::MOVE_PAGE_PARAMS,
        enabled: has_pages,
        run: Run::Edit(report::move_page),
    },
    Command {
        id: "insert.newVisual",
        label: "New visual",
        ribbon: "Insert > Visuals",
        shortcut: None,
        params: report::NEW_VISUAL_PARAMS,
        enabled: |_| true,
        run: Run::Edit(report::new_visual),
    },
    // On the canvas and in the Visualizations pane.
    Command {
        id: "visual.delete",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: report::VISUAL_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::delete_visual),
    },
    Command {
        id: "visual.move",
        label: "Move",
        ribbon: "",
        shortcut: None,
        params: report::MOVE_VISUAL_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::move_visual),
    },
    Command {
        id: "visual.resize",
        label: "Resize",
        ribbon: "",
        shortcut: None,
        params: report::RESIZE_VISUAL_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::resize_visual),
    },
    // The Visualizations pane: a type picked with a Visual selected.
    Command {
        id: "visual.changeType",
        label: "Change visual type",
        ribbon: "",
        shortcut: None,
        params: report::CHANGE_TYPE_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::change_type),
    },
    // The field wells: drop a Field, or tick it in the Fields pane.
    Command {
        id: "visual.addField",
        label: "Add field",
        ribbon: "",
        shortcut: None,
        params: report::ADD_FIELD_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::add_field),
    },
    Command {
        id: "visual.removeField",
        label: "Remove field",
        ribbon: "",
        shortcut: None,
        params: report::REMOVE_FIELD_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::remove_field),
    },
    Command {
        id: "visual.moveField",
        label: "Move field",
        ribbon: "",
        shortcut: None,
        params: report::MOVE_FIELD_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::move_field),
    },
    // A Visual's More options > Sort axis / Sort by.
    Command {
        id: "visual.sort",
        label: "Sort by",
        ribbon: "",
        shortcut: None,
        params: report::SORT_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::sort),
    },
    // The Format pane.
    Command {
        id: "visual.format",
        label: "Format",
        ribbon: "",
        shortcut: None,
        params: report::FORMAT_PARAMS,
        enabled: has_visuals,
        run: Run::Edit(report::format),
    },
    Command {
        id: "options.autodetectRelationships",
        label: "Autodetect new relationships after data is loaded",
        // File > Options and settings > Options > Current file > Data Load.
        ribbon: "",
        shortcut: None,
        params: "enabled: logical — default: the opposite of now",
        enabled: |_| true,
        run: Run::Edit(relationship::set_autodetect),
    },
    Command {
        id: "options.autosaveMinutes",
        label: "Store AutoRecovery information every N minutes",
        // File > Options and settings > Options > Global > Auto recovery. An app option, not
        // undoable.
        ribbon: "",
        shortcut: None,
        params: "minutes: integer — 0 (off) to 1440; default 5",
        enabled: |_| true,
        run: Run::App(recovery::set_minutes),
    },
];
