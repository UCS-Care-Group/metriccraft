# Parity case 0029 — aggregates over blanks, negatives and whole numbers

Spec X-1 (M3.3): which aggregates skip blanks, what an all-blank or empty set gives, and whether a whole-number sum stays whole. Workflow: `docs/parity.md`.

Covers: SUM, AVERAGE, COUNT, COUNTA, COUNTROWS, MIN, MAX, CALCULATE, ISBLANK

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
    "SumQty",          SUM(Sales[Qty]),
    "AvgQty",          AVERAGE(Sales[Qty]),
    "MinAmount",       MIN(Sales[Amount]),
    "MaxQty",          MAX(Sales[Qty]),
    "CountQty",        COUNT(Sales[Qty]),
    "CountAProduct",   COUNTA(Sales[Product]),
    "SumAllBlank",     CALCULATE(SUM(Sales[Amount]), Sales[Region] = "West"),
    "AvgAllBlank",     CALCULATE(AVERAGE(Sales[Amount]), Sales[Region] = "West"),
    "CountAllBlank",   CALCULATE(COUNT(Sales[Amount]), Sales[Region] = "West"),
    "RowsNone",        CALCULATE(COUNTROWS(Sales), Sales[Region] = "Nowhere"),
    "RowsNoneIsBlank", ISBLANK(CALCULATE(COUNTROWS(Sales), Sales[Region] = "Nowhere")),
    "MinNone",         CALCULATE(MIN(Sales[Amount]), Sales[Region] = "Nowhere"),
    "SumZeroQty",      CALCULATE(SUM(Sales[Qty]), Sales[Region] = "West")
)
```
