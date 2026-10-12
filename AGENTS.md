# OpenDash BI — instructions for agents

OpenDash BI is a clean-room, open-source, Rust-native business-intelligence authoring app: a Power BI Desktop alternative that connects to data, shapes it, models it, and builds interactive report pages. DAX results match Power BI exactly; the UI follows Power BI's workflow but may improve on it. Desktop (Linux, Windows, macOS) is the product; the web build (WASM) is a reduced, file-only variant. Publishing, sharing and scheduled refresh (the Power BI Service half) is a separate, later product.

Independent project (ADR 0002). Its engineering rules were adapted from the storytold crafting apps (`storytold/gridcraft`; verbatim copies in `docs/agents/upstream/` for reference only) and are now our own.

## Start every session here
1. `CONTEXT.md` — the glossary. Use its terms; challenge drift.
2. `docs/spec/` — what we're building. `docs/plan/` — how and in what order. `docs/adr/` — decisions expensive to reverse.
3. Tasks are GitHub issues (`docs/agents/issue-tracker.md`).

Work is planned **Glossary → Specification → Implementation Plan** before code; an ADR for anything expensive to reverse.

## Assets and iconography — absolute rule
**No Microsoft, Adobe, Tableau/Salesforce, Qlik (or any other vendor's) iconography, images, artwork, fonts, templates, themes, visuals or sample files — ever.** This is the most important rule in this repository; breaking it is a serious failure.
- Every asset in the repo is original work by OpenDash BI contributors, or public domain / CC0, or open source / Creative Commons that allows redistribution.
- Every asset file has a row in [`ATTRIBUTION.md`](ATTRIBUTION.md) (author, source, licence). Adding an asset without a row is a bug.
- Prefer art generated in code: the UI icon set, report themes, visual palettes and sample datasets are drawn/defined in code and original.
- Never copy Power BI's icons, ribbon artwork, theme JSON, visual definitions, templates, sample `.pbix` files (Contoso, AdventureWorks reports…) or wording beyond feature names. Power BI feature *names* (menu labels, DAX/M function names) are fine.
- Fonts: system fonts, plus at most one bundled OFL-licensed font with an `ATTRIBUTION.md` row.

## Never crash
People trust OpenDash BI with their numbers; a crash loses their work. **This outranks feature work.**
- **No panics in non-test code:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`; no `unsafe` (`unsafe_code = "forbid"`).
- **Errors are `Result<T, E>`** through the crate's error type and `?`.
- **Input-derived numbers are hostile** (files, data sources, DAX/M expressions, commands, MCP/control params): `get()` instead of indexing, checked/saturating arithmetic, cap input-sized allocations (huge tables, cross joins, `GENERATESERIES`, unbounded `List.Generate`).
- **Bound recursion** (expression nesting, measure-references-measure chains, relationship paths).
- **Every crash fix lands with a regression test.**
- Lints are set once in the workspace `Cargo.toml` (`unsafe_code = "forbid"`; clippy `unwrap_used`, `expect_used`, `panic`, `unimplemented`, `todo`, `unreachable` denied); every crate has `[lints] workspace = true`. `clippy.toml` allows panicking only in tests.

## Non-negotiables
- **Clean-room.** Power BI Desktop is *observed* black-box only, in a Windows VM: run it, use its UI with synthetic datasets. Screenshots live only in the gitignored `observations/` folder. Never read, disassemble or copy anything inside the Power BI install, never commit files produced by Power BI, never copy GPL/AGPL/LGPL code. DAX, M and file-format behaviour comes from public documentation and observation.
- **Parity cases** (see `CONTEXT.md`, ADR 0004): synthetic dataset (inline text in the case, or generated in code) + DAX query + the result copied out of Power BI Desktop. Commit only the values. `cargo xtask parity` shows the scoreboard; `--update` records newly passing Checks.
- **Test corpora** (real Power BI-authored files) live outside this repo, fetched pinned and sha256-verified. Never commit large binary fixtures; build test datasets and reports in code.
- **Everything is a command.** User-visible behaviour = a command (id, label, ribbon path, shortcut, params doc, `enabled`, `run`) + tests. Ids follow Power BI's ribbon (`home.getData`, `insert.newVisual`, `modeling.newMeasure`) plus primitives. The control channel, CLI and MCP reach all of them.
- **Programmatic calls never open dialogs**: a command with empty params runs with defaults or returns an error; only menu-style invocation opens a dialog.
- **Layering** is enforced: core → expression languages (DAX, M) → model/query/file formats → visuals → engine → UI/MCP → apps. Nothing below the UI layer depends on egui/eframe/winit/rfd. **The UI crate is swappable.** Exact crate list is fixed in `docs/plan/`.
- **The UI is thin**: panels read engine state and act through `app.run(id, params)`. Colours come from theme tokens. UI strings go through the string catalogue; every widget is keyboard-reachable and carries an AccessKit label.
- **Rust only** (no handwritten JS/TS). **Never break wasm.**
- **Quality gates** before every commit: `cargo xtask ci` (fmt, layers, assets, clippy `-D warnings`, tests, wasm, bench). Benches are `harness = false` targets printing `BENCH <name> <ns>`; after an intended performance change, re-record the baseline on the reference machine with `cargo xtask bench --save` and commit `bench/<machine>.json` in the same commit. A new crate needs a row in the xtask `LAYERS` table. One task id per commit (`M1.2: CSV connector`).
- Parallel agents: separate `CARGO_TARGET_DIR` per agent; edit only the crates you own; delete your target dir when done.

## Running and looking at the app
Nothing to run yet. When the workspace lands, document here: the run command with a sample dataset and control port, the headless CLI, and the offscreen snapshot render. **For UI work, look at the result** (snapshot PNG).

## Roadmap
`ROADMAP.md` (committed, once work starts) is the one-page summary: stage, headline numbers, progress log. Detail lives in `docs/spec/` and `docs/plan/`. Update the affected docs whenever work lands.

## Markdown
Tables must be space-padded so columns align in plain text — they are read raw (terminals, diffs) at least as often as rendered.
