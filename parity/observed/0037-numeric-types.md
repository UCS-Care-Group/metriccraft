# Parity case 0037 — integer overflow and type promotion

M3.2: whole-number arithmetic at the 64-bit edge, `/` always decimal, mixed comparisons. If a column errors, note the message and remove it so the rest can be copied. Workflow: `docs/parity.md`.

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

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[MaxPlusOne]	[MaxTimesTwo]	[BigLiteral]	[IntDivide]	[IntEqDecimal]	[Power]	[PowerHalf]	[NegZero]	[TextPlusNum]	[NumConcat]
-9223372036854776000	-2	9223372036854776000	3.5	true	1024	2	0	7	34
```

Notes: No errors. **Whole-number overflow wraps silently** instead of raising an error: MaxPlusOne is
-2^63 and MaxTimesTwo is -2. The copy prints large whole numbers through a double
(`-9223372036854776000`); the grid shows the same digits.
