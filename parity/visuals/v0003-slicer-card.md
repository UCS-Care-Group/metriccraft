# Visual parity case v0003 — slicer selection filters a card

Spec R-3 (M4.6): a slicer on Sales[Region] with North and West selected filters the card's
measure. Workflow: `docs/parity.md` (visual cases).

Covers: card, slicer, Selection

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
Sales[Total] = SUM(Sales[Amount])
```

## Visual

Filter: Sales[Region] = North, West

```json
{
  "type": "card",
  "wells": {"values": [{"measure": "Total"}]}
}
```
