# Parity case 0014 — rows of different widths

The column count is the widest of the first 201 rows (`Columns=4` here); short rows get
nulls. The header row is short, so its 4th name is blank (`Column4` once promoted).
Workflow: `docs/parity.md` (shaping cases).

Covers: Get Data CSV, Promoted Headers
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,A,B
1,x
2,y,z,extra
3,u,v
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=4, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Changed Type" = Table.TransformColumnTypes(Source,{{"Column1", type text}, {"Column2", type text}, {"Column3", type text}, {"Column4", type text}}),
    #"Promoted Headers" = Table.PromoteHeaders(#"Changed Type", [PromoteAllScalars=true])
in
    #"Promoted Headers"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
