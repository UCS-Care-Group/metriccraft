# Benches

`cargo xtask bench` runs every bench and compares it with `bench/<machine>.json`: a result more
than 1.25× slower, or using more than 1.10× the memory, fails. Machines without a baseline only
report. `--save` re-records the baseline (on tardis2, when it's quiet: load average < 2).

## Import benches (M1.9; spec N-2, N-8; ADR 0005)

`crates/engine/benches/import.rs` runs Get Data (`home.getData`: connect, run, load) on a new App,
each measurement in a fresh process, median of 3. Per case it prints the time, and on Linux the
peak RSS (`/peak`), the RSS afterwards with the table held (`/steady`) and the table's column
buffers (`/table`). `engine/idle/steady` is a new App's RSS (the floor under N-8's 100 MB idle).

| Case                 | File                                         | Run by                          |
|----------------------|----------------------------------------------|---------------------------------|
| `import/parquet_1m`  | 1M rows, generated into `target/bench-data/` | `cargo xtask bench`             |
| `import/csv_200k`    | 200k rows, same                              | `cargo xtask bench`             |
| `import/xlsx_50k`    | 50k rows, same                               | `cargo xtask bench`             |
| `import/parquet_10m` | 10M rows, `observations/reference/`          | `cargo xtask bench --reference` |

Generated files are cached by size; delete `target/bench-data/` after changing the generator
(or bump `dataset::VERSION`).

## DAX benches (M3.10; spec N-3)

`crates/engine/benches/dax.rs` loads each `import/parquet_<n>` file once (Get Data, as above),
then times DAX query evaluation only (parse + evaluate, not the load or building the
evaluator's `Database`), median of 5, on the reference dataset's columns:

| Case                       | Query                                                                                                                      |
|----------------------------|----------------------------------------------------------------------------------------------------------------------------|
| `dax/sum_<n>`              | `ROW("v", SUM(T[SalesAmount]))`: a full decimal-column scan                                                                |
| `dax/calculate_filter_<n>` | `ROW("v", CALCULATE(SUM(T[SalesAmount]), T[Channel] = "Store"))`: a text-column filter (4 values, ~30% match) then the sum |
| `dax/distinctcount_<n>`    | `ROW("v", DISTINCTCOUNT(T[CustomerID]))`: 100,000 distinct whole numbers                                                   |

`<n>` is `1m` (`cargo xtask bench`) and `10m` (`cargo xtask bench --reference`). Budget (N-3):
a slicer change refreshes every visual on the page in < 100 ms on 10M rows, so one visual's
measure must be well under that.

First run (2026-10-14, busy host, load average 8–14; ranges over three 1M and two 10M runs):

| Case                       | 1M          | 10M         |
|----------------------------|-------------|-------------|
| `dax/sum`                  | 2.7–11 ms   | 27–39 ms    |
| `dax/calculate_filter`     | 106–435 ms  | 0.87–2.1 s  |
| `dax/distinctcount`        | 42–416 ms   | 1.0–1.3 s   |

`CALCULATE` and `DISTINCTCOUNT` were far over N-3 (risk row in `docs/plan/0001-mvp.md` §4).

After M3.11's fast paths (2026-10-14, load average 2–7; same file, two runs of each, ranges):

| Case                       | 1M             | 10M            |
|----------------------------|----------------|----------------|
| `dax/sum`                  | 0.70–0.74 ms   | 6.6 ms         |
| `dax/calculate_filter`     | 1.2–1.3 ms     | 12.3–12.7 ms   |
| `dax/distinctcount`        | 1.2 ms         | 12.1–12.2 ms   |

Measured on the same host just before the change (load average ~6): 2.6 / 106 / 42 ms at 1M,
26 / 880 / 414 ms at 10M. Where the time went: the boolean filter's `FILTER(ALL(T[c]), …)`
materialised a `Value` and a folded text key per row to find the column's distinct values
(95% of `CALCULATE`); `DISTINCTCOUNT` hashed a `Key` per row; `SUM` of a decimal column paid
for the integer overflow checks per value. Now: distinct values come from distinct dictionary
codes (stopping once every code is seen), the filter's mask is built a 64-row word at a time
from the integer codes, `DISTINCTCOUNT` of whole numbers or dates is a range bitmap (an integer
hash set when the range is sparse), whole-number value filters compare integers, and a float
`SUM` is a plain loop (the same additions in the same order).

## Page refresh (M4.7; spec N-3)

`crates/engine/benches/page.rs` loads each `import/parquet_<n>` file once and builds the
reference page: a card (Sum of SalesAmount), a clustered bar (Country, 20 values), a line
(OrderDate, ~2,555 days), a table (Product, 5,000 values, 500-row window), a matrix
(Region → Country × Channel, with subtotals) and a Category slicer. Median of 5:

| Case               | What                                                                         |
|--------------------|------------------------------------------------------------------------------|
| `page/refresh_<n>` | a slicer change (`slicer.select`), then every Visual re-queried and laid out |
| `page/layout_<n>`  | the same page again with nothing changed (every query a cache hit)           |

A refresh is 11 group-by queries (`summarize`): one per Visual, the matrix's levels and the
Totals, all over the ~1/12 of the rows the slicer leaves.

**Before** (b8deb84, 2026-10-14, load average ~40; one refresh): `page/refresh_1m` 108 s
(84 s on a quieter run). Where it went, per query at 1M rows: the line chart 106 s, the table
0.9 s, each matrix level 25–360 ms. Every group's expression ran its own scan under an equality
filter (2,555 date groups × a 1M-row `Key` comparison each: date filters compared a `Key` per
row), and listing the groups built a `Value` and a folded text key per row (7–35 ms per query).
10M was not run (extrapolated: over 15 minutes).

**After** (2026-10-14, load average 9; three runs each, ranges):

| Case           | 1M           | 10M          |
|----------------|--------------|--------------|
| `page/refresh` | 12.7–13.7 ms | 65.9–69.7 ms |
| `page/layout`  | 1.9–2.0 ms   | 1.9–2.0 ms   |

N-3 (< 100 ms on 10M rows) is met on tardis2 under that load. What changed:

- **One-pass group-by** (`crates/model/src/dax/summarize_fast.rs`): when every expression is a
  plain aggregation of one Table (directly or through Measures) and the group-by columns are on
  it or one Relationship away, one scan of the visible rows works out each row's group from
  dictionary codes and whole numbers and accumulates every expression per group. Results are
  identical to the per-group path (a differential test checks it); anything else falls back.
- **Threads** (not wasm): the scan's first phase (visible rows, group keys, each aggregation's
  values gathered into flat buffers) runs on up to 4 threads per 64Ki-row batch; the second
  (groups in first-seen order, the additions) stays on one thread in row order, so float sums
  are bit-identical. The scan is memory-bandwidth bound: 10M refresh at load ~10 was
  115–128 ms on 1 thread, 67–95 ms on 2, 62–72 ms on 4, 71–84 ms on 8.
