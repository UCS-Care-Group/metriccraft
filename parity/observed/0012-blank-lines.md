# Parity case 0012 — blank lines

A blank line inside the file and one at the end: we skip both. Power Query may load them as
rows of nulls.
Workflow: `docs/parity.md` (shaping cases).

Covers: Get Data CSV
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Qty
1,5

2,6

```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
