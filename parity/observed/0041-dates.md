# Parity case 0041 — date arithmetic

Spec X-2 (M3.2): dates are numbers of days; adding, subtracting and comparing them. Workflow: `docs/parity.md`.

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
