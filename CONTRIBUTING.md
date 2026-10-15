# Contributing to MetricCraft

Thanks for helping. MetricCraft is a clean-room reimplementation of Power BI Desktop's authoring
workflow, so a few rules matter more here than in most projects.

## Clean-room: never copy from Power BI

- Power BI Desktop may only be **observed** as a black box: run it and use its UI with synthetic
  data. Never read, disassemble or copy anything inside its install, and never commit files it
  produces (`.pbix`, `.pbit`, exported themes, screenshots).
- DAX, M and file-format behaviour comes from public documentation and observation only.
- No Microsoft (or any other vendor's) icons, images, fonts, templates, themes, visuals or sample
  files. Every asset is original, public domain/CC0 or openly licensed, with a row in
  [`ATTRIBUTION.md`](ATTRIBUTION.md). Power BI feature *names* (menu labels, DAX/M function names)
  are fine.
- Never copy GPL/AGPL/LGPL code.

If you have seen Power BI's internals or source, please don't contribute code in that area.

## Reporting a DAX difference

The most useful issue is a **parity case**: a small synthetic dataset (inline text), the DAX query,
and the result Power BI Desktop gave, copied out as values. See `CONTEXT.md` and
`docs/adr/0004-parity-case-format.md`.

## Code

- **Never crash.** No `unwrap`/`expect`/`panic!`/indexing on input-derived values in non-test
  code, and no `unsafe`. Errors are `Result`. Treat every file, expression and parameter as
  hostile: checked arithmetic, capped allocations, bounded recursion. A crash fix comes with a
  regression test.
- **Everything is a command.** User-visible behaviour is a command with tests; the UI only reads
  engine state and runs commands.
- **Layering** is enforced (`cargo xtask layers`): nothing below the UI layer depends on egui.
- **Rust only**, and the web (WASM) build must keep compiling.
- Before sending a pull request, run `cargo xtask ci` (fmt, layers, assets, strings, clippy, tests,
  wasm, bench). The full rulebook, also used by AI coding agents, is [`AGENTS.md`](AGENTS.md).

Planning happens as **glossary → specification → plan**: `CONTEXT.md`, `docs/spec/`,
`docs/plan/`, with decisions in `docs/adr/`. For anything large, please open an issue first.

## Licence

MetricCraft is licensed under MIT OR Apache-2.0. Unless you state otherwise, any contribution you
submit is licensed the same way, without additional terms.
