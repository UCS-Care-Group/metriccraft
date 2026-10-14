# Visual parity case v0001 — matrix subtotals of a non-additive measure

Spec R-2, CONTEXT.md **Total** (M4.6): a matrix's subtotals are the measure over the whole group,
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
