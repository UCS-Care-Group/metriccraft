# Parity case 0040 — BLANK in operators and comparisons

Spec X-2 (M3.2): follow-up to case 0001's BLANK rows: subtraction, negation, concatenation, ordering and logical operators. Workflow: `docs/parity.md`.

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
