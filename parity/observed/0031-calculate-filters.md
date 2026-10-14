# Parity case 0031 — CALCULATE filter arguments, FILTER, ALL, ALLEXCEPT

Spec X-1/X-2 (M3.4): an inner CALCULATE filter on a column replaces the outer one; several arguments AND together; FILTER over the table; ALL and ALLEXCEPT remove filters. Workflow: `docs/parity.md`.

Covers: CALCULATE, FILTER, ALL, ALLEXCEPT, SUM, COUNTROWS, &&, ||

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
EVALUATE
ROW(
    "InnerWins",      CALCULATE(CALCULATE(SUM(Sales[Qty]), Sales[Region] = "North"), Sales[Region] = "South"),
    "TwoArgs",        CALCULATE(SUM(Sales[Qty]), Sales[Region] = "South", Sales[Qty] > 1),
    "OrSameColumn",   CALCULATE(SUM(Sales[Qty]), Sales[Region] = "North" || Sales[Region] = "West"),
    "FilterTable",    CALCULATE(SUM(Sales[Amount]), FILTER(Sales, Sales[Qty] >= 2)),
    "FilterRows",     COUNTROWS(FILTER(Sales, Sales[Amount] > 0)),
    "AllRows",        CALCULATE(COUNTROWS(Sales), Sales[Region] = "North", ALL(Sales)),
    "AllColumn",      CALCULATE(CALCULATE(SUM(Sales[Qty]), ALL(Sales[Region])), Sales[Region] = "North"),
    "AllExcept",      CALCULATE(CALCULATE(SUM(Sales[Qty]), ALLEXCEPT(Sales, Sales[Region])), Sales[Region] = "South", Sales[Product] = "Apple"),
    "AllOtherColumn", CALCULATE(CALCULATE(SUM(Sales[Qty]), ALL(Sales[Product])), Sales[Region] = "South", Sales[Product] = "Apple"),
    "NoMatch",        CALCULATE(SUM(Sales[Qty]), Sales[Region] = "North", Sales[Region] = "South")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[InnerWins]	[TwoArgs]	[OrSameColumn]	[FilterTable]	[FilterRows]	[AllRows]	[AllColumn]	[AllExcept]	[AllOtherColumn]	[NoMatch]
5	5	5	7.5	3	2	11	6	6	
```

Notes: No errors.
