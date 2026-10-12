# Parity case 0008 — two-digit years

Where two-digit years turn over: we map 00–49 to 20xx and 50–99 to 19xx (.NET's
`TwoDigitYearMax` = 2049). Also whether `dd/mm/yy` detects as a date at all.
Workflow: `docs/parity.md` (shaping cases).

Covers: Culture, Changed Type, type detection
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Day
1,01/02/49
2,01/02/50
3,31/12/99
4,01/01/00
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
