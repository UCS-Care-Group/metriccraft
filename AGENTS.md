# MetricCraft — instructions for agents

MetricCraft is a clean-room, open-source, Rust-native business-intelligence authoring app: a Power BI Desktop alternative that connects to data, shapes it, models it, and builds interactive report pages. DAX results match Power BI exactly; the UI follows Power BI's workflow but may improve on it. Desktop (Linux, Windows, macOS) is the product; the web build (WASM) is a reduced, file-only variant. Publishing, sharing and scheduled refresh (the Power BI Service half) is a separate, later product: don't design for it until that is decided.

Independent project. Its engineering rules were adapted from the storytold crafting apps (`storytold/gridcraft`; verbatim copies in `docs/agents/upstream/` for reference only) and are now our own.

## Start every session here
1. `plan/` if it exists: maintainers' local working notes, gitignored, never committed. `plan/CONTEXT.md` is the glossary (use its terms; challenge drift), `plan/spec-*.md` what we're building, `plan/plan-*.md` how and in what order, `plan/adr/` decisions expensive to reverse. Without it, start from `ROADMAP.md` and the issues.
2. Never commit anything from `plan/`, nor copy specs, plans or ADRs into tracked files.
3. Tasks are GitHub issues (`docs/agents/issue-tracker.md`), driven with `gh` (not the GitHub MCP).

Work is planned **Glossary → Specification → Implementation Plan** before code; an ADR for anything expensive to reverse.

