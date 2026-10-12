# Parity case 0024 — Filtered Rows: and, negative number, date

Three conditions joined by `and`: text begins with, `>= -1` (how M prints a negative number),
and a date literal (`#date(2026, 4, 3)`). The null Qty row is dropped.
Workflow: `docs/parity.md` (shaping cases).

Covers: Filtered Rows
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Region,Qty,Day
1,North,0,2026-04-03
2,North,-2,2026-05-01
3,South,5,2026-05-01
4,Norway,3,2026-04-02
5,Nile,,2026-06-01
6,Nome,-1,2026-12-31
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=4, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Region", type text}, {"Qty", Int64.Type}, {"Day", type date}}),
    #"Filtered Rows" = Table.SelectRows(#"Changed Type", each Text.StartsWith([Region], "N") and [Qty] >= -1 and [Day] >= #date(2026, 4, 3))
in
    #"Filtered Rows"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
