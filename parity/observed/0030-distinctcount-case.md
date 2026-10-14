# Parity case 0030 — DISTINCTCOUNT, blanks and letter case

Spec X-1/X-2 (M3.3, M3.10): DISTINCTCOUNT counts BLANK as a value; `Apple`/`apple` and `Pear`/`pear` differ only by case, so the Model should hold them as one value. Before running, check in Table view which spelling Product shows for each (note it below). Workflow: `docs/parity.md`.

Covers: DISTINCTCOUNT, COUNTA, COUNTROWS, CALCULATE, MIN, MAX

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
    "DistinctProduct",   DISTINCTCOUNT(Sales[Product]),
    "DistinctRegion",    DISTINCTCOUNT(Sales[Region]),
    "DistinctAmount",    DISTINCTCOUNT(Sales[Amount]),
    "DistinctQty",       DISTINCTCOUNT(Sales[Qty]),
    "ApplesUpper",       CALCULATE(COUNTROWS(Sales), Sales[Product] = "APPLE"),
    "ApplesLower",       CALCULATE(COUNTROWS(Sales), Sales[Product] = "apple"),
    "MinProduct",        MIN(Sales[Product]),
    "MaxProduct",        MAX(Sales[Product]),
    "DistinctNone",      CALCULATE(DISTINCTCOUNT(Sales[Product]), Sales[Region] = "Nowhere")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[DistinctProduct]	[DistinctRegion]	[DistinctAmount]	[DistinctQty]	[ApplesUpper]	[ApplesLower]	[MinProduct]	[MaxProduct]	[DistinctNone]
3	4	5	6	3	3	Apple	pear	
```

Notes: No errors. Text differing only by case is one value: DISTINCTCOUNT(Product) is 3 (Apple,
pear and blank), and MAX(Product) keeps the first casing loaded (`pear`).
