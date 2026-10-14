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
