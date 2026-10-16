# Parity case 0001 — aggregates and BLANK

aggregates, DIVIDE, BLANK propagation and comparison. Workflow: `docs/parity.md`.

Covers: SUM, COUNT, COUNTA, COUNTROWS, AVERAGE, MIN, MAX, CALCULATE, DIVIDE, BLANK, /

## Dataset

Empty cells are blanks.

Table: Sales — Region: text, Amount: decimal, Qty: whole

```tsv
Region	Amount	Qty
North	10.5	2
North		3
South	4	1
West		0
```

## Query

```dax
EVALUATE
ROW(
    "SumAll",        SUM(Sales[Amount]),
    "SumWest",       CALCULATE(SUM(Sales[Amount]), Sales[Region] = "West"),
    "CountAmount",   COUNT(Sales[Amount]),
    "CountAAmount",  COUNTA(Sales[Amount]),
    "Rows",          COUNTROWS(Sales),
    "AvgAmount",     AVERAGE(Sales[Amount]),
    "MinAmount",     MIN(Sales[Amount]),
    "MaxAmount",     MAX(Sales[Amount]),
    "AvgWest",       CALCULATE(AVERAGE(Sales[Amount]), Sales[Region] = "West"),
    "DivAmountQty",  DIVIDE(SUM(Sales[Amount]), SUM(Sales[Qty])),
    "DivByZero",     DIVIDE(1, 0),
    "DivByZeroAlt",  DIVIDE(1, 0, -1),
    "DivWestByQty",  CALCULATE(DIVIDE(SUM(Sales[Amount]), SUM(Sales[Qty])), Sales[Region] = "West"),
    "BlankPlusOne",  BLANK() + 1,
    "BlankTimesTwo", BLANK() * 2,
    "BlankEqZero",   BLANK() = 0,
    "BlankEqEmpty",  BLANK() = "",
    "BlankStrictEq", BLANK() == 0,
    "OneSlashZero",  1 / 0
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-12 · Regional settings: English (United Kingdom)

```tsv
[SumAll]	[SumWest]	[CountAmount]	[CountAAmount]	[Rows]	[AvgAmount]	[MinAmount]	[MaxAmount]	[AvgWest]	[DivAmountQty]	[DivByZero]	[DivByZeroAlt]	[DivWestByQty]	[BlankPlusOne]	[BlankTimesTwo]	[BlankEqZero]	[BlankEqEmpty]	[BlankStrictEq]	[OneSlashZero]
14.5		2	2	4	7.25	4	10.5		2.4166666666666665		-1		1		true	true	false	Infinity
```

Notes: No errors. The DAX query view grid shows numbers rounded to 2 decimal places (DivAmountQty
shows 2.42) and shows infinity as an empty cell (OneSlashZero); copying the result gives full
precision and `Infinity`. Case 0002 explores division by zero further.
