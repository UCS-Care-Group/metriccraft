use serde_json::Value;

use crate::expression;
use crate::file;
use crate::load;
use crate::model::{self, has_tables};
use crate::param::{Kind, Param, null, opt, req};
use crate::query::{self, has_queries};
use crate::recovery;
use crate::relationship;
use crate::rename;
use crate::report::{self, has_pages, has_visuals};
use crate::selection;
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
    /// What the command takes (spec C-3), checked by [`App::run`]. Empty params must run with
    /// defaults or return [`crate::Error::InvalidParams`], never open a dialog.
    pub params: &'static [Param],
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
        params: &[],
        enabled: App::can_undo,
        run: Run::App(|app, _| app.undo()),
    },
    Command {
        id: "edit.redo",
        label: "Redo",
        ribbon: "",
        shortcut: Some("Ctrl+Y"),
        params: &[],
        enabled: App::can_redo,
        run: Run::App(|app, _| app.redo()),
    },
    // The File menu (ADR 0003, spec F-1). Open and New replace the document: not undoable.
    Command {
        id: "file.new",
        label: "New",
        ribbon: "File",
        shortcut: Some("Ctrl+N"),
        params: &[],
        enabled: |_| true,
        run: Run::App(file::new),
    },
    Command {
        id: "file.open",
        label: "Open",
        ribbon: "File",
        shortcut: Some("Ctrl+O"),
        params: &[
            req(
                "path",
                Kind::Path,
                "a Project folder, its project.odash.json, or a .odash Package",
            ),
            opt(
                "recover",
                Kind::Choice(&["staging", "lastGood"]),
                "after an interrupted save only: staging (the save that was being made) or lastGood (the state before it)",
            ),
        ],
        enabled: |_| true,
        run: Run::App(file::open),
    },
    Command {
        id: "file.save",
        label: "Save",
        ribbon: "File",
        shortcut: Some("Ctrl+S"),
        params: &[opt(
            "path",
            Kind::Path,
            "save here instead, as Save As does (default: where it was opened or last saved)",
        )],
        enabled: |_| true,
        run: Run::App(file::save),
    },
    Command {
        id: "file.saveAs",
        label: "Save as",
        ribbon: "File",
        shortcut: None,
        params: &[
            req(
                "path",
                Kind::Path,
                "the Project folder or .odash Package to write",
            ),
            opt(
                "package",
                Kind::Bool,
                "write a .odash Package (default: whether path ends in .odash)",
            ),
            opt(
                "data",
                Kind::Bool,
                "include the Data cache in a Package (default true; a Project folder always keeps it)",
            ),
        ],
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
        params: &[
            req(
                "from",
                Kind::Path,
                "a Source file path as the Project's Queries name it (relative to the Project folder, or absolute)",
            ),
            null(req(
                "to",
                Kind::Path,
                "read it from here on this machine instead (relative to the Project folder, or absolute); null removes the remap. Kept in .local/, never shared",
            )),
        ],
        enabled: |_| true,
        run: Run::App(file::remap_source),
    },
    // Autosave recovery (ADR 0003; `recovery.rs`): offered at launch, as Power BI's AutoRecovery.
    Command {
        id: "file.recover",
        label: "Recover",
        ribbon: "",
        shortcut: None,
        params: &[
            req("id", Kind::Text, "the recovery copy, as listed at launch"),
            opt(
                "dir",
                Kind::Path,
                "the recovery folder (default: the user's, e.g. ~/.cache/opendash/recovery)",
            ),
        ],
        enabled: |_| true,
        run: Run::App(recovery::recover),
    },
    Command {
        id: "file.discardRecovery",
        label: "Discard",
        ribbon: "",
        shortcut: None,
        params: &[
            req("id", Kind::Text, "the recovery copy, as listed at launch"),
            opt(
                "dir",
                Kind::Path,
                "the recovery folder (default: the user's, e.g. ~/.cache/opendash/recovery)",
            ),
        ],
        enabled: |_| true,
        run: Run::App(recovery::discard),
    },
    // Get Data and Refresh (spec S-1, S-2); both undoable, both report what they loaded.
    Command {
        id: "home.getData",
        label: "Get Data",
        ribbon: "Home > Data",
        shortcut: None,
        params: &[
            req(
                "kind",
                Kind::Choice(&["csv", "parquet", "xlsx"]),
                "csv, parquet or xlsx",
            ),
            req("path", Kind::Path, "the file"),
            opt(
                "name",
                Kind::Text,
                "the Query and table name (default: the file name without its extension, or the sheet/table)",
            ),
            opt(
                "culture",
                Kind::Text,
                "reads numbers and dates, e.g. en-GB (default: en-US)",
            ),
            opt(
                "delimiter",
                Kind::Text,
                "CSV: one character, \\t for tab (default: detected)",
            ),
            opt(
                "encoding",
                Kind::Choice(&["utf-8", "windows-1252", "utf-16le", "utf-16be"]),
                "CSV: utf-8, windows-1252, utf-16le or utf-16be (default: detected)",
            ),
            opt(
                "item",
                Kind::Text,
                "XLSX: the sheet or table to read (required)",
            ),
            opt(
                "itemKind",
                Kind::Choice(&["sheet", "table"]),
                "XLSX: sheet or table (default: sheet)",
            ),
        ],
        enabled: |_| true,
        run: Run::App(load::get_data),
    },
    Command {
        id: "home.refresh",
        label: "Refresh",
        ribbon: "Home > Queries",
        shortcut: None,
        params: &[],
        enabled: has_queries,
        run: Run::App(load::refresh),
    },
    Command {
        id: "home.closeApply",
        label: "Close & Apply",
        ribbon: "Power Query Editor: Home > Close",
        shortcut: None,
        params: &[],
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
        params: &[req("query", Kind::Query, "the Query")],
        enabled: has_queries,
        run: Run::Edit(query::use_first_row_as_headers),
    },
    Command {
        id: "transform.changeType",
        label: "Data Type",
        ribbon: "Transform > Any Column",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query"),
            req("columns", Kind::Texts, "the columns to change"),
            req(
                "type",
                Kind::Choice(&[
                    "text",
                    "decimal",
                    "fixedDecimal",
                    "whole",
                    "date",
                    "datetime",
                    "boolean",
                ]),
                "text, decimal, fixedDecimal, whole, date, datetime or boolean",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::change_type),
    },
    Command {
        id: "transform.renameColumn",
        label: "Rename",
        ribbon: "Transform > Any Column",
        shortcut: Some("F2"),
        params: &[
            req("query", Kind::Query, "the Query"),
            req("column", Kind::Text, "the column to rename"),
            req("name", Kind::Text, "its new name, unique in the table"),
        ],
        enabled: has_queries,
        run: Run::Edit(query::rename_column),
    },
    Command {
        id: "home.removeColumns",
        label: "Remove Columns",
        ribbon: "Power Query Editor: Home > Manage Columns",
        shortcut: Some("Delete"),
        params: &[
            req("query", Kind::Query, "the Query"),
            req(
                "columns",
                Kind::Texts,
                "the columns to remove (at least one must remain)",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::remove_columns),
    },
    Command {
        id: "transform.reorderColumns",
        label: "Move",
        ribbon: "Transform > Any Column",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query"),
            req(
                "columns",
                Kind::Texts,
                "these columns, in this order, fill the positions they hold; the others stay put",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::reorder_columns),
    },
    Command {
        id: "transform.filterRows",
        label: "Filter Rows",
        // Power Query's column-header filter menu; not on the ribbon.
        ribbon: "Power Query Editor: Home > Reduce Rows",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query"),
            req(
                "conditions",
                Kind::Json,
                "list of {column: text, op: text, value} — op is equals, notEquals, greater, greaterOrEqual, less, lessOrEqual, contains, beginsWith or endsWith; value is a number, text, logical or null (equals null = is null); text for a date or datetime column is read as one",
            ),
            opt(
                "combine",
                Kind::Choice(&["and", "or"]),
                "and (default) or or",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::filter_rows),
    },
    Command {
        id: "home.mergeQueries",
        label: "Merge Queries",
        ribbon: "Power Query Editor: Home > Combine",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query to merge into"),
            req("keys", Kind::Texts, "its key columns"),
            req("with", Kind::Query, "the other Query"),
            req(
                "withKeys",
                Kind::Texts,
                "the other Query's key columns, paired with keys in order",
            ),
            opt(
                "kind",
                Kind::Choice(&["leftOuter", "inner", "fullOuter"]),
                "leftOuter (default), inner or fullOuter",
            ),
            opt(
                "column",
                Kind::Text,
                "the merged column's name (default: the other Query's name)",
            ),
            opt(
                "expand",
                Kind::Texts,
                "the other Query's columns to add (default: all)",
            ),
            opt(
                "prefix",
                Kind::Bool,
                "name added columns `column.name` (default true)",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::merge_queries),
    },
    Command {
        id: "query.deleteStep",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query"),
            req(
                "step",
                Kind::Integer,
                "the step's position in Applied Steps, 0 = first",
            ),
        ],
        enabled: has_queries,
        run: Run::Edit(query::delete_step),
    },
    Command {
        id: "query.enableLoad",
        label: "Enable load",
        // Power Query's Queries pane, right-click a query.
        ribbon: "",
        shortcut: None,
        params: &[
            req("query", Kind::Query, "the Query"),
            opt(
                "enabled",
                Kind::Bool,
                "load it into the model (default: the opposite of now)",
            ),
        ],
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
        params: &[
            req("query", Kind::Query, "the Query (and its Table)"),
            req(
                "name",
                Kind::Text,
                "its new name, unique among Queries ignoring case; rewrites Relationships and other Queries' merges (same as modeling.renameTable)",
            ),
        ],
        enabled: has_queries,
        run: Run::App(rename::rename_query),
    },
    Command {
        id: "query.delete",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: &[req(
            "query",
            Kind::Query,
            "the Query; its Table and Relationships go too (refused while another Query merges it)",
        )],
        enabled: has_queries,
        run: Run::Edit(rename::delete_query),
    },
    Command {
        id: "query.duplicate",
        label: "Duplicate",
        ribbon: "",
        shortcut: None,
        params: &[req(
            "query",
            Kind::Query,
            "the Query to copy, as `<query> (2)` (the next free number); loaded at the next Close & Apply",
        )],
        enabled: has_queries,
        run: Run::Edit(rename::duplicate_query),
    },
    // The model (spec D-1): Column tools and the Properties pane.
    Command {
        id: "columnTools.dataType",
        label: "Data type",
        ribbon: "Column tools > Formatting",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req("column", Kind::Column, "the Column"),
            req(
                "type",
                Kind::Choice(&[
                    "text",
                    "decimal",
                    "fixedDecimal",
                    "whole",
                    "date",
                    "datetime",
                    "boolean",
                ]),
                "decimal, fixedDecimal, whole, datetime, date, text or boolean; converts the loaded data (a value that doesn't convert fails the change), the Query is untouched",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_data_type),
    },
    Command {
        id: "columnTools.format",
        label: "Format",
        ribbon: "Column tools > Formatting",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req("column", Kind::Column, "the Column or Measure"),
            req(
                "formatString",
                Kind::Text,
                "a format string, e.g. #,0.00 (empty: the default)",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_format),
    },
    Command {
        id: "columnTools.summarization",
        label: "Summarization",
        ribbon: "Column tools > Properties",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req("column", Kind::Column, "the Column"),
            req(
                "summarizeBy",
                Kind::Choice(&[
                    "none",
                    "sum",
                    "average",
                    "min",
                    "max",
                    "count",
                    "distinctCount",
                ]),
                "none, sum, average, min, max, count or distinctCount (sum and average need a number column)",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_summarize_by),
    },
    Command {
        id: "columnTools.sortByColumn",
        label: "Sort by column",
        ribbon: "Column tools > Sort",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req("column", Kind::Column, "the Column to sort"),
            null(req(
                "by",
                Kind::Column,
                "another column of the Table with one value per value of this one; null sorts by the column itself",
            )),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_sort_by),
    },
    Command {
        id: "model.setHidden",
        label: "Is hidden",
        // The Properties pane, or right-click > Hide in report view.
        ribbon: "",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            opt(
                "column",
                Kind::Column,
                "one of its Columns or Measures (default: the Table itself)",
            ),
            opt("hidden", Kind::Bool, "default true"),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_hidden),
    },
    Command {
        id: "model.setDescription",
        label: "Description",
        // The Properties pane.
        ribbon: "",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            opt(
                "column",
                Kind::Column,
                "one of its Columns or Measures (default: the Table itself)",
            ),
            req("description", Kind::Text, "empty clears it"),
        ],
        enabled: has_tables,
        run: Run::Edit(model::set_description),
    },
    // The Fields pane / Data view: right-click > Rename or Delete from model (spec D-4).
    Command {
        id: "model.moveTable",
        label: "Move table",
        ribbon: "",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req(
                "x",
                Kind::Integer,
                "its box's new left edge in Model view, 0 to 100000",
            ),
            req("y", Kind::Integer, "its box's new top edge, 0 to 100000"),
            opt(
                "merge",
                Kind::Bool,
                "join the previous move of the same object into one undo step (arrow-key nudges; default false)",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(crate::diagram::move_table),
    },
    Command {
        id: "modeling.renameTable",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: &[
            req("table", Kind::Table, "the Table (and its Query)"),
            req(
                "name",
                Kind::Text,
                "its new name, unique among Queries ignoring case; rewrites Relationships and other Queries' merges (same as query.rename)",
            ),
        ],
        enabled: has_tables,
        run: Run::App(rename::rename_table),
    },
    Command {
        id: "modeling.renameColumn",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: &[
            req("table", Kind::Table, "the Table"),
            req("column", Kind::Column, "the Column"),
            req(
                "name",
                Kind::Text,
                "its new name, unique in the Table ignoring case; adds a Renamed Columns step to the Query and rewrites sort by, Relationships and other Queries' merges",
            ),
        ],
        enabled: has_tables,
        run: Run::App(rename::rename_column),
    },
    Command {
        id: "modeling.deleteTable",
        label: "Delete from model",
        ribbon: "",
        shortcut: None,
        params: &[req(
            "table",
            Kind::Table,
            "the Table; its Query and Relationships go too (refused while another Query merges it)",
        )],
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
        params: &[
            req("table", Kind::Table, "the Measure's home Table"),
            req(
                "name",
                Kind::Text,
                "unique among the Model's Measures, not a Column name of the Table (ignoring case)",
            ),
            req(
                "expression",
                Kind::Text,
                "its DAX, kept as written (one that doesn't parse is kept, with its error)",
            ),
            opt("formatString", Kind::Text, "optional, e.g. #,0.00"),
            opt("description", Kind::Text, "optional"),
        ],
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::new_measure),
    },
    Command {
        id: "modeling.newColumn",
        label: "New column",
        ribbon: "Modeling > Calculations",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req(
                "name",
                Kind::Text,
                "unique in the Table, not one of its Measures (ignoring case)",
            ),
            req(
                "expression",
                Kind::Text,
                "its DAX, evaluated for each row; the data type comes from the values",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(expression::new_column),
    },
    Command {
        id: "modeling.editExpression",
        label: "Edit formula",
        // The formula bar.
        ribbon: "",
        shortcut: None,
        params: &[
            req("table", Kind::Table, "the Table"),
            req(
                "name",
                Kind::Text,
                "one of its Measures or Calculated columns",
            ),
            req("expression", Kind::Text, "the new DAX"),
        ],
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::edit_expression),
    },
    Command {
        id: "modeling.delete",
        label: "Delete from model",
        ribbon: "",
        shortcut: Some("Delete"),
        params: &[
            req("table", Kind::Table, "the Table"),
            req(
                "name",
                Kind::Text,
                "one of its Measures or Calculated columns (a Calculated column's Relationships go too; DAX that used it is kept and shows an error)",
            ),
        ],
        enabled: expression::has_model_tables,
        run: Run::Edit(expression::delete),
    },
    Command {
        id: "modeling.renameMeasure",
        label: "Rename",
        ribbon: "",
        shortcut: Some("F2"),
        params: &[
            req("table", Kind::Table, "the Measure's home Table"),
            req("measure", Kind::Measure, "the Measure"),
            req(
                "name",
                Kind::Text,
                "its new name; rewrites every Measure and Calculated column that uses it",
            ),
        ],
        enabled: expression::has_model_tables,
        run: Run::App(expression::rename_measure),
    },
    // Relationships (spec D-2): Modeling > Manage relationships.
    Command {
        id: "modeling.newRelationship",
        label: "New relationship",
        ribbon: "Modeling > Relationships",
        shortcut: None,
        params: &[
            req("fromTable", Kind::Table, "the many side's Table"),
            req("fromColumn", Kind::Column, "its column"),
            req("toTable", Kind::Table, "the one side's Table"),
            req(
                "toColumn",
                Kind::Column,
                "its column: unique, non-blank values",
            ),
            opt(
                "active",
                Kind::Bool,
                "default true; made inactive (with a notice) if the two Tables already have an active Relationship",
            ),
            opt(
                "orient",
                Kind::Bool,
                "default false; true swaps the ends when only that way round is valid (as dragging a column onto another in the Model view does)",
            ),
        ],
        enabled: has_tables,
        run: Run::App(relationship::new_relationship),
    },
    Command {
        id: "modeling.editRelationship",
        label: "Edit relationship",
        // Manage relationships > Edit.
        ribbon: "",
        shortcut: None,
        params: &[
            req(
                "relationship",
                Kind::Json,
                "integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns",
            ),
            opt("fromTable", Kind::Table, "the new ends (default: as now)"),
            opt("fromColumn", Kind::Column, "the new ends (default: as now)"),
            opt("toTable", Kind::Table, "the new ends (default: as now)"),
            opt("toColumn", Kind::Column, "the new ends (default: as now)"),
            opt(
                "active",
                Kind::Bool,
                "default as now; made inactive (with a notice) if the two Tables already have another active Relationship",
            ),
        ],
        enabled: has_tables,
        run: Run::App(relationship::edit_relationship),
    },
    Command {
        id: "modeling.deleteRelationship",
        label: "Delete relationship",
        // Manage relationships > Delete.
        ribbon: "",
        shortcut: None,
        params: &[req(
            "relationship",
            Kind::Json,
            "integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns",
        )],
        enabled: has_tables,
        run: Run::Edit(relationship::delete_relationship),
    },
    Command {
        id: "modeling.setRelationshipActive",
        label: "Active",
        // Manage relationships: the Active check box.
        ribbon: "",
        shortcut: None,
        params: &[
            req(
                "relationship",
                Kind::Json,
                "integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns",
            ),
            opt(
                "active",
                Kind::Bool,
                "default true; activating fails while the two Tables have another active Relationship (deactivate it first)",
            ),
        ],
        enabled: has_tables,
        run: Run::Edit(relationship::set_active),
    },
    Command {
        id: "modeling.autodetectRelationships",
        label: "Autodetect",
        // Manage relationships > Autodetect.
        ribbon: "",
        shortcut: None,
        params: &[],
        enabled: has_tables,
        run: Run::App(relationship::autodetect),
    },
    // The report (spec R-1; `report.rs`): Pages and Visuals, all undoable.
    Command {
        id: "insert.newPage",
        label: "New page",
        ribbon: "Insert > Pages",
        shortcut: None,
        params: &[opt(
            "name",
            Kind::Text,
            "the Page's name, unique ignoring case (default: Page n, the first free n)",
        )],
        enabled: |_| true,
        run: Run::Edit(report::new_page),
    },
    // The page tabs: right-click a Page.
    Command {
        id: "page.setCurrent",
        label: "Go to page",
        ribbon: "",
        shortcut: None,
        params: &[req("page", Kind::Page, "the Page's id or name")],
        enabled: has_pages,
        run: Run::App(report::set_current),
    },
    Command {
        id: "page.delete",
        label: "Delete page",
        ribbon: "",
        shortcut: None,
        params: &[req("page", Kind::Page, "the Page's id or name")],
        enabled: has_pages,
        run: Run::Edit(report::delete_page),
    },
    Command {
        id: "page.rename",
        label: "Rename page",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req(
                "name",
                Kind::Text,
                "its new name, unique among Pages ignoring case",
            ),
        ],
        enabled: has_pages,
        run: Run::Edit(report::rename_page),
    },
    // Dragging a page tab.
    Command {
        id: "page.move",
        label: "Move page",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("index", Kind::Integer, "its new position, 0 = first"),
        ],
        enabled: has_pages,
        run: Run::Edit(report::move_page),
    },
    Command {
        id: "insert.newVisual",
        label: "New visual",
        ribbon: "Insert > Visuals",
        shortcut: None,
        params: &[
            req(
                "type",
                Kind::Choice(&[
                    "table",
                    "matrix",
                    "card",
                    "clusteredBar",
                    "clusteredColumn",
                    "line",
                    "slicer",
                ]),
                "table, matrix, card, clusteredBar, clusteredColumn, line or slicer",
            ),
            opt(
                "page",
                Kind::Page,
                "the Page's id or name (default: the first Page; a report with none gets `Page 1`)",
            ),
            opt(
                "x",
                Kind::Integer,
                "position in logical pixels, 0 to 100000 (default 0)",
            ),
            opt(
                "y",
                Kind::Integer,
                "position in logical pixels, 0 to 100000 (default 0)",
            ),
            opt("w", Kind::Integer, "size, 1 to 100000 (default 400 × 300)"),
            opt("h", Kind::Integer, "size, 1 to 100000 (default 400 × 300)"),
        ],
        enabled: |_| true,
        run: Run::Edit(report::new_visual),
    },
    // On the canvas and in the Visualizations pane.
    Command {
        id: "visual.delete",
        label: "Delete",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::delete_visual),
    },
    Command {
        id: "visual.move",
        label: "Move",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req("x", Kind::Integer, "the new position, 0 to 100000"),
            req("y", Kind::Integer, "the new position, 0 to 100000"),
            opt(
                "merge",
                Kind::Bool,
                "join the previous move of the same object into one undo step (arrow-key nudges; default false)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::move_visual),
    },
    Command {
        id: "visual.resize",
        label: "Resize",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req("w", Kind::Integer, "the new size, 1 to 100000"),
            req("h", Kind::Integer, "the new size, 1 to 100000"),
            opt(
                "merge",
                Kind::Bool,
                "join the previous move of the same object into one undo step (arrow-key nudges; default false)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::resize_visual),
    },
    // The Visualizations pane: a type picked with a Visual selected.
    Command {
        id: "visual.changeType",
        label: "Change visual type",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req(
                "type",
                Kind::Choice(&[
                    "table",
                    "matrix",
                    "card",
                    "clusteredBar",
                    "clusteredColumn",
                    "line",
                    "slicer",
                ]),
                "the new type; Fields in wells it has are kept, the others dropped",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::change_type),
    },
    // The field wells: drop a Field, or tick it in the Fields pane.
    Command {
        id: "visual.addField",
        label: "Add field",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req(
                "well",
                Kind::Choice(&["values", "rows", "columns", "axis", "legend", "field"]),
                "values, rows, columns, axis, legend or field (one the type has)",
            ),
            req(
                "field",
                Kind::Field,
                "a Column or Measure of the model; only values takes a Measure; a one-field well has its Field replaced",
            ),
            opt(
                "index",
                Kind::Integer,
                "position in the well (default: last)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::add_field),
    },
    Command {
        id: "visual.removeField",
        label: "Remove field",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req(
                "well",
                Kind::Choice(&["values", "rows", "columns", "axis", "legend", "field"]),
                "the well",
            ),
            opt(
                "index",
                Kind::Integer,
                "the Field's position in it (or give field)",
            ),
            opt("field", Kind::Field, "the Field (its first occurrence)"),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::remove_field),
    },
    Command {
        id: "visual.moveField",
        label: "Move field",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req(
                "well",
                Kind::Choice(&["values", "rows", "columns", "axis", "legend", "field"]),
                "the well it is in",
            ),
            req("index", Kind::Integer, "its position there"),
            opt(
                "toWell",
                Kind::Choice(&["values", "rows", "columns", "axis", "legend", "field"]),
                "the well to move it to (default: the same)",
            ),
            opt(
                "toIndex",
                Kind::Integer,
                "its new position there (default: last)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::move_field),
    },
    // A Visual's More options > Sort axis / Sort by.
    Command {
        id: "visual.sort",
        label: "Sort by",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            null(req(
                "field",
                Kind::Field,
                "a Field of the Visual; null: the type's default sort",
            )),
            opt(
                "direction",
                Kind::Choice(&["ascending", "descending"]),
                "ascending or descending (default descending)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::sort),
    },
    // The Format pane.
    Command {
        id: "visual.format",
        label: "Format",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the Visual's id"),
            req(
                "option",
                Kind::Choice(&[
                    "title",
                    "showTitle",
                    "dataLabels",
                    "legend",
                    "legendPosition",
                    "rowSubtotals",
                    "columnSubtotals",
                    "slicerStyle",
                ]),
                "title (text, or null for the automatic title), showTitle, dataLabels (charts), legend (charts), legendPosition (charts: top, bottom, left or right), rowSubtotals and columnSubtotals (matrix), slicerStyle (slicer: list or dropdown); the others are logical",
            ),
            req("value", Kind::Json, "the option's value"),
        ],
        enabled: has_visuals,
        run: Run::Edit(report::format),
    },
    // Selection (M4.5): slicer items are saved and undoable; data points are session-only.
    Command {
        id: "slicer.select",
        label: "Select slicer items",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req("visual", Kind::Visual, "the slicer's id"),
            req(
                "items",
                Kind::Json,
                "list — values of the slicer's Column (text, number, logical; null for BLANK; dates as their serial number); empty clears the selection",
            ),
            opt(
                "add",
                Kind::Bool,
                "toggle these items in the selection instead of replacing it (Ctrl; default false)",
            ),
        ],
        enabled: has_visuals,
        run: Run::Edit(selection::slicer_select),
    },
    Command {
        id: "selection.select",
        label: "Select data point",
        ribbon: "",
        shortcut: None,
        params: &[
            req("page", Kind::Page, "the Page's id or name"),
            req(
                "visual",
                Kind::Visual,
                "the Visual's id (not a slicer: use slicer.select)",
            ),
            req(
                "point",
                Kind::Json,
                "{row, series} — a row of the Visual's result (and its series: the cell index j × values + k), or {keys: [{table, column, value}, …]} — the Columns and values",
            ),
            opt(
                "add",
                Kind::Bool,
                "toggle the point in the selection instead of replacing it (Ctrl; default false); selecting the only selected point again clears",
            ),
        ],
        enabled: has_visuals,
        run: Run::App(selection::select),
    },
    Command {
        id: "selection.clear",
        label: "Clear selection",
        ribbon: "",
        shortcut: None,
        params: &[opt(
            "page",
            Kind::Page,
            "the Page's id or name (default: every Page)",
        )],
        enabled: |_| true,
        run: Run::App(selection::clear),
    },
    Command {
        id: "options.autodetectRelationships",
        label: "Autodetect new relationships after data is loaded",
        // File > Options and settings > Options > Current file > Data Load.
        ribbon: "",
        shortcut: None,
        params: &[opt("enabled", Kind::Bool, "default: the opposite of now")],
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
        params: &[opt("minutes", Kind::Integer, "0 (off) to 1440; default 5")],
        enabled: |_| true,
        run: Run::App(recovery::set_minutes),
    },
];

/// Every command as Markdown: id, label, where it is, its shortcut and its params. Kept in
/// `docs/commands.md`, which a test holds to this.
pub fn reference() -> String {
    let mut out = String::from(
        "# Commands\n\nGenerated from the engine's command table (`ODASH_SNAPSHOT_UPDATE=1 cargo test -p \
         opendash-engine commands_doc`); do not \
         edit. `?` marks an optional param.\n",
    );
    for c in COMMANDS {
        out.push_str(&format!("\n## `{}` — {}\n\n", c.id, c.label));
        if !c.ribbon.is_empty() {
            out.push_str(&format!("- Ribbon: {}\n", c.ribbon));
        }
        if let Some(key) = c.shortcut {
            out.push_str(&format!("- Shortcut: {key}\n"));
        }
        if c.params.is_empty() {
            out.push_str("- No params\n");
        } else {
            for line in crate::param::doc(c.params).lines() {
                out.push_str(&format!("- `{}\n", line.replacen(": ", "`: ", 1)));
            }
        }
    }
    out
}
