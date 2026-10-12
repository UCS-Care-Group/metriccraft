# Parity case 0005 — Culture en-GB

Same file as case 0006, read in en-GB: `03/04/2026` is 3 April. Thousands separators (`1,234.5`),
and `1,2` (not grouped by three: text for us; .NET-style parsing may read 12).
Workflow: `docs/parity.md` (shaping cases).

Covers: Culture, Changed Type, type detection
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Day,Amount,Grouped,OddGroup
1,03/04/2026,1.5,"1,234.5","1,2"
2,01/02/2026,2,"12,345",3
3,12/11/2026,0.25,"1,000",4
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
