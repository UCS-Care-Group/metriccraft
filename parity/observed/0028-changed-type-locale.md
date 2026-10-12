# Parity case 0028 — Changed Type with a locale

US dates in an en-GB file detect as text; a *Changed Type* step with culture `en-US`
(Power Query's *Using Locale*) reads `03/04/2026` as 4 March. Settles how the culture argument
prints (we print it on every Changed Type step of the Query, spaced).
Workflow: `docs/parity.md` (shaping cases).

Covers: Culture, Changed Type
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Day
1,12/31/2026
2,03/04/2026
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=2, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers", {{"Id", Int64.Type}, {"Day", type text}}, "en-US"),
    #"Changed Type1" = Table.TransformColumnTypes(#"Changed Type", {{"Day", type date}}, "en-US")
in
    #"Changed Type1"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
