# Parity case 0041 — date arithmetic

M3.2: dates are numbers of days; adding, subtracting and comparing them. Workflow: `docs/parity.md`.

Covers: MIN, MAX, +, -, <, COUNTROWS

## Dataset

Empty cells are blanks.

Table: Days — Day: date, At: datetime

```tsv
Day	At
2026-01-01	2026-01-01 06:00:00
2026-01-31	2026-01-02 18:30:00
2024-02-29	
```

## Query

```dax
EVALUATE
ROW(
    "NextDay",       MIN(Days[Day]) + 1,
    "Span",          MAX(Days[Day]) - MIN(Days[Day]),
    "SpanTimes",     MAX(Days[At]) - MIN(Days[At]),
    "AsNumber",      MIN(Days[Day]) * 1,
    "Earlier",       COUNTROWS(FILTER(Days, Days[Day] < MAX(Days[Day]))),
    "DateEqText",    ISERROR(MIN(Days[Day]) = "2026-01-01")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[NextDay]	[Span]	[SpanTimes]	[AsNumber]	[Earlier]
Fri, 01 Mar 2024 00:00:00	Mon, 02 Dec 1901 00:00:00	Sun, 31 Dec 1899 12:30:00	45351	2
```

Notes: The full query fails before running, so ISERROR never sees the error:
`Query (8, 30) DAX comparison operations do not support comparing values of type Date with values
of type Text. Consider using the VALUE or FORMAT function to convert one of the values.`
DateEqText was removed so the rest could be copied (it stays Pending). Date − date is a datetime
(Span copies as 702 days after day 0); dates copy as `Fri, 01 Mar 2024 00:00:00` while the grid
shows `3/1/2024 12:00:00 AM`.
