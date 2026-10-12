# DashCraft — instructions for agents

DashCraft is a clean-room, open-source, Rust-native business-intelligence authoring app targeting Microsoft Power BI Desktop parity — and superiority (speed, openness, agent control). It connects to data, shapes it, models it, and builds interactive report pages. It runs natively on macOS, Windows, Linux and BSD, and on the web via WASM. It is the *authoring* half; publishing, sharing and scheduled refresh (the Power BI Service half) is a separate, later product — see `docs/adr/`.

Adapted from the storytold crafting-app conventions (`docs/agents/upstream/`, from `storytold/gridcraft` — the closest sibling). Siblings with the same conventions: `storytold/gridcraft` (Excel, the closest reference: formulas, charts, pivots), `storytold/deckcraft` (PowerPoint), `storytold/designcraft` (InDesign).

Standards and learnings shared across the crafting apps live in `storytold/craftrules` (checked out next to the craft apps). Read its `AGENTS.md` at the start of a session, follow its standards, and contribute reusable learnings back there. Never code: repos don't share code. **Status: craftrules is not publicly readable yet — access pending.** Until then, the rules below are the authority.

## Start every session here
1. Read `plan/STATUS.md` (current milestone, next task), then the task in `plan/execution-plan.md` and the relevant `plan/architecture.md` section. Behaviour reference: `plan/powerbi/*.md` (`observed-ui.md` holds measured observations of the running app).
2. Follow the autonomous operation protocol (`plan/execution-plan.md` §7). Don't stop to ask unless §7 lists the decision as the user's.

Upstream, `plan/` is gitignored (local only). While this repo is private it is tracked; ignore it before going public.

## Assets and iconography — absolute rule
**No Microsoft, Adobe, Tableau/Salesforce, Qlik (or any other vendor's) iconography, images, artwork, fonts, templates, themes, visuals or sample files — ever.** This is the most important rule in this repository; breaking it is a serious failure.
- Every asset in the repo is original work by DashCraft contributors, or public domain / CC0, or open source / Creative Commons that allows redistribution (or licensed open source by the contributor who created it).
- Every asset file has a row in [`ATTRIBUTION.md`](ATTRIBUTION.md) (author, source, licence). Adding an asset without a row is a bug.
- Prefer art generated in code: the UI icon set, report themes, visual palettes and sample datasets are drawn/defined in code and original.
- Never copy Power BI's icons, ribbon artwork, theme JSON, visual definitions, templates, sample `.pbix` files (Contoso, AdventureWorks reports…) or wording beyond feature names. Power BI feature *names* (menu labels, DAX/M function names) are fine.
- Screenshots of Microsoft software are never committed (they live only in `plan/powerbi/screenshots/`).

## Never crash
People trust DashCraft with their numbers; a crash loses their work. **This outranks feature work.**
- **No panics in non-test code:** no `unwrap()`, `expect()`, `panic!`, `unreachable!`, `todo!`, `unimplemented!`; no `unsafe` (`unsafe_code = "forbid"`).
- **Errors are `Result<T, E>`** through the crate's error type and `?`.
- **Input-derived numbers are hostile** (files, data sources, DAX/M expressions, commands, MCP/control params): `get()` instead of indexing, checked/saturating arithmetic, cap input-sized allocations (huge tables, cross joins, `GENERATESERIES`, unbounded `List.Generate`).
- **Bound recursion** (expression nesting, measure-references-measure chains, relationship paths).
- **Every crash fix lands with a regression test.**
- Crates carry `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`.

## Non-negotiables
- **Clean-room.** Power BI Desktop may be *observed* black-box (it is Windows-only: use a Windows VM): run it, use its UI with synthetic datasets, take screenshots stored only under `plan/powerbi/screenshots/`. Never read, disassemble or copy anything inside the Power BI install, never commit files produced by Power BI, never copy GPL/AGPL/LGPL code. DAX, M and file-format behaviour comes from public documentation and observation.
- **Fonts live in [`storytold/craft-fonts`](https://github.com/storytold/craft-fonts)**, never in this repo (optional build input `CRAFT_FONTS_DIR`).
- **Test corpora** (real Power BI-authored files) live in a separate corpus repo, fetched pinned and sha256-verified. Never commit large binary fixtures; build test datasets and reports in code.
- **Everything is a command.** User-visible behaviour = a command (id, label, ribbon path, shortcut, params doc, `enabled`, `run`) + tests. Ids follow Power BI's ribbon (`home.getData`, `insert.newVisual`, `modeling.newMeasure`) plus primitives. The control channel, CLI and MCP reach all of them.
- **Programmatic calls never open dialogs**: a command with empty params runs with defaults or returns an error; only menu-style invocation opens a dialog.
- **Layering** is enforced: core → expression languages (DAX, M) → model/query/file formats → visuals → engine → UI/MCP → apps. Nothing below the UI layer depends on egui/eframe/winit/rfd. **The UI crate is swappable.** Exact crate list is fixed by ADR before the first crate lands.
- **The UI is thin**: panels read engine state and act through `app.run(id, params)`. Colours come from theme tokens.
- **Rust only** (no handwritten JS/TS). **Never break wasm.**
- **Quality gates** before every commit: fmt, clippy `-D warnings`, tests, assets, layers, wasm (`cargo xtask ci` once the workspace exists). One task id per commit (`M1.2: CSV connector`).
- Parallel agents: separate `CARGO_TARGET_DIR` per agent; edit only the crates you own; delete your target dir when done.

## Running and looking at the app
Nothing to run yet. When the workspace lands, document here: the run command with a sample dataset and control port, the headless CLI, and the offscreen snapshot render. **For UI work, look at the result** (snapshot PNG) and compare with `plan/powerbi/observed-ui.md`.

## Roadmap
`ROADMAP.md` (committed, once work starts) is the one-page summary: stage, headline numbers, progress log. It points at the detail in `docs/`: `target-app-parity.md`, `gaps.md`, `roadmap.md`, `architecture.md`, and area checklists (`dax-parity.md`, `m-parity.md`, `visual-parity.md`, `connector-parity.md`, `file-format-parity.md`, `ui-parity.md`). Update the affected docs whenever work lands.

## Markdown
Tables must be space-padded so columns align in plain text — they are read raw (terminals, diffs) at least as often as rendered. (From `storytold/artcraft`.)

## Contributor credits
Upstream apps compile credits from `contributors/people.toml` (names people chose for themselves) and a generated `contributors.json`. Never add, edit, guess or copy anyone else's entry or name. Adopt the upstream mechanism when this repo moves to storytold.
