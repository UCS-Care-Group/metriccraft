# Visual parity case v0001 — matrix subtotals of a non-additive measure

M4.6, glossary **Total**: a matrix's subtotals are the measure over the whole group,
not the sum of its rows. `Customers` (DISTINCTCOUNT) at North is 2 (Ann, Bob), though its
products show 2 and 1. Workflow: `docs/parity.md` (visual cases).

Covers: matrix, Total, DISTINCTCOUNT, implicit measure

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
  "type": "matrix",
  "wells": {
    "rows": [{"table": "Sales", "column": "Region"}, {"table": "Sales", "column": "Product"}],
    "values": [{"measure": "Customers"}, {"table": "Sales", "column": "Amount"}]
  },
  "format": {"rowSubtotals": true, "columnSubtotals": true}
}
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
Sales[Region]	Sales[Product]	[IsGrandTotalRowTotal]	[IsDM1Total]	[Customers]	[SumAmount]
		true	true	4	26.75
North		false	true	2	11.5
North	Apple	false	false	2	14.5
North	Pear	false	false	1	-3
South		false	true	2	14.25
South		false	false	1	5
South	Apple	false	false	1	7.25
South	Pear	false	false	1	2
West		false	true	1	1
West	Plum	false	false	1	1
```

Map: Region = Region
Map: Product = Product
Map: Customers = Customers
Map: SumAmount = Sum of Amount

Notes: Matrix expanded with "Expand all down one level"; the query has both levels and the subtotals (`IsDM1Total`). The blank Product under South is its own row.
