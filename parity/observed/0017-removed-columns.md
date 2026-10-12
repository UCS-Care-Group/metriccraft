# Parity case 0017 — Removed Columns

Two columns removed in one step.
Workflow: `docs/parity.md` (shaping cases).

Covers: Removed Columns
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Region,Qty,Price
1,North,3,2.5
2,South,4,1.25
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=4, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Region", type text}, {"Qty", Int64.Type}, {"Price", type number}}),
    #"Removed Columns" = Table.RemoveColumns(#"Changed Type",{"Region", "Price"})
in
    #"Removed Columns"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
