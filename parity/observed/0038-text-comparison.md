# Parity case 0038 — text comparison and case

DAX compares text ignoring case; probes accents, ß, ordering and the empty text against BLANK. Workflow: `docs/parity.md`.

Covers: =, ==, <, <>, BLANK

## Query

```dax
EVALUATE
ROW(
    "CaseEq",        "a" = "A",
    "CaseStrict",    "a" == "A",
    "AccentEq",      "é" = "É",
    "AccentVsPlain", "e" = "é",
    "SharpS",        "ß" = "SS",
    "Order",         "a" < "B",
    "Digits",        "10" < "9",
    "EmptyBlank",    "" = BLANK(),
    "EmptyStrict",   "" == BLANK(),
    "NotEqual",      "x" <> "X"
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[CaseEq]	[CaseStrict]	[AccentEq]	[AccentVsPlain]	[SharpS]	[Order]	[Digits]	[EmptyBlank]	[EmptyStrict]	[NotEqual]
true	true	true	false	true	true	true	true	false	false
```

Notes: No errors. `==` is case-insensitive too; `ß` = `SS`; accents are significant.
