# MetricCraft

![stage](https://img.shields.io/badge/stage-pre--alpha%20%C2%B7%20M5%20done-orange)
![DAX parity](https://img.shields.io/badge/DAX%20MVP%20functions-17%20of%2017%20implemented-yellow)
![parity checks](https://img.shields.io/badge/parity%20checks-247%20of%20247%20passing-brightgreen)
![tests](https://img.shields.io/badge/tests-476%20passing-brightgreen)
![unsafe](https://img.shields.io/badge/unsafe-forbidden-success)
![rust](https://img.shields.io/badge/rust-1.98%2B%20%C2%B7%20edition%202024-b7410e)
![platforms](https://img.shields.io/badge/platforms-Linux%20%C2%B7%20Windows%20%C2%B7%20macOS%20%C2%B7%20WASM-informational)
![licence](https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-blue)

A clean-room, open-source, Rust-native alternative to Microsoft Power BI Desktop: connect to
data, shape it, model it, and build interactive reports. The goal is DAX results identical to
Power BI's, checked by parity cases whose expected values were observed in Power BI Desktop. Your
work saves to an open, text-based file. Native on Linux, Windows and macOS; a reduced web build via
WASM.

**Status:** pre-alpha. The desktop app connects to CSV/XLSX/Parquet, shapes data in a Power Query
editor, models relationships, evaluates DAX and draws six visual types. Roadmap:
[`ROADMAP.md`](ROADMAP.md). Contributor and agent rules: [`AGENTS.md`](AGENTS.md).

**Limitations:** not yet released, and not a drop-in replacement. It does not open `.pbix`/`.pbip`
files, and supports only file data sources, a subset of DAX and M, and six visuals. There is no
publishing or sharing (the Power BI Service half). Parity is measured only by the cases in this repo.

## Try it

There are no release builds yet; build from source. Install Rust with [rustup](https://rustup.rs)
(the toolchain is pinned in `rust-toolchain.toml` and installs itself), then:

```bash
git clone https://github.com/UCS-Care-Group/metriccraft
cd metriccraft
cargo run --release -p metriccraft             # empty window: Home > Get Data to load a CSV, XLSX or Parquet file
cargo run --release -p metriccraft -- PROJECT  # open a Project folder or .mcraft Package
cargo run --release -p metriccraft-cli -- dax PROJECT 'EVALUATE Sales'  # no window: see docs/cli.md
```

On Linux the app needs a Wayland or X11 session. If OpenGL misbehaves, try the wgpu renderer:
`cargo run --release -p metriccraft --no-default-features --features wgpu`.

Found a bug or a DAX result that differs from Power BI? Please
[open an issue](https://github.com/UCS-Care-Group/metriccraft/issues). See
[`CONTRIBUTING.md`](CONTRIBUTING.md) before sending code.

## Milestones

| Milestone | Scope                                                              | State                                                                 |
|-----------|--------------------------------------------------------------------|-----------------------------------------------------------------------|
| M0        | **Foundations**: workspace, CI gates, command framework, benches   | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M1        | **Connect + shape**: CSV/XLSX/Parquet, Applied Steps, M            | ![11/13](https://img.shields.io/badge/-11%2F13-yellow)                |
| M2        | **Model + file**: tables, relationships, Project and Package       | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M3        | **DAX**: evaluator, MVP functions, parity cases                    | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M4        | **Visuals**: table, matrix, bar/column, line, card, slicer         | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M5        | **Desktop UI**: ribbon, Power Query editor, model view, canvas     | ![done](https://img.shields.io/badge/-done-brightgreen)               |
| M6        | **Control, CLI, MCP, web, release**                                | ![3/7](https://img.shields.io/badge/-3%2F7-yellow)                    |

Not affiliated with or endorsed by Microsoft. Power BI is a trademark of Microsoft Corporation.

Licensed under MIT OR Apache-2.0, at your option.
