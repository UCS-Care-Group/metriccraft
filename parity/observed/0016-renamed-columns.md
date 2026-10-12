# Parity case 0016 — Renamed Columns

Two renames in one step.
Workflow: `docs/parity.md` (shaping cases).

Covers: Renamed Columns
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
    #"Renamed Columns" = Table.RenameColumns(#"Changed Type",{{"Qty", "Quantity"}, {"Region", "Area"}})
in
    #"Renamed Columns"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
