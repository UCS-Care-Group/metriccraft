# Parity case 0020 — Filtered Rows: greater than, with nulls

`[Qty] > 2` where Qty has a null: we drop the null row (`null > 2` is null).
Workflow: `docs/parity.md` (shaping cases).

Covers: Filtered Rows
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Qty
1,1
2,3
3,
4,5
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=2, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Qty", Int64.Type}}),
    #"Filtered Rows" = Table.SelectRows(#"Changed Type", each ([Qty] > 2))
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
