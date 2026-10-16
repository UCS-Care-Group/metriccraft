# Parity case 0043 — SUMMARIZECOLUMNS over two related Tables, ORDER BY two keys

M3.9: group by a one-side column (`Regions[Region]`) and a many-side column
(`Sales[Product]`); each expression evaluated in its group's filter context; groups whose
expressions are all BLANK are dropped (South/Apple and South/Plum here); a multi-row result
sorted by two keys, one descending. Workflow: `docs/parity.md`.

Covers: SUMMARIZECOLUMNS, ORDER BY, SUM, COUNTROWS, FILTER, Relationship

## Dataset

Empty cells are blanks. Enter both tables; Power BI's relationship auto-detect relates
`Sales[Region]` (many) to `Regions[Region]` (one) on the shared column name. Check in Model view
that exactly that one Relationship exists, active, single direction, before running the query.
`West` has no Sales rows.

Table: Regions — Region: text, Manager: text

```tsv
Region	Manager
North	Ann
South	Bob
West	Cy
```

Table: Sales — Region: text, Product: text, Amount: decimal, Qty: whole

```tsv
Region	Product	Amount	Qty
North	Apple	10.5	2
North	Pear	-3	3
North	Apple	4	1
South	Apple		1
South	Pear	7.25	5
South	Plum		2
```

## Query

```dax
EVALUATE
SUMMARIZECOLUMNS(
    Regions[Region],
    Sales[Product],
    "Total", SUM(Sales[Amount]),
    "Big Orders", COUNTROWS(FILTER(Sales, Sales[Qty] >= 3))
)
ORDER BY Regions[Region] DESC, Sales[Product]
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
Regions[Region]	Sales[Product]	[Total]	[Big Orders]
South	Pear	7.25	1
North	Apple	14.5	
North	Pear	-3	1
```

Notes: No errors. Power BI auto-detected Sales[Region] many-to-one Regions[Region], single direction.
Combinations where every measure is blank are left out (South/Apple, South/Plum, West).
