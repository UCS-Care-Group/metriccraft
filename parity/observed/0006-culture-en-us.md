# Parity case 0006 — Culture en-US

Same file as case 0005, read in en-US: `03/04/2026` is 4 March.
Workflow: `docs/parity.md` (shaping cases).

Covers: Culture, Changed Type, type detection
Culture: en-US

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
