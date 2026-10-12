# Observing Power BI Desktop (parity workflow)

How expected values for **Parity cases** (`CONTEXT.md`) are obtained. The storage format for cases
is ADR 0004: one markdown file per case in `parity/observed/`.

## Clean-room rules

- Observe Power BI Desktop **black-box only**: type into its UI, read what it shows.
- Never open, read, disassemble or copy anything from the Power BI install or from files it writes
  (`.pbix`, `.pbip`, caches, logs).
- Never commit a file Power BI produced. Commit only **values you read off the screen**.
- Datasets are synthetic and written by us (inline in the case file, or generated in code).
  Never use Microsoft samples (Contoso, AdventureWorks…).
- Screenshots are allowed for your own reference, kept in `observations/` (gitignored) or inside
  the VM. Never committed.

## Environment

- The Windows VM on the workstation, with Power BI Desktop under Martin's licence.
- Record for every observation session: **Power BI Desktop version** (Help → About), **date**,
  and **regional settings** (File → Options and settings → Options → Regional settings), since
  number and date rendering depend on them.

## Running a case

0. **Get the copyable files.** On the host, `cargo xtask parity --export` clears
   `observations/cases/` and writes one folder per case: `query.dax`, each *Enter data* table as
   `<Table>.tsv` (CRLF, for Notepad), and a `README.txt` with that case's steps. Shaping cases
   (below) add their CSV files and M.
1. **New report.** File → New. One case per report; don't reuse models between cases.
2. **Load the dataset.** Home → Enter data. Paste the case's table (tab-separated, header row),
   name the table as the case says, then *Edit* (Power Query) and set each column's type as the
   case says. Empty cells must show `null` in Power Query; if any shows `Error`, stop and note it.
   Close & Apply.
3. **Check the data.** In Table view, confirm row count and that blanks show as empty cells.
4. **Run the query.** DAX query view → paste the case's `EVALUATE` query → Run.
5. **Copy the whole result** (select all in the grid, copy). Don't read values off the grid: it
   rounds numbers to 2 decimal places and shows infinity as an empty cell; the copy is exact.
6. **Paste it verbatim** into the case's ```` ```tsv ```` block under *Observed* (keep trailing
   tabs: they are blank cells), with the version / date / regional settings line above it.
7. **Check it:** `cargo xtask parity` must not report the case as malformed.

## Things to note while observing

Write these down whenever you notice them; they are parity facts too:

- how booleans, dates and very small/large numbers display;
- any warning or error text, verbatim (and how an error cell copies).

## Shaping cases (`Source: csv`)

