# Parity case 0044 — aggregates over text, boolean and date columns

Spec X-1 (M3.3): which aggregates accept text, boolean and date columns, and what they return.
Errors are wrapped in ISERROR so one unsupported form doesn't fail the whole query. Workflow:
`docs/parity.md`.

Covers: SUM, AVERAGE, COUNT, COUNTA, MIN, MAX, DISTINCTCOUNT, ISERROR, ISBLANK

## Dataset

Empty cells are blanks.

Table: Items — Name: text, Flag: boolean, Day: date

```tsv
Name	Flag	Day
b	true	2026-01-01
A	false	2026-01-03
	true	
c		2026-01-02
```

## Query

```dax
EVALUATE
ROW(
    "SumTextErr",     ISERROR(SUM(Items[Name])),
    "AvgTextErr",     ISERROR(AVERAGE(Items[Name])),
    "AvgTextBlank",   IFERROR(ISBLANK(AVERAGE(Items[Name])), "error"),
    "CountText",      IFERROR(COUNT(Items[Name]), -1),
    "CountAText",     COUNTA(Items[Name]),
    "MinText",        IFERROR(MIN(Items[Name]), "error"),
    "MaxText",        IFERROR(MAX(Items[Name]), "error"),
    "CountFlag",      IFERROR(COUNT(Items[Flag]), -1),
    "CountAFlag",     COUNTA(Items[Flag]),
    "DistinctFlag",   DISTINCTCOUNT(Items[Flag]),
    "SumFlagErr",     ISERROR(SUM(Items[Flag])),
    "MinFlag",        IFERROR(MIN(Items[Flag]), "error"),
    "CountDay",       COUNT(Items[Day]),
    "MinDay",         MIN(Items[Day]),
    "SumDayErr",      IFERROR(SUM(Items[Day]) * 1, -1),
    "AvgDay",         IFERROR(AVERAGE(Items[Day]) * 1, -1),
    "MinTwoBlank",    MIN(1, BLANK()),
    "MaxTwoBlank",    MAX(-1, BLANK()),
    "MinTwoText",     IFERROR(MIN("b", "a"), "error")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[CountText]	[CountAText]	[MinText]	[MaxText]	[CountAFlag]	[DistinctFlag]	[CountDay]	[MinDay]	[SumDayErr]	[AvgDay]	[MinTwoBlank]	[MaxTwoBlank]	[MinTwoText]
3	3	A	c	3	3	3	Thu, 01 Jan 2026 00:00:00	138072	46024			a
```

Notes: Six columns fail the whole query before it runs (type checks, so ISERROR/IFERROR never see
them); they were removed so the rest could be copied, and stay Pending:
- SumTextErr: `The function SUM cannot work with values of type String.`
- AvgTextErr, AvgTextBlank: `The function AVERAGE cannot work with values of type String.`
- CountFlag: `The function COUNT cannot work with values of type Boolean.`
- SumFlagErr: `The function SUM cannot work with values of type Boolean.`
- MinFlag: `The function MIN cannot work with values of type Boolean.`
Each message is prefixed `Query (line, col)`. COUNT over text counts non-blank text (3).
