# Parity case 0039 — operator precedence

Spec X-2: `^` against unary minus, associativity of `^`, `NOT` against `&&`, `&` against `+`. Workflow: `docs/parity.md`.

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
