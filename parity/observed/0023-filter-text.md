# Parity case 0023 — Filtered Rows: text begins with / contains

`Text.StartsWith([Region], "N") or Text.Contains([Region], "or")`, case-sensitive: North,
NORTH and Norway begin with N, Lord contains `or`; Orkney (capital O) and the empty Region
don't.
Workflow: `docs/parity.md` (shaping cases).

Covers: Filtered Rows
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Region
1,North
2,NORTH
3,Orkney
4,
5,Norway
6,Lord
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=2, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Region", type text}}),
    #"Filtered Rows" = Table.SelectRows(#"Changed Type", each Text.StartsWith([Region], "N") or Text.Contains([Region], "or"))
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
