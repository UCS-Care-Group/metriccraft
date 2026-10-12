# Parity case 0018 — Reordered Columns with some columns listed

`Table.ReorderColumns` listing only `Price` and `Region`: we put the listed columns, in list
order, into the positions they held (Id, Price, Qty, Region). `Column order` settles it.
Workflow: `docs/parity.md` (shaping cases).

Covers: Reordered Columns
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
    #"Reordered Columns" = Table.ReorderColumns(#"Changed Type",{"Price", "Region"})
in
    #"Reordered Columns"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
