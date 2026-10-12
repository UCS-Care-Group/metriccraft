# Parity case 0010 — headers in an all-text file

Every column is text, so the header row looks like data. We don't promote it (Get Data M
settles whether Power BI does); the case M then uses *Use First Row as Headers* explicitly.
Workflow: `docs/parity.md` (shaping cases).

Covers: Promoted Headers, Get Data CSV
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Name,City
Ann,Leeds
Bob,York
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=2, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Changed Type" = Table.TransformColumnTypes(Source,{{"Column1", type text}, {"Column2", type text}}),
    #"Promoted Headers" = Table.PromoteHeaders(#"Changed Type", [PromoteAllScalars=true])
in
    #"Promoted Headers"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Name]
```

## Observed

Awaiting observation.
