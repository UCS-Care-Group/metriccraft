# Parity case 0009 — Cell errors on load

`abc` and `x` can't become numbers: Cell errors, loaded as blank, counted by Close & Apply
(`Load errors`; both are in one row, so the count tells cells from rows: we count 2 cells). An
empty field is null, not an error. `2.5` to a whole number: we round half to even (2).
Workflow: `docs/parity.md` (shaping cases).

Covers: Cell error, Changed Type
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Qty,Price
1,5,1.5
2,abc,x
3,,3.5
4,2.5,2.5
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Qty", type text}, {"Price", type text}}),
    #"Changed Type1" = Table.TransformColumnTypes(#"Changed Type",{{"Qty", Int64.Type}, {"Price", type number}})
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
