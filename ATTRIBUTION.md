# Asset attribution

Every non-code asset in this repository (images, icons, fonts, example files, presets) is listed
here with its author, source and licence. Policy: see "Assets and iconography" in `AGENTS.md`.

| Asset | Author | Source | Licence | Notes |
|-------|--------|--------|---------|-------|
| `crates/visuals/fonts/Inter-Regular.ttf` | The Inter Project Authors (Rasmus Andersson) | https://github.com/rsms/inter (release v4.1, `extras/ttf/`) | SIL OFL 1.1 (`crates/visuals/fonts/OFL.txt`) | Bundled UI and visual font (ADR 0007); sha256 40d692fc… |
| `crates/visuals/fonts/Inter-Bold.ttf` | The Inter Project Authors (Rasmus Andersson) | https://github.com/rsms/inter (release v4.1, `extras/ttf/`) | SIL OFL 1.1 (`crates/visuals/fonts/OFL.txt`) | Bundled UI and visual font (ADR 0007); sha256 28831609… |

## Fonts bundled by dependencies

Not files in this repository: the desktop and web builds embed these through egui's default fonts
(crate `epaint_default_fonts` 0.36.2, `eframe`'s `default_fonts` feature). Hack is the monospace face of
the DAX and M editors; the others are fallbacks behind Inter. Their licence texts are in
`licenses/egui-default-fonts/` and ship in every release archive.

- **Ubuntu Light** (`Ubuntu-Light.ttf`) — Dalton Maag Ltd for Canonical; Ubuntu Font Licence 1.0 (`UFL.txt`).
- **Hack Regular** (`Hack-Regular.ttf`) — Source Foundry Authors, from DejaVu and Bitstream Vera; MIT and
  Bitstream Vera Licence (`Hack-Regular.txt`).
- **Noto Emoji Regular** (`NotoEmoji-Regular.ttf`) — Google; SIL OFL 1.1 (`OFL.txt`).
- **emoji-icon-font** (`emoji-icon-font.ttf`) — John Slegers; MIT (`emoji-icon-font-mit-license.txt`).
