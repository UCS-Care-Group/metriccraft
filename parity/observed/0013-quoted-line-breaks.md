# Parity case 0013 — quoted line breaks

A quoted field with a line break stays one field (`QuoteStyle.Csv`); Get Data M settles
whether Get Data picks `QuoteStyle.Csv` itself. The multi-line column is removed so the copy
stays one line per row; `say "hi"` shows how a quote copies.
Workflow: `docs/parity.md` (shaping cases).

Covers: Get Data CSV, Removed Columns
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Note,Quoted,Qty
1,"two
lines","say ""hi""",5
2,plain,x,6
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=4, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Note", type text}, {"Quoted", type text}, {"Qty", Int64.Type}}),
    #"Removed Columns" = Table.RemoveColumns(#"Changed Type",{"Note"})
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
