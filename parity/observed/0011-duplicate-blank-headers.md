# Parity case 0011 — duplicate and blank headers

Promoting a header row with a repeated name and an empty cell: we name them `Name_1` and
`Column4`. A blank header cell also stops our automatic promotion (Get Data M settles
Power BI's).
Workflow: `docs/parity.md` (shaping cases).

Covers: Promoted Headers, Get Data CSV
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Name,Name,,Qty
1,a,b,c,5
2,d,e,f,6
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=5, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Changed Type" = Table.TransformColumnTypes(Source,{{"Column1", type text}, {"Column2", type text}, {"Column3", type text}, {"Column4", type text}, {"Column5", type text}}),
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
