# Visual parity case v0004 — table with a total row

M4.6, glossary **Total**: a table groups by its text Column and shows a total row
whose DISTINCTCOUNT is over all rows (4), not the sum of the rows (5). Workflow:
`docs/parity.md` (visual cases).

Covers: table, Total, DISTINCTCOUNT, implicit measure

## Dataset

Empty cells are blanks.

Table: Sales — Region: text, Product: text, Customer: text, Amount: decimal

```tsv
Region	Product	Customer	Amount
North	Apple	Ann	10.5
North	Apple	Bob	4
North	Pear	Ann	-3
South	Apple	Cy	7.25
South	Pear	Cy	2
South		Dee	5
West	Plum	Ann	1
```

## Measures

```dax
Sales[Customers] = DISTINCTCOUNT(Sales[Customer])
```

## Visual

```json
{
  "type": "table",
  "wells": {
    "values": [
      {"table": "Sales", "column": "Region"},
      {"table": "Sales", "column": "Amount"},
      {"measure": "Customers"}
    ]
  }
}
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
Sales[Region]	[IsGrandTotalRowTotal]	[SumAmount]	[Customers]
	true	26.75	4
North	false	11.5	2
South	false	14.25	2
West	false	1	1
```

Map: Region = Region
Map: SumAmount = Sum of Amount
Map: Customers = Customers
