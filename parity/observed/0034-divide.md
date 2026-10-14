# Parity case 0034 — DIVIDE with BLANK, zero and alternate results

Spec X-1 (M3.5): DIVIDE returns BLANK (or the alternate) for a zero or BLANK denominator. Workflow: `docs/parity.md`.

Covers: DIVIDE, BLANK, ISBLANK

## Query

```dax
EVALUATE
ROW(
    "Normal",        DIVIDE(7, 2),
    "Integers",      DIVIDE(6, 3),
    "ZeroDen",       ISBLANK(DIVIDE(1, 0)),
    "ZeroZero",      ISBLANK(DIVIDE(0, 0)),
    "BlankDen",      ISBLANK(DIVIDE(1, BLANK())),
    "BlankNum",      ISBLANK(DIVIDE(BLANK(), 2)),
    "BlankNumAlt",   DIVIDE(BLANK(), 0, -1),
    "AltText",       DIVIDE(1, 0, "none"),
    "Negative",      DIVIDE(-1, 4)
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[Normal]	[Integers]	[ZeroDen]	[ZeroZero]	[BlankDen]	[BlankNum]	[BlankNumAlt]	[AltText]	[Negative]
3.5	2	true	true	true	true		none	-0.25
```

Notes: No errors.
