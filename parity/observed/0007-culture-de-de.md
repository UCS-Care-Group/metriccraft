# Parity case 0007 — Culture de-DE

German numbers and dates (`1.234,5`, `03.04.2026`) in a semicolon-separated file (also settles
delimiter detection).
Workflow: `docs/parity.md` (shaping cases).

Covers: Culture, Changed Type, type detection, Get Data CSV
Culture: de-DE

## Dataset

Source: csv Sales.csv

```csv
Id;Day;Amount;Grouped
1;03.04.2026;1,5;1.234,5
2;01.02.2026;2;12.345
3;12.11.2026;0,25;1.000
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