- **Mask memo**: the Visuals of a Page share one filter context, so each Table's visible rows
  are worked out once per slicer change, not once per query (a 10M-row mask is ~6 ms).
- Date value filters compare integers a word at a time, like whole numbers (the per-group
  path's fallback).

Not yet: the second phase (~1.5–3 ns per visible row and query) is the floor of a refresh; it
could skip the group-slot lookup when keys are dense. A group-by column of a big dictionary
folds the whole dictionary per query. Baselines not re-recorded (host load > 2).

## Reference dataset

`cargo xtask dataset [--rows N] [--out DIR] [--csv]` writes `sales_10m.parquet` (and with
`--csv`, `sales_10m.csv`) to `observations/reference/` (gitignored; never commit it). The rows
are the same on every machine: a fixed seed, 20 columns of a sales fact table (schema in
`xtask/src/dataset.rs`): 3 whole-number, 9 text (from 4 to 10M distinct values), 4 decimal, a
date, a datetime and a boolean column, some with nulls. 10M rows is about 460 MB of Parquet.

## Measured on tardis2

| Date       | Case                 | Time   | Peak    | Steady  | Table   |
|------------|----------------------|--------|---------|---------|---------|
| 2026-10-12 | `import/parquet_10m` | 7.5 s  | 1.50 GB | 1.37 GB | 0.99 GB |
| 2026-10-12 | `import/parquet_1m`  | 0.69 s | 173 MB  | 171 MB  | 97 MB   |
| 2026-10-12 | `engine/idle`        |        |         | 4.2 MB  |         |

Measured with other builds running (load average 5–12), so the times are pessimistic.

M3.10 (case fold at load, 2026-10-14, A/B on a busy host, load average ~18): `import/parquet_1m`
about +5–10% time and +5 MB peak (8 bytes per distinct text value, hashed once; 1M distinct
`OrderNumber`s), `import/csv_200k` about +10–15% time, peak unchanged. Baseline not re-recorded (needs a quiet tardis2).

## Power BI Desktop comparison (spec N-8)

In the Windows VM, on the same data:

1. On the host: `cargo xtask dataset`. Copy `observations/reference/sales_10m.parquet` into the
   VM (the folder is shared).
2. Start Power BI Desktop with a blank report. Note the memory of the idle app.
3. **Get Data → Parquet**, enter the file's path, **Load**. (If the dialog won't take a local
   path: **Get Data → Blank Query**, Advanced Editor,
   `Parquet.Document(File.Contents("C:\…\sales_10m.parquet"))`, Close & Apply.) Wait until
   the load finishes and the Data pane shows the table.
4. Task Manager → **Details**: add up the *Memory (private working set)* of every
   `PBIDesktop.exe` process and of `msmdsrv.exe` (close to our RSS, which also counts the
   binary's shared pages). Record the total once it settles (steady) and the highest seen
   while loading (peak).
5. Add a row below. Only values actually observed go here.

| Date | Power BI version | Idle | Peak (load) | Steady (loaded) |
|------|------------------|------|-------------|-----------------|

N-8 is met when our `import/parquet_10m` peak and steady are at most half of Power BI's.
