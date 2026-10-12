# Parity case 0003 — Get Data from a plain CSV

The baseline for every shaping case: a header row, text, whole and decimal columns. Settles the
M Get Data writes (`Get Data M`: `Csv.Document` options, `QuoteStyle`, spacing) and header
promotion + type detection on an ordinary file.
Workflow: `docs/parity.md` (shaping cases).

Covers: Get Data CSV, Promoted Headers, Changed Type
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Region,Qty,Price
1,North,3,2.5
2,South,10,0.1
3,East,-4,1234.5
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
