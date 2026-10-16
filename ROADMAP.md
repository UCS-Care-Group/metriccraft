# Roadmap

**Stage:** pre-alpha, working towards the first preview build (`v0.1.0-preview.1`) and then v0.1.

v0.1 is an authoring app: connect to files, shape them in a Power Query editor, model
relationships, write DAX measures and build interactive report pages, saved to an open,
text-based Project. Publishing, sharing and scheduled refresh (Power BI's Service half) are a
later, separate product.

## Headline numbers

Measured on the reference machine (AMD Ryzen 5 5600X, 32 GB, Linux); details in
[`docs/bench.md`](docs/bench.md).

| Measure                                          | Target   | Now                |
|--------------------------------------------------|----------|--------------------|
| Slicer change → every visual refreshed, 10M rows | < 100 ms | 66–70 ms           |
| Import 10M rows × 20 columns from Parquet        | < 10 s   | met                |
| Cold start to a window                           | < 1 s    | 338 ms             |
| Idle memory (private)                            | ≤ 100 MB | 70 MB              |
| Release archive                                  | ≤ 50 MB  | 10.5–22.0 MB       |

## Milestones

| Milestone | Scope                                                                        | State       |
|-----------|------------------------------------------------------------------------------|-------------|
| M0        | Foundations: workspace, CI gates, command framework, benches                 | done        |
| M1        | Connect + shape: CSV/XLSX/Parquet, Applied Steps, M                          | 11/13       |
| M2        | Model + file: tables, relationships, Project and Package                     | done        |
| M3        | DAX: evaluator, MVP functions, parity cases                                  | done        |
| M4        | Visuals: table, matrix, bar/column, line, card, slicer                       | done        |
| M5        | Desktop UI: ribbon, Power Query editor, model view, canvas                   | done        |
| M6        | Control channel, CLI, MCP, web build, release                                | 2/7         |

M6 in order: release workflow (done) → Read commands, Batch, Origin (done) → headless CLI →
Control channel → MCP server → web build → fuzz soak, acceptance run and v0.1.0.

## After v0.1

1. Time intelligence (date tables, `TOTALYTD`, `SAMEPERIODLASTYEAR`, `DATEADD`…).
2. PBIP/TMDL import.
3. PostgreSQL source (desktop only).
4. SQL Server source.

## Progress log

- **2026-10-16** — M6.2: Read commands (`dax.evaluate`, `document.describe`, `page.render`),
  Batch (one undo step) and Origin on undo labels. M6.1: release workflow; archives 10.5–22.0 MB.
- **2026-10-15** — M5 done: keyboard navigation, accessibility audit, UI snapshots and budgets.
  CI green on Linux, Windows and macOS hosted runners. Renamed to MetricCraft.
