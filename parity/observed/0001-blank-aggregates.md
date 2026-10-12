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
    "OneSlashZero",  1 / 0,
    "DivAmountQtyE9", DIVIDE(SUM(Sales[Amount]), SUM(Sales[Qty])) * 1000000000
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-12 · Regional settings: English (United Kingdom)

| Column         | Value         |
| -------------- | ------------- |
| SumAll         | 14.5          |
| SumWest        | (blank)       |
| CountAmount    | 2             |
| CountAAmount   | 2             |
| Rows           | 4             |
| AvgAmount      | 7.25          |
| MinAmount      | 4             |
| MaxAmount      | 10.5          |
| AvgWest        | (blank)       |
| DivAmountQty   | 2.42          |
| DivByZero      | (blank)       |
| DivByZeroAlt   | -1            |
| DivWestByQty   | (blank)       |
| BlankPlusOne   | 1             |
| BlankTimesTwo  | (blank)       |
| BlankEqZero    | True          |
| BlankEqEmpty   | True          |
| BlankStrictEq  | False         |
| OneSlashZero   | (blank)       |
| DivAmountQtyE9 | 2416666666.67 |

Notes: No errors. The DAX query view grid shows numbers rounded to 2 decimal places: DivAmountQty
shows 2.42, while ×1e9 shows 2416666666.67, so the stored value is full precision (14.5 / 6).

OneSlashZero shows an empty cell, although public DAX docs say `/` by zero returns Infinity.
Open question for a follow-up case: is the value BLANK, or is Infinity displayed as empty by the
grid? Distinguish with `ISBLANK(1 / 0)` and `1 / 0 > 1000000`.
