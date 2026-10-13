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
