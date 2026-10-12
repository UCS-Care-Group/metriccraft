# Parity case 0001 — aggregates and BLANK

Covers spec X-1/X-2: SUM, COUNT, COUNTA, COUNTROWS, AVERAGE, MIN, MAX, DIVIDE, CALCULATE, BLANK
propagation and comparison. Workflow: `docs/parity.md`.

## Dataset

Table name: **`Sales`**. Types: `Region` = Text, `Amount` = Decimal number, `Qty` = Whole number.
Empty cells are blanks.

```
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

_Not yet observed._ Power BI Desktop version: · Date: · Regional settings:

| Column        | Value |
|---------------|-------|
| SumAll        |       |
| SumWest       |       |
| CountAmount   |       |
| CountAAmount  |       |
| Rows          |       |
| AvgAmount     |       |
| MinAmount     |       |
| MaxAmount     |       |
| AvgWest       |       |
| DivAmountQty  |       |
| DivByZero     |       |
| DivByZeroAlt  |       |
| DivWestByQty  |       |
| BlankPlusOne  |       |
| BlankTimesTwo |       |
| BlankEqZero   |       |
| BlankEqEmpty  |       |
| BlankStrictEq |       |
| OneSlashZero  |       |

Notes:
