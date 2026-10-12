# Parity case 0004 — type detection

Which type Get Data's automatic *Changed Type* picks per column: whole vs decimal (`1.0`),
booleans in any case, ISO dates and datetimes, number + text and number + boolean mixes (text),
an all-empty column. Read the types off `Get Data M`; the result shows how values loaded.
Workflow: `docs/parity.md` (shaping cases).

Covers: Changed Type, type detection
Culture: en-GB

## Dataset

Source: csv Sales.csv

```csv
Id,Whole,Dec,OnePointZero,Flag,Day,Stamp,Mixed,NumBool,Empty
1,10,1.5,1.0,TRUE,2026-04-03,2026-04-03 10:30,1,1,
2,-3,2,2.0,false,2026-12-31,2026-12-31 23:59:59,x,true,
3,,,,,,,,,
```

## Query

```dax
EVALUATE 'Sales'
ORDER BY 'Sales'[Id]
```

## Observed

Awaiting observation.
