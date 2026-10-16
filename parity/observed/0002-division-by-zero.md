# Parity case 0002 — `/` by zero: BLANK or Infinity?

Follow-up to case 0001, where `1 / 0` showed an empty cell in the DAX query view grid.
Arithmetic semantics. Workflow: `docs/parity.md`. No dataset needed.

Covers: /, ISBLANK, ISERROR, FORMAT, &

## Query

```dax
EVALUATE
ROW(
    "IsBlank",        ISBLANK(1 / 0),
    "GtMillion",      1 / 0 > 1000000,
    "NegLtMillion",   -1 / 0 < -1000000,
    "AsText",         "" & (1 / 0),
    "NegAsText",      "" & (-1 / 0),
    "Formatted",      FORMAT(1 / 0, "General Number"),
    "IsError",        ISERROR(1 / 0),
    "ZeroZeroBlank",  ISBLANK(0 / 0),
    "ZeroZeroText",   "" & (0 / 0)
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-12 · Regional settings: English (United Kingdom)

```tsv
[IsBlank]	[GtMillion]	[NegLtMillion]	[AsText]	[NegAsText]	[Formatted]	[IsError]	[ZeroZeroBlank]	[ZeroZeroText]
false	true	true	inf	-inf	inf	true	false	-nan(ind)
```

Notes: `1 / 0` is **not** BLANK: it is positive infinity, which the query grid displays as an empty
cell and copies as `Infinity`. Converted to text it is `inf` / `-inf`, and `0 / 0` is `-nan(ind)`.
Those spellings match the Microsoft C runtime's float formatting, not the docs' "Infinity"/"NaN". `ISERROR(1 / 0)` is
True, so infinity counts as an error for ISERROR. `0 / 0` is not BLANK either (NaN).
