# Commands

Generated from the engine's command table (`MCRAFT_SNAPSHOT_UPDATE=1 cargo test -p metriccraft-engine commands_doc`); do not edit. `?` marks an optional param.

## `edit.undo` — Undo

- Shortcut: Ctrl+Z
- No params

## `edit.redo` — Redo

- Shortcut: Ctrl+Y
- No params

## `file.new` — New

- Ribbon: File
- Shortcut: Ctrl+N
- No params

## `file.open` — Open

- Ribbon: File
- Shortcut: Ctrl+O
- `path`: path — a Project folder, its project.mcraft.json, or a .mcraft Package
- `recover?`: staging | lastGood — after an interrupted save only: staging (the save that was being made) or lastGood (the state before it)

## `file.save` — Save

- Ribbon: File
- Shortcut: Ctrl+S
- `path?`: path — save here instead, as Save As does (default: where it was opened or last saved)

## `file.saveAs` — Save as

- Ribbon: File
- `path`: path — the Project folder or .mcraft Package to write
- `package?`: logical — write a .mcraft Package (default: whether path ends in .mcraft)
- `data?`: logical — include the Data cache in a Package (default true; a Project folder always keeps it)

## `file.remapSource` — Remap source path

- `from`: path — a Source file path as the Project's Queries name it (relative to the Project folder, or absolute)
- `to`: path or null — read it from here on this machine instead (relative to the Project folder, or absolute); null removes the remap. Kept in .local/, never shared

## `file.recover` — Recover

