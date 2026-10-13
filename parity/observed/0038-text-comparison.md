# Parity case 0038 — text comparison and case

Spec X-2: DAX compares text ignoring case; probes accents, ß, ordering and the empty text against BLANK. Workflow: `docs/parity.md`.

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
