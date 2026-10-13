# Parity case 0037 — integer overflow and type promotion

Spec X-2 (M3.2): whole-number arithmetic at the 64-bit edge, `/` always decimal, mixed comparisons. If a column errors, note the message and remove it so the rest can be copied. Workflow: `docs/parity.md`.

Covers: +, -, *, /, ^, =

## Query

```dax
EVALUATE
ROW(
    "MaxPlusOne",    9223372036854775807 + 1,
    "MaxTimesTwo",   9223372036854775807 * 2,
    "BigLiteral",    9223372036854775808,
    "IntDivide",     7 / 2,
    "IntEqDecimal",  7 = 7.0,
    "Power",         2 ^ 10,
    "PowerHalf",     4 ^ 0.5,
    "NegZero",       -0.0,
    "TextPlusNum",   "3" + 4,
    "NumConcat",     3 & 4
)
```
