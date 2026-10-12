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
