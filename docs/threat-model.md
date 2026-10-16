# Threat model

What MetricCraft protects, from whom, and what it does about it. Reports go through
[`SECURITY.md`](../SECURITY.md).

MetricCraft is a desktop authoring app. Today it reads local files only: it opens no network
connection and listens on no port. The web build runs in the browser's sandbox on files the user
picks.

## Assets

- **The user's data** in the files they import, and the numbers computed from it.
- **The user's work**: Projects and Packages, unsaved changes and autosave recovery copies.
- **The user's computer**: files outside what they asked MetricCraft to open; memory and CPU.

## Adversaries and what MetricCraft does about them

### A hostile file

CSV, XLSX, Parquet, Projects and Packages may come from anyone. A crash, hang, runaway memory use
or a read or write outside what the user chose, caused by a file, is a security bug.

- No panics in non-test code and no `unsafe` (workspace lints); every error is a value.
- Sizes read from a file are hostile: checked arithmetic, capped allocations, and loads stop at a
  memory cap (half of physical memory by default) with an error naming the table.
- Packages are zip files: the entry count and total uncompressed size are capped, and entries
  with absolute paths, `..`, symlinks or encryption are refused. XLSX parts are read through a
  metered reader so a decompression bomb can't fill memory; password-protected workbooks are
  refused before they are parsed.
- Parquet decoding runs behind checks for the corrupt shapes known to panic the decoder.
- Fuzzed: parsers and command parameters run against generated hostile input in the tests.

### A hostile expression

DAX measures, calculated columns, DAX queries and M are text from a file or a person.

- Expression nesting and measure-reference chains are bounded; cycles are errors.
- Intermediate tables count against the memory cap; queries can be cancelled.
- An erroring measure shows an error in its visual; it never stops the app.

### Other programs and users on the same computer

- Recovery copies and preferences live in the user's own config and data directories.
- Saves are staged and switched in atomically, so an interrupted save leaves the previous good
  state.

### A supply-chain compromise

- Dependencies come from crates.io only and are checked by cargo-deny on every change:
  RustSec advisories, permissive licences only, no other sources (`deny.toml`).
- Every third-party GitHub Action is pinned to a commit SHA; Dependabot proposes updates after a
  week's cooldown; zizmor checks the workflows; OpenSSF Scorecard reports on the repository.
- Checkouts keep no credentials. Release builds attach to a draft that a person reviews and
  publishes; tools downloaded while packaging are pinned and checksum-verified (`docs/releasing.md`).

## Out of scope and known limits

- Release archives are not yet signed or notarised: verify downloads against the GitHub Release.
- A user who opens a file is trusted to want its data loaded; MetricCraft does not judge content.
- The browser build inherits the browser's security model and limits.

## Revision history

| Date       | Change         |
|------------|----------------|
| 2026-10-16 | First version. |