## Assets and iconography — absolute rule
**No Microsoft, Adobe, Tableau/Salesforce, Qlik (or any other vendor's) iconography, images, artwork, fonts, templates, themes, visuals or sample files — ever.** This is the most important rule in this repository; breaking it is a serious failure.
- Every asset in the repo is original work by MetricCraft contributors, or public domain / CC0, or open source / Creative Commons that allows redistribution.
- Every asset file has a row in [`ATTRIBUTION.md`](ATTRIBUTION.md) (author, source, licence). Adding an asset without a row is a bug.
- Prefer art generated in code: the UI icon set, report themes, visual palettes and sample datasets are drawn/defined in code and original.
- Never copy Power BI's icons, ribbon artwork, theme JSON, visual definitions, templates, sample `.pbix` files (Contoso, AdventureWorks reports…) or wording beyond feature names. Power BI feature *names* (menu labels, DAX/M function names) are fine.
- Fonts: system fonts, plus at most one bundled OFL-licensed font with an `ATTRIBUTION.md` row. Exception: egui's default fonts (Ubuntu Light, Hack, Noto Emoji, emoji-icon-font), embedded by the `default_fonts` feature, listed in `ATTRIBUTION.md` with their licences in `licenses/egui-default-fonts/`. No others.

## Never crash
People trust MetricCraft with their numbers; a crash loses their work. **This outranks feature work.**
- **No panics in non-test code:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`; no `unsafe` (`unsafe_code = "forbid"`).
- **Errors are `Result<T, E>`** through the crate's error type and `?`.
- **Input-derived numbers are hostile** (files, data sources, DAX/M expressions, commands, MCP/control params): `get()` instead of indexing, checked/saturating arithmetic, cap input-sized allocations (huge tables, cross joins, `GENERATESERIES`, unbounded `List.Generate`).
- **Bound recursion** (expression nesting, measure-references-measure chains, relationship paths).
- **Every crash fix lands with a regression test.**
- Lints are set once in the workspace `Cargo.toml` (`unsafe_code = "forbid"`; clippy `unwrap_used`, `expect_used`, `panic`, `unimplemented`, `todo`, `unreachable` denied); every crate has `[lints] workspace = true`. `clippy.toml` allows panicking only in tests.

## Non-negotiables
- **Clean-room.** Power BI Desktop is *observed* black-box only, in a Windows VM: run it, use its UI with synthetic datasets. Screenshots live only in the gitignored `observations/` folder. Never read, disassemble or copy anything inside the Power BI install, never commit files produced by Power BI, never copy GPL/AGPL/LGPL code. DAX, M and file-format behaviour comes from public documentation and observation.
- **Parity cases** (`docs/parity.md`): synthetic dataset (inline text in the case, or generated in code) + DAX query + the result copied out of Power BI Desktop. Commit only the values. `cargo xtask parity` shows the scoreboard; `--update` records newly passing Checks; `--export` writes copy-ready files to `observations/cases/` for the VM.
- **Test corpora** (real Power BI-authored files) live outside this repo, fetched pinned and sha256-verified. Never commit large binary fixtures; build test datasets and reports in code.
- **Everything is a command.** User-visible behaviour = a command (id, label, ribbon path, shortcut, params doc, `enabled`, `run`) + tests. Ids follow Power BI's ribbon (`home.getData`, `insert.newVisual`, `modeling.newMeasure`) plus primitives. The control channel, CLI and MCP reach all of them.
- **Programmatic calls never open dialogs**: a command with empty params runs with defaults or returns an error; only menu-style invocation opens a dialog.
- **Layering** is enforced: core → expression languages (DAX, M) → model/query/file formats → visuals → engine → UI/MCP → apps. Nothing below the UI layer depends on egui/eframe/winit/rfd. **The UI crate is swappable.** Exact crate list is fixed in `docs/plan/`.
- **The UI is thin**: panels read engine state and act through `app.run(id, params)`. Colours come from theme tokens. UI strings go through the string catalogue; every widget is keyboard-reachable and carries an AccessKit label.
- **Rust only** (no handwritten JS/TS). **Never break wasm.**
- **Quality gates** before every commit: `cargo xtask ci` (fmt, layers, assets, clippy `-D warnings`, tests, wasm, bench, deny: `cargo install cargo-deny --locked --version 0.20.2`). On Linux `cargo xtask test` runs each test binary under `prlimit --as` (4 GiB address space, override with `MCRAFT_TEST_MEM_MB`), so a runaway test aborts instead of OOM-killing the host. Benches are `harness = false` targets printing `BENCH <name> <ns>`; after an intended performance change, re-record the baseline on the reference machine with `cargo xtask bench --save` and commit `bench/<machine>.json` in the same commit. A new crate needs a row in the xtask `LAYERS` table. One task id per commit (`M1.2: CSV connector`).
- Parallel agents: separate `CARGO_TARGET_DIR` per agent; edit only the crates you own; delete your target dir when done.

## Running and looking at the app
- **Desktop app:** `cargo run -p metriccraft -- [PROJECT]` (a Project folder or `.mcraft` Package). `--no-default-features --features wgpu` swaps the OpenGL renderer for wgpu.
- **Look without opening a window:** `cargo run -p metriccraft-ui-egui --example ui_shot -- OUT.png [SCRIPT.jsonl]` renders the window offscreen (wgpu) to PNG; the script runs commands (`{"run": id, "params": {…}}`), switches theme or View, selects Visuals, invokes Ribbon buttons, types, opens the Power Query editor and takes more shots (see the example's docs). **For UI work, look at the result.**
- **UI tests** drive the window through AccessKit labels (`crates/ui-egui/tests/`, `egui_kittest`). `tests/a11y.rs` fails on any control without a worded label: give symbol-only controls one with `icons::name`.
- **UI snapshots:** `cargo xtask snapshot` writes PNGs of every View (light, dark) and the Power Query editor to `observations/snapshots/`.
- **UI budgets:** `cargo xtask ui-bench` (cold start < 1 s on the reference machine, idle private memory ≤ 100 MB; opens a window for ~2 s, or runs under `xvfb-run`).
- Still to come: the control port (M6) and the headless CLI.

## Roadmap
`ROADMAP.md` (committed) is the one-page public summary: status line, headline numbers, progress log, revision history. Update it, and the local `plan/`, whenever work lands.

Specs, plans, ADRs and the glossary are private: they live in plan/ (gitignored) and are never committed, nor quoted in commit messages, PR bodies, issues or release notes beyond a task id. ROADMAP.md shows the overall direction; each contributor keeps their own specs privately for clean-room work. Public docs say what the program does, not how the next step will be built.

## Markdown
Tables must be space-padded so columns align in plain text — they are read raw (terminals, diffs) at least as often as rendered.
