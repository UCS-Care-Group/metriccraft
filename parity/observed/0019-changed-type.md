# Parity case 0019 — Changed Type between typed columns

A second *Changed Type* on typed columns: whole to text, decimal to whole (we round half to
even: 2, 4, -2), logical to text, date to text (we write ISO `2026-04-03`; Power Query likely
uses the Culture), datetime to date.
Workflow: `docs/parity.md` (shaping cases).

Covers: Changed Type
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Price,Flag,Day,Stamp
1,2.5,true,2026-04-03,2026-04-03 10:30
2,3.5,false,2026-12-31,2026-12-31 23:59:59
3,-2.5,TRUE,2026-01-01,2026-01-01 00:00
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=5, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Price", type number}, {"Flag", type logical}, {"Day", type date}, {"Stamp", type datetime}}),
    #"Changed Type1" = Table.TransformColumnTypes(#"Changed Type",{{"Id", type text}, {"Price", Int64.Type}, {"Flag", type text}, {"Day", type text}, {"Stamp", type date}})
in
    #"Changed Type1"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Price]
```

## Observed

Awaiting observation.
