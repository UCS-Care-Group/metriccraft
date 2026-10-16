# Parity case 0033 — IF and SWITCH with BLANK and missing branches

M3.5: a missing else is BLANK; a BLANK condition is false; SWITCH with no match and no default is BLANK; untaken branches are not evaluated. Workflow: `docs/parity.md`.

Covers: IF, SWITCH, BLANK, ISBLANK, /

## Query

```dax
EVALUATE
ROW(
    "IfTrue",          IF(1 < 2, "yes", "no"),
    "IfNoElse",        ISBLANK(IF(1 > 2, "yes")),
    "IfBlankCond",     IF(BLANK(), "yes", "no"),
    "IfZeroCond",      IF(0, "yes", "no"),
    "IfUntaken",       IF(TRUE(), 1, "a" + 1),
    "SwitchMatch",     SWITCH(2, 1, "one", 2, "two", "other"),
    "SwitchDefault",   SWITCH(9, 1, "one", 2, "two", "other"),
    "SwitchNoDefault", ISBLANK(SWITCH(9, 1, "one")),
    "SwitchTrue",      SWITCH(TRUE(), 1 > 2, "a", 2 > 1, "b", "c"),
    "SwitchBlank",     SWITCH(BLANK(), 0, "zero", "other")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[IfTrue]	[IfNoElse]	[IfBlankCond]	[IfZeroCond]	[IfUntaken]	[SwitchMatch]	[SwitchDefault]	[SwitchNoDefault]	[SwitchTrue]	[SwitchBlank]
yes	true	no	no	1	two	other	true	b	other
```

Notes: No errors. The grid shows booleans as `True`/`False`; the copy gives `true`/`false`.