- `id`: text — the recovery copy, as listed at launch
- `dir?`: path — the recovery folder (default: the user's, e.g. ~/.cache/metriccraft/recovery)

## `file.discardRecovery` — Discard

- `id`: text — the recovery copy, as listed at launch
- `dir?`: path — the recovery folder (default: the user's, e.g. ~/.cache/metriccraft/recovery)

## `home.getData` — Get Data

- Ribbon: Home > Data
- `kind`: csv | parquet | xlsx — csv, parquet or xlsx
- `path`: path — the file
- `name?`: text — the Query and table name (default: the file name without its extension, or the sheet/table)
- `culture?`: text — reads numbers and dates, e.g. en-GB (default: en-US)
- `delimiter?`: text — CSV: one character, \t for tab (default: detected)
- `encoding?`: utf-8 | windows-1252 | utf-16le | utf-16be — CSV: utf-8, windows-1252, utf-16le or utf-16be (default: detected)
- `item?`: text — XLSX: the sheet or table to read (required)
- `itemKind?`: sheet | table — XLSX: sheet or table (default: sheet)

## `home.refresh` — Refresh

- Ribbon: Home > Queries
- No params

## `home.closeApply` — Close & Apply

- Ribbon: Power Query Editor: Home > Close
- No params

## `transform.useFirstRowAsHeaders` — Use First Row as Headers

- Ribbon: Transform > Table
- `query`: Query — the Query

## `transform.changeType` — Data Type

- Ribbon: Transform > Any Column
- `query`: Query — the Query
- `columns`: list of text — the columns to change
- `type`: text | decimal | fixedDecimal | whole | date | datetime | boolean — text, decimal, fixedDecimal, whole, date, datetime or boolean

## `transform.renameColumn` — Rename

- Ribbon: Transform > Any Column
- Shortcut: F2
- `query`: Query — the Query
- `column`: text — the column to rename
- `name`: text — its new name, unique in the table

## `home.removeColumns` — Remove Columns

- Ribbon: Power Query Editor: Home > Manage Columns
- Shortcut: Delete
- `query`: Query — the Query
- `columns`: list of text — the columns to remove (at least one must remain)

## `transform.reorderColumns` — Move

- Ribbon: Transform > Any Column
- `query`: Query — the Query
- `columns`: list of text — these columns, in this order, fill the positions they hold; the others stay put

## `transform.filterRows` — Filter Rows

- Ribbon: Power Query Editor: Home > Reduce Rows
- `query`: Query — the Query
- `conditions`: JSON — list of {column: text, op: text, value} — op is equals, notEquals, greater, greaterOrEqual, less, lessOrEqual, contains, beginsWith or endsWith; value is a number, text, logical or null (equals null = is null); text for a date or datetime column is read as one
- `combine?`: and | or — and (default) or or

## `home.mergeQueries` — Merge Queries

- Ribbon: Power Query Editor: Home > Combine
- `query`: Query — the Query to merge into
- `keys`: list of text — its key columns
- `with`: Query — the other Query
- `withKeys`: list of text — the other Query's key columns, paired with keys in order
- `kind?`: leftOuter | inner | fullOuter — leftOuter (default), inner or fullOuter
- `column?`: text — the merged column's name (default: the other Query's name)
- `expand?`: list of text — the other Query's columns to add (default: all)
- `prefix?`: logical — name added columns `column.name` (default true)

## `query.deleteStep` — Delete

- `query`: Query — the Query
- `step`: integer — the step's position in Applied Steps, 0 = first

## `query.enableLoad` — Enable load

- `query`: Query — the Query
- `enabled?`: logical — load it into the model (default: the opposite of now)

## `query.rename` — Rename

- `query`: Query — the Query (and its Table)
- `name`: text — its new name, unique among Queries ignoring case; rewrites Relationships and other Queries' merges (same as modeling.renameTable)

## `query.delete` — Delete

- `query`: Query — the Query; its Table and Relationships go too (refused while another Query merges it)

## `query.duplicate` — Duplicate

- `query`: Query — the Query to copy, as `<query> (2)` (the next free number); loaded at the next Close & Apply

## `columnTools.dataType` — Data type

- Ribbon: Column tools > Formatting
- `table`: Table — the Table
- `column`: Column — the Column
- `type`: text | decimal | fixedDecimal | whole | date | datetime | boolean — decimal, fixedDecimal, whole, datetime, date, text or boolean; converts the loaded data (a value that doesn't convert fails the change), the Query is untouched

## `columnTools.format` — Format

- Ribbon: Column tools > Formatting
- `table`: Table — the Table
- `column`: Column — the Column or Measure
- `formatString`: text — a format string, e.g. #,0.00 (empty: the default)

## `columnTools.summarization` — Summarization

- Ribbon: Column tools > Properties
- `table`: Table — the Table
- `column`: Column — the Column
- `summarizeBy`: none | sum | average | min | max | count | distinctCount — none, sum, average, min, max, count or distinctCount (sum and average need a number column)

## `columnTools.sortByColumn` — Sort by column

- Ribbon: Column tools > Sort
- `table`: Table — the Table
- `column`: Column — the Column to sort
- `by`: Column or null — another column of the Table with one value per value of this one; null sorts by the column itself

## `model.setHidden` — Is hidden

- `table`: Table — the Table
- `column?`: Column — one of its Columns or Measures (default: the Table itself)
- `hidden?`: logical — default true

## `model.setDescription` — Description

- `table`: Table — the Table
- `column?`: Column — one of its Columns or Measures (default: the Table itself)
- `description`: text — empty clears it

## `model.moveTable` — Move table

- `table`: Table — the Table
- `x`: integer — its box's new left edge in Model view, 0 to 100000
- `y`: integer — its box's new top edge, 0 to 100000
- `merge?`: logical — join the previous move of the same object into one undo step (arrow-key nudges; default false)

## `modeling.renameTable` — Rename

- Shortcut: F2
- `table`: Table — the Table (and its Query)
- `name`: text — its new name, unique among Queries ignoring case; rewrites Relationships and other Queries' merges (same as query.rename)

## `modeling.renameColumn` — Rename

- Shortcut: F2
- `table`: Table — the Table
- `column`: Column — the Column
- `name`: text — its new name, unique in the Table ignoring case; adds a Renamed Columns step to the Query and rewrites sort by, Relationships and other Queries' merges

## `modeling.deleteTable` — Delete from model

- `table`: Table — the Table; its Query and Relationships go too (refused while another Query merges it)

## `modeling.newMeasure` — New measure

- Ribbon: Modeling > Calculations
- `table`: Table — the Measure's home Table
- `name`: text — unique among the Model's Measures, not a Column name of the Table (ignoring case)
- `expression`: text — its DAX, kept as written (one that doesn't parse is kept, with its error)
- `formatString?`: text — optional, e.g. #,0.00
- `description?`: text — optional

## `modeling.newColumn` — New column

- Ribbon: Modeling > Calculations
- `table`: Table — the Table
- `name`: text — unique in the Table, not one of its Measures (ignoring case)
- `expression`: text — its DAX, evaluated for each row; the data type comes from the values

## `modeling.editExpression` — Edit formula

- `table`: Table — the Table
- `name`: text — one of its Measures or Calculated columns
- `expression`: text — the new DAX

## `modeling.delete` — Delete from model

- Shortcut: Delete
- `table`: Table — the Table
- `name`: text — one of its Measures or Calculated columns (a Calculated column's Relationships go too; DAX that used it is kept and shows an error)

## `modeling.renameMeasure` — Rename

- Shortcut: F2
- `table`: Table — the Measure's home Table
- `measure`: Measure — the Measure
- `name`: text — its new name; rewrites every Measure and Calculated column that uses it

## `modeling.newRelationship` — New relationship

- Ribbon: Modeling > Relationships
- `fromTable`: Table — the many side's Table
- `fromColumn`: Column — its column
- `toTable`: Table — the one side's Table
- `toColumn`: Column — its column: unique, non-blank values
- `active?`: logical — default true; made inactive (with a notice) if the two Tables already have an active Relationship
- `orient?`: logical — default false; true swaps the ends when only that way round is valid (as dragging a column onto another in the Model view does)

## `modeling.editRelationship` — Edit relationship

- `relationship`: JSON — integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns
- `fromTable?`: Table — the new ends (default: as now)
- `fromColumn?`: Column — the new ends (default: as now)
- `toTable?`: Table — the new ends (default: as now)
- `toColumn?`: Column — the new ends (default: as now)
- `active?`: logical — default as now; made inactive (with a notice) if the two Tables already have another active Relationship

## `modeling.deleteRelationship` — Delete relationship

- `relationship`: JSON — integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns

## `modeling.setRelationshipActive` — Active

- `relationship`: JSON — integer or {fromTable, fromColumn, toTable, toColumn} — the Relationship: its position (0 = first) or its columns
- `active?`: logical — default true; activating fails while the two Tables have another active Relationship (deactivate it first)

## `modeling.autodetectRelationships` — Autodetect

- No params

## `insert.newPage` — New page

- Ribbon: Insert > Pages
- `name?`: text — the Page's name, unique ignoring case (default: Page n, the first free n)

## `page.setCurrent` — Go to page

- `page`: Page — the Page's id or name

## `page.delete` — Delete page

- `page`: Page — the Page's id or name

## `page.rename` — Rename page

- `page`: Page — the Page's id or name
- `name`: text — its new name, unique among Pages ignoring case

## `page.move` — Move page

- `page`: Page — the Page's id or name
- `index`: integer — its new position, 0 = first

## `insert.newVisual` — New visual

- Ribbon: Insert > Visuals
- `type`: table | matrix | card | clusteredBar | clusteredColumn | line | slicer — table, matrix, card, clusteredBar, clusteredColumn, line or slicer
- `page?`: Page — the Page's id or name (default: the first Page; a report with none gets `Page 1`)
- `x?`: integer — position in logical pixels, 0 to 100000 (default 0)
- `y?`: integer — position in logical pixels, 0 to 100000 (default 0)
- `w?`: integer — size, 1 to 100000 (default 400 × 300)
- `h?`: integer — size, 1 to 100000 (default 400 × 300)

## `visual.delete` — Delete

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id

## `visual.move` — Move

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `x`: integer — the new position, 0 to 100000
- `y`: integer — the new position, 0 to 100000
- `merge?`: logical — join the previous move of the same object into one undo step (arrow-key nudges; default false)

## `visual.resize` — Resize

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `w`: integer — the new size, 1 to 100000
- `h`: integer — the new size, 1 to 100000
- `merge?`: logical — join the previous move of the same object into one undo step (arrow-key nudges; default false)

## `visual.changeType` — Change visual type

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `type`: table | matrix | card | clusteredBar | clusteredColumn | line | slicer — the new type; Fields in wells it has are kept, the others dropped

## `visual.addField` — Add field

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `well`: values | rows | columns | axis | legend | field — values, rows, columns, axis, legend or field (one the type has)
- `field`: Field — a Column or Measure of the model; only values takes a Measure; a one-field well has its Field replaced
- `index?`: integer — position in the well (default: last)

## `visual.removeField` — Remove field

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `well`: values | rows | columns | axis | legend | field — the well
- `index?`: integer — the Field's position in it (or give field)
- `field?`: Field — the Field (its first occurrence)

## `visual.moveField` — Move field

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `well`: values | rows | columns | axis | legend | field — the well it is in
- `index`: integer — its position there
- `toWell?`: values | rows | columns | axis | legend | field — the well to move it to (default: the same)
- `toIndex?`: integer — its new position there (default: last)

## `visual.sort` — Sort by

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `field`: Field or null — a Field of the Visual; null: the type's default sort
- `direction?`: ascending | descending — ascending or descending (default descending)

## `visual.format` — Format

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id
- `option`: title | showTitle | dataLabels | legend | legendPosition | rowSubtotals | columnSubtotals | slicerStyle — title (text, or null for the automatic title), showTitle, dataLabels (charts), legend (charts), legendPosition (charts: top, bottom, left or right), rowSubtotals and columnSubtotals (matrix), slicerStyle (slicer: list or dropdown); the others are logical
- `value`: JSON — the option's value

## `slicer.select` — Select slicer items

- `page`: Page — the Page's id or name
- `visual`: Visual — the slicer's id
- `items`: JSON — list — values of the slicer's Column (text, number, logical; null for BLANK; dates as their serial number); empty clears the selection
- `add?`: logical — toggle these items in the selection instead of replacing it (Ctrl; default false)

## `selection.select` — Select data point

- `page`: Page — the Page's id or name
- `visual`: Visual — the Visual's id (not a slicer: use slicer.select)
- `point`: JSON — {row, series} — a row of the Visual's result (and its series: the cell index j × values + k), or {keys: [{table, column, value}, …]} — the Columns and values
- `add?`: logical — toggle the point in the selection instead of replacing it (Ctrl; default false); selecting the only selected point again clears

## `selection.clear` — Clear selection

- `page?`: Page — the Page's id or name (default: every Page)

## `options.autodetectRelationships` — Autodetect new relationships after data is loaded

- `enabled?`: logical — default: the opposite of now

## `options.autosaveMinutes` — Store AutoRecovery information every N minutes

- `minutes?`: integer — 0 (off) to 1440; default 5

## `dax.evaluate` — Evaluate DAX query

- Read command: changes nothing, not undoable
- `query`: text — a DAX query with one EVALUATE (DEFINE and ORDER BY allowed); the Model's Measures are in scope
- `maxRows?`: integer — most rows returned, 0 to 1000000 (default 10000); `truncated` says if more were left out

## `document.describe` — Describe document

- Read command: changes nothing, not undoable
- No params

## `page.render` — Render page

- Read command: changes nothing, not undoable
- `page?`: Page — the Page (default: the current Page)
- `width?`: number — image width in pixels (overrides scale)
- `scale?`: number — pixels per Page pixel (default 1)

## `batch` — Batch

- `commands`: JSON — a list of {"run": command id, "params": {...}}, run in order, all or nothing (no file.*, edit.* or batch); at most 10000
- `label?`: text — the undo label (default: how many changes)
