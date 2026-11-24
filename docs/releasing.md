# Releasing MetricCraft

Last updated: 2026-10-16. Matches `.github/workflows/release.yml` and `cargo xtask dist`.

## What a release is

A GitHub Release with three archives, unsigned for now:

| Archive                                     | Holds                                                          |
|---------------------------------------------|----------------------------------------------------------------|
| `metriccraft-<version>-linux-x86_64.tar.gz` | `metriccraft`, `metriccraft-cli`                               |
| `metriccraft-<version>-windows-x86_64.zip`  | `metriccraft.exe`, `metriccraft-cli.exe`                       |
| `metriccraft-<version>-macos-universal.zip` | `MetricCraft.app` (Apple silicon and Intel), `metriccraft-cli` |

Each also holds `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`, `ATTRIBUTION.md`, `README.md`,
`THIRD-PARTY-LICENSES.html` (every shipped crate's licence, from cargo-about) and `licenses/`
(the bundled fonts' licences). `cargo xtask dist` fails an archive over 50 MB.

## Cutting a release

1. Set `version` in the workspace `Cargo.toml` to the release (`0.1.0-preview.1`) and land it on
   `main` through a pull request. `cargo xtask dist` refuses a tag that doesn't match it.
2. Tag `main` and push the tag:
   `git tag v0.1.0-preview.1 && git push origin v0.1.0-preview.1`.
   Tags `v*` are protected: only maintainers can create them.
3. The workflow waits for a maintainer to approve the `release` environment, then:
   - `draft` creates a **draft** Release with generated notes (a prerelease when the tag has a
     `-`, such as `-preview.1` or `-rc.1`);
   - `build` builds each archive on its own system and checks its size;
   - `upload` attaches the archives to the draft.
4. A maintainer reads the draft (notes, the three archives, sizes), downloads one archive and
   starts it, then publishes the draft by hand. The workflow never publishes.

Release notes name task ids only: no specification, plan or decision-record references.

## Dry run

Start the workflow by hand (Actions › release › Run workflow). It builds and size-checks the
three archives and keeps them as workflow artifacts for a week. It uses no environment, no
secrets and no write permission, and creates no Release.

## Secrets and tools

- Release secrets (signing, when it comes) live only in the `release` environment, which needs a
  maintainer's approval and serves only `v*` tags. The dry run can't reach them.
- Every action is pinned to a commit SHA. cargo-about is installed by `taiki-e/install-action`
  as a pinned version whose download is checked against its sha256.

## Building an archive on your own machine

```bash
cargo install cargo-about --locked --version 0.9.2 --features cli
cargo xtask dist                      # this system; on macOS add both targets:
cargo xtask dist --target aarch64-apple-darwin --target x86_64-apple-darwin
```

The archive lands in `target/dist/`.

## macOS signing and DMG packaging (`packaging/macos/`)

`release.yml` continues to build unsigned archives with `cargo xtask dist` (pinning
`cargo-about@0.9.2`). For standalone or manual macOS `.app` / `.dmg` / CLI `.zip` builds
with optional Developer ID signing and notarization, run:

```bash
cargo install cargo-about --locked --version 0.9.2 --features cli
packaging/macos/package.sh [--arch universal|aarch64|x86_64] [--skip-build | --build-only] [--no-run]
```

- `packaging/env.sh` exports workspace metadata (`VERSION`, `APP_ID`, `DIST`, `CARGO_TARGET_DIR`)
  and copies legal/font licence files (`LICENSE-MIT`, `LICENSE-APACHE`, `ATTRIBUTION.md`, `NOTICE`,
  `crates/visuals/fonts/OFL.txt`, `licenses/egui-default-fonts/`).
- `packaging/macos/package.sh` builds universal or single-arch binaries, renders `Info.plist.in`,
  signs inside-out with `entitlements.plist` (ad-hoc `-` locally; fails closed in GitHub Actions
  unless `MACOS_ALLOW_ADHOC=1`), builds `$DIST/metriccraft-<version>-macos-<arch>.dmg` and
  `$DIST/metriccraft-cli-<version>-macos-<arch>.zip` with `.sha256` sidecars, notarizes/staples
  the `.app` and `.dmg`, and notarizes the CLI `.zip` for online Gatekeeper verification.
- `packaging/macos/import-cert.sh` imports `APPLE_CERTIFICATE` into a temporary CI keychain and
  fails closed in GitHub Actions unless `MACOS_ALLOW_ADHOC=1`.

## Revision history

| Date       | Change                                                                        |
|------------|-------------------------------------------------------------------------------|
| 2026-10-16 | First version.                                                                |
| 2026-11-24 | Document standalone macOS signing and DMG packaging (`packaging/macos/` #73). |
