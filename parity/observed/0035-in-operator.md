# Parity case 0035 — `IN` with table constructors

M3.5: `IN` on single values and row tuples; text matching ignores case; BLANK against 0. Workflow: `docs/parity.md`.

Covers: IN, CALCULATE, SUM

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
    "InList",        2 IN {1, 2, 3},
    "NotInList",     NOT (5 IN {1, 2, 3}),
    "TextCase",      "north" IN {"North", "South"},
    "BlankZero",     BLANK() IN {0},
    "Tuple",         (1, "a") IN {(1, "a"), (2, "b")},
    "InFilter",      CALCULATE(SUM(Sales[Qty]), Sales[Region] IN {"North", "West"})
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[InList]	[NotInList]	[TextCase]	[BlankZero]	[Tuple]	[InFilter]
true	true	true	false	true	5
```

Notes: No errors. `BLANK() IN {0}` is false: IN compares strictly.
