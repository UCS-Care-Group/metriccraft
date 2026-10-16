# Parity case 0039 — operator precedence

`^` against unary minus, associativity of `^`, `NOT` against `&&`, `&` against `+`. Workflow: `docs/parity.md`.

Covers: ^, -, NOT, &&, ||, &, +

## Query

```dax
EVALUATE
ROW(
    "NegPower",      -2 ^ 2,
    "PowerChain",    2 ^ 3 ^ 2,
    "NotAnd",        NOT TRUE() && FALSE(),
    "OrAnd",         TRUE() || FALSE() && FALSE(),
    "ConcatPlus",    1 & 2 + 3,
    "MinusChain",    10 - 4 - 3,
    "CompareConcat", "a" & "b" = "ab"
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[NegPower]	[PowerChain]	[NotAnd]	[OrAnd]	[ConcatPlus]	[MinusChain]	[CompareConcat]
-4	512	false	true	15	3	true
```

Notes: No errors. `-2 ^ 2` is -4 (unary minus binds looser than `^`); `2 ^ 3 ^ 2` is 512 (`^` is
right-associative).
