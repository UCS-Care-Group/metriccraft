# OpenDash BI

![stage](https://img.shields.io/badge/stage-pre--alpha%20%C2%B7%20M0%20done-orange)
![DAX parity](https://img.shields.io/badge/DAX%20MVP%20functions-0%20of%2017-lightgrey)
![parity checks](https://img.shields.io/badge/parity%20checks-0%20of%2028%20passing-lightgrey)
![tests](https://img.shields.io/badge/tests-300%20passing-brightgreen)
![unsafe](https://img.shields.io/badge/unsafe-forbidden-success)
![rust](https://img.shields.io/badge/rust-1.98%2B%20%C2%B7%20edition%202024-b7410e)
![platforms](https://img.shields.io/badge/platforms-Linux%20%C2%B7%20Windows%20%C2%B7%20macOS%20%C2%B7%20WASM-informational)
![licence](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)

A clean-room, open-source, Rust-native alternative to Microsoft Power BI Desktop: connect to
data, shape it, model it, and build interactive reports. DAX results match Power BI; your work
saves to an open, text-based file. Native on Linux, Windows and macOS; a reduced web build via WASM.

**Status:** specified; foundations in progress. Scope: [`docs/spec/`](docs/spec/). Plan:
[`docs/plan/`](docs/plan/). Contributor and agent rules: [`AGENTS.md`](AGENTS.md).

## Milestones

| Milestone | Scope                                                              | State                                                               |
|-----------|--------------------------------------------------------------------|---------------------------------------------------------------------|
| M0        | **Foundations**: workspace, CI gates, command framework, benches   | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M1        | **Connect + shape**: CSV/XLSX/Parquet, Applied Steps, M            | ![10/12](https://img.shields.io/badge/-10%2F12-yellow)                  |
| M2        | **Model + file**: tables, relationships, Project and Package       | ![7/9](https://img.shields.io/badge/-7%2F9-yellow)                      |
| M3        | **DAX**: evaluator, MVP functions, parity cases                    | ![not started](https://img.shields.io/badge/-not%20started-lightgrey) |
| M4        | **Visuals**: table, matrix, bar/column, line, card, slicer         | ![not started](https://img.shields.io/badge/-not%20started-lightgrey) |
| M5        | **Desktop UI**: ribbon, Power Query editor, model view, canvas     | ![not started](https://img.shields.io/badge/-not%20started-lightgrey) |
| M6        | **Control, CLI, MCP, web, release**                                | ![not started](https://img.shields.io/badge/-not%20started-lightgrey) |

Not affiliated with or endorsed by Microsoft. Power BI is a trademark of Microsoft Corporation.

Licensed under MIT OR Apache-2.0, at your option.
