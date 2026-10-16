# Parity case 0045 — SUMMARIZECOLUMNS across Tables with a constant

M3.9: group-by columns from two related Tables. Does SUMMARIZECOLUMNS keep only the
combinations that exist in Sales, or cross-join Region × Product and keep every pair whose
expressions aren't all BLANK? A constant expression (`"One", 1`) is never BLANK, so the row count
tells. Same dataset as case 0043. Workflow: `docs/parity.md`.

Covers: SUMMARIZECOLUMNS, COUNTROWS

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
    "One", 1,
    "Rows", COUNTROWS(Sales)
)
ORDER BY Regions[Region], Sales[Product]
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
Regions[Region]	Sales[Product]	[One]	[Rows]
North	Apple	1	2
North	Pear	1	1
North	Plum	1	
South	Apple	1	1
South	Pear	1	1
South	Plum	1	1
West	Apple	1	
West	Pear	1	
West	Plum	1	
```

Notes: No errors. Same auto-detected relationship as 0043.