Cases 0003–0028 check Get Data, the Culture, Cell errors, every Applied Step and our M against
Power BI (ADR 0004, "Shaping cases"; issue #23). They run **now**, without a DAX evaluator: the
runner loads each CSV through our real Get Data, applies the case's M, sorts by the `ORDER BY`
and compares with the copied result. Until a case is observed it has no Checks.

### Conventions

- `Source: csv Sales.csv` + a ```` ```csv ```` block is the file, byte-exact (LF line ends). The
  Query and table are named after the file (`Sales`).
- `Culture: en-GB` is the *Locale for import* to set before Get Data.
- `## M` holds a Query's whole M exactly as OpenDash BI prints it, with the file named bare
  (`File.Contents("Sales.csv")`); the gate fails if it isn't our printer's output. `--export`
  writes it as `<Query>.m` with the **VM path**: copy `observations/cases/` to
  **`C:\opendash-parity\`** in the VM, so `0003-csv-get-data/Sales.csv` is
  `C:\opendash-parity\0003-csv-get-data\Sales.csv`. That folder is fixed: the `Get Data M`
  Check compares paths too.

### Running a shaping case

The case folder's `README.txt` lists these steps with the names filled in.

1. **Copy** `observations/cases/` (after `cargo xtask parity --export`) to `C:\opendash-parity\`.
2. **New report**, then File → Options and settings → Options → *Current File* → Regional
   Settings → **Locale for import** = the case's `Culture`.
3. **Get Data → Text/CSV** for each CSV in the case folder → **Transform Data** (for the second
   file, Power Query's New Source → Text/CSV). Leave the File Origin and delimiter as detected.
4. For each Query, open the **Advanced Editor**, copy all, and paste it into the case's Observed
   block as `Get Data M: <Query>` followed by an ```` ```m ```` block. This is Power BI's own M
   (detection, `Csv.Document` options, spacing).
5. **If the case has `## M`**: in that Query's Advanced Editor select all, paste `<Query>.m`
   (already pointing at `C:\opendash-parity\…`), Done. Check that it loads without error and
   that **Applied Steps shows the same step names** as the `#"…"` names in the M. Record
   `M accepted: yes`, or `M accepted: no — <what happened, error text verbatim>`. If Power BI
   rewrites the M when you reopen the Advanced Editor, say so after `no` and paste what it shows
   as a note (not a Check).
6. **Close & Apply.** Record `Load errors: <Query> <n>` for every Query: the error count the load
   dialog reports (`0` when there are none; note whether it counts rows or cells, if it says).
7. **DAX query view**: paste `query.dax`, Run, copy the whole result, and paste it as the
   ```` ```tsv ```` block, as for any case. Headers may copy as `Sales[Id]` or `[Id]`; both parse.
8. On the host: `cargo xtask parity` shows the new Checks; `cargo xtask parity --update` records
   the ones that pass, committed with the observation.

An Observed block for a shaping case looks like:

    Power BI Desktop version: … · Date: … · Regional settings: English (United Kingdom)

    M accepted: yes
    Load errors: Sales 2

    Get Data M: Sales

    ```m
    let
        Source = Csv.Document(File.Contents("C:\opendash-parity\0009-cell-errors\Sales.csv"),…
    …
    ```

    ```tsv
    Sales[Id]	Sales[Qty]	Sales[Price]
    …
    ```

Checks per shaping case: one per result column, `Column order`, `Get Data M <Query>` and
`Load errors <Query>` per Query, and `M accepted` when it has `## M`.

### Our guesses the first observations settle

- **Dates and datetimes** copy as `dd/MM/yyyy HH:mm:ss` (UK settings; a date at midnight). Every
  case with a date column (0004–0008, 0019, 0024, 0028) shows the real form; fix the renderer in
  `crates/model/src/parity/shaping.rs` once, then re-run.
- `ORDER BY` puts blanks first and compares text case-insensitively.
- NaN copies as `NaN`; large numbers copy without an exponent.

### Cases and what each settles

| Case | Settles                                                                                                |
|------|--------------------------------------------------------------------------------------------------------|
| 0003 | Get Data's M for a plain CSV (`Csv.Document` options, `QuoteStyle`, spacing); basic detection          |
| 0004 | Type detection: whole vs decimal `1.0`, `TRUE`/`false`, ISO date/datetime, mixed → text, empty         |
| 0005 | en-GB: `03/04/2026` = 3 April, `1,234.5`, `1,2` not grouped by three                                   |
| 0006 | en-US on the same file: `03/04/2026` = 4 March                                                         |
| 0007 | de-DE numbers and dates; `;` delimiter detection                                                       |
| 0008 | Two-digit years: 49 → 2049, 50 → 1950                                                                  |
| 0009 | Cell errors load as blank; the count (cells vs rows); empty ≠ error; `2.5` → whole rounds half to even |
| 0010 | All-text file: does Get Data promote headers? Explicit Promoted Headers after Changed Type             |
| 0011 | Duplicate and blank header names (`Name_1`, `Column4`); blank header stops auto-promotion?             |
| 0012 | Blank lines: skipped, or rows of nulls                                                                 |
| 0013 | Quoted line breaks; Get Data's `QuoteStyle`; how a quote copies; Removed Columns                       |
| 0014 | Ragged rows: `Columns=` = widest row; short rows null; short header → `Column4`                        |
| 0015 | UTF-8 without BOM: `Encoding=65001` or 1252                                                            |
| 0016 | Renamed Columns (two in one step)                                                                      |
| 0017 | Removed Columns                                                                                        |
| 0018 | Reordered Columns listing a subset: where unlisted columns end up                                      |
| 0019 | Changed Type between typed columns: whole→text, decimal→whole, logical→text, date→text, datetime→date  |
| 0020 | Filter `> 2` drops nulls                                                                               |
| 0021 | Filter `<> 3` keeps nulls                                                                              |
| 0022 | Filter `= null`                                                                                        |
| 0023 | `Text.StartsWith` / `Text.Contains`, case-sensitive; `or` printing                                     |
| 0024 | `and` of text, negative number (`-1`) and `#date` conditions; their printing                           |
| 0025 | Merge left outer: duplicate keys, null = null, row order, expand naming                                |
| 0026 | Merge inner                                                                                            |
| 0027 | Merge full outer: unmatched other-side rows appended                                                   |
| 0028 | Changed Type with a culture (`, "en-US"`): parsing and how the argument prints                         |

### Not yet a case

- **Encodings other than UTF-8** (Windows-1252 fallback, codepages beyond 65001/1252/1200/1201):
  a case's CSV is markdown text, so always UTF-8. Needs a generated file.
- **Promoted Headers on a header-only file**, **removing every column**, **filters comparing
  different types** (`[Region] > 3`) and **merge keys of different types** (`1` vs `"1"`): the
  result is an error or an empty table, which the copy-paste format can't record yet.
- **`#datetime` literals** in filters, and how Power Query prints **step names it renames**.
- **Power Query's own step merging** (a rename after a rename becomes one step; Changed Type
  stacking asks first): UI behaviour, observed when the Power Query editor lands (M5.2).

### XLSX follow-up (no cases yet)

XLSX cases need a workbook built by hand in Excel in the VM (we never commit Microsoft-produced
files) or generated in code; deferred. Open questions from `xlsx.rs` / `source.rs`:

- whether Power Query keeps a typed Excel column's type, and detects types from text cells in a
  sheet at all (we detect from text, as for CSV);
- columns mixing numbers and text (we make them text, numbers as Rust prints them, dates ISO);
- Excel error cells (`#DIV/0!`, `#N/A`) loading as blank and counted as Cell errors;
- header promotion of a sheet; duplicate/blank header names in sheets and Excel tables
  (`unique_names` in `xlsx.rs`: Power Query may trim or number differently);
- the navigation step (`Source{[Item="Sheet1",Kind="Sheet"]}[Data]`) name and spacing;
- dates stored as numbers with a date format, and the 1900 leap-year bug.
