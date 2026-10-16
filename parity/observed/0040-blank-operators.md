# Parity case 0040 — BLANK in operators and comparisons

M3.2: follow-up to case 0001's BLANK rows: subtraction, negation, concatenation, ordering and logical operators. Workflow: `docs/parity.md`.

Covers: BLANK, -, &, <, >, =, &&, ||, NOT

## Query

```dax
EVALUATE
ROW(
    "MinusBlank",    5 - BLANK(),
    "BlankMinus",    BLANK() - 5,
    "NegBlank",      ISBLANK(-BLANK()),
    "BlankMinusBlank", ISBLANK(BLANK() - BLANK()),
    "DivBlank",      BLANK() / 2,
    "ConcatBlanks",  BLANK() & BLANK(),
    "LessOne",       BLANK() < 1,
    "GreaterNeg",    BLANK() > -1,
    "EqFalse",       BLANK() = FALSE(),
    "AndTrue",       BLANK() && TRUE(),
    "OrTrue",        BLANK() || TRUE(),
    "NotBlank",      NOT BLANK()
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[MinusBlank]	[BlankMinus]	[NegBlank]	[BlankMinusBlank]	[DivBlank]	[ConcatBlanks]	[LessOne]	[GreaterNeg]	[EqFalse]	[AndTrue]	[OrTrue]	[NotBlank]
5	-5	true	true			true	true	true	false	true	true
```

Notes: No errors.
