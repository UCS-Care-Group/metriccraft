# Observing Power BI Desktop (parity workflow)

How expected values for **Parity cases** (`CONTEXT.md`) are obtained. The storage format for cases
is ADR 0004; until it lands, observed cases live as markdown in `parity/observed/`.

## Clean-room rules

- Observe Power BI Desktop **black-box only**: type into its UI, read what it shows.
- Never open, read, disassemble or copy anything from the Power BI install or from files it writes
  (`.pbix`, `.pbip`, caches, logs).
- Never commit a file Power BI produced. Commit only **values you read off the screen**.
- Datasets are synthetic and written by us (in the case file now; in code once ADR 0004 lands).
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
5. **Record what you see**, column by column, exactly as displayed:
   - an empty cell → `(blank)`;
   - an error → `ERROR: <message>`;
   - numbers: every digit shown, including trailing zeros and separators.
6. **Paste the results into the case file** under *Observed*, with the version / date / regional
   settings line.

## Things to note while observing

Write these down whenever you notice them; they are parity facts too:

- whether the DAX query view rounds or formats numbers (vs full precision);
- how booleans, dates and very small/large numbers display;
- any warning or error text, verbatim.
