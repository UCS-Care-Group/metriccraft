# Parity case 0032 — context transition through CALCULATE and Measure references

M3.4, M3.6: inside FILTER's row context a bare SUM sees every row, CALCULATE (or a Measure reference) sees only the current row. Workflow: `docs/parity.md`.

Covers: CALCULATE, FILTER, COUNTROWS, SUM, DEFINE MEASURE

## Dataset

Empty cells are blanks.

Table: Sales — Region: text, Product: text, Amount: decimal, Qty: whole

```tsv
Region	Product	Amount	Qty
North	Apple	10.5	2
North	pear	-3	3
South	Apple	4	1
South	Pear		5
West	apple		0
East		7.25	
```

## Query

```dax
DEFINE
    MEASURE Sales[Total Qty] = SUM(Sales[Qty])
EVALUATE
ROW(
    "NoTransition",   COUNTROWS(FILTER(Sales, SUM(Sales[Qty]) > 3)),
    "Transition",     COUNTROWS(FILTER(Sales, CALCULATE(SUM(Sales[Qty])) > 3)),
    "MeasureRef",     COUNTROWS(FILTER(Sales, [Total Qty] > 3)),
    "MeasureTotal",   [Total Qty],
    "MeasureFiltered", CALCULATE([Total Qty], Sales[Region] = "South")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[NoTransition]	[Transition]	[MeasureRef]	[MeasureTotal]	[MeasureFiltered]
6	1	1	11	6
```

Notes: No errors. The DAX query view offers an *Update model: Add new measure* link above the DEFINE
line; not used.
