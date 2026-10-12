# Parity case 0025 — Merged Queries: left outer

Sales merges Regions on RegionId. Regions has a duplicate key (2: Bob, Bea), a null key
(Nobody) and an unmatched key (5); Sales has a null key (Id 3) and an unmatched one (9).
We match null to null (M's `null = null`) and repeat a row per match, in the other table's
order.
Workflow: `docs/parity.md` (shaping cases).

Covers: Merged Queries
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,RegionId,Amount
1,1,10
2,2,20
3,,30
4,9,40
```

Source: csv Regions.csv

```csv
RegionId,Manager
1,Ann
2,Bob
2,Bea
,Nobody
5,Eve
```

## M

```m
let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"RegionId", Int64.Type}, {"Amount", Int64.Type}}),
    #"Merged Queries" = Table.NestedJoin(#"Changed Type", {"RegionId"}, Regions, {"RegionId"}, "Regions", JoinKind.LeftOuter),
    #"Expanded Regions" = Table.ExpandTableColumn(#"Merged Queries", "Regions", {"RegionId", "Manager"}, {"Regions.RegionId", "Regions.Manager"})
in
    #"Expanded Regions"
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id], 'Sales'[Regions.Manager]
```

## Observed

Awaiting observation.
