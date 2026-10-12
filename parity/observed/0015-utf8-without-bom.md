# Parity case 0015 — UTF-8 without a byte-order mark

We read valid UTF-8 as UTF-8 (`Encoding=65001`); Get Data may pick Windows-1252 and show
`CafÃ©`.
Workflow: `docs/parity.md` (shaping cases).

Covers: Get Data CSV
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Name
1,Café
2,Zoë
3,naïve
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
