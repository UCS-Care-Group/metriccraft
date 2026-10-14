# Visual parity case v0002 — clustered bar with a BLANK category

Spec R-2 (M4.6): a BLANK axis value is its own category; the implicit measure sums Amount per
Product. Workflow: `docs/parity.md` (visual cases).

Covers: clustered bar, BLANK, implicit measure

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

## Visual

```json
{
  "type": "clusteredBar",
  "wells": {
    "axis": [{"table": "Sales", "column": "Product"}],
    "values": [{"table": "Sales", "column": "Amount"}]
  }
}
```
