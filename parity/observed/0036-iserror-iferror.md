# Parity case 0036 — ISERROR and IFERROR

M3.5: type errors and infinity are caught; IFERROR's value when there's no error. Workflow: `docs/parity.md`.

Covers: ISERROR, IFERROR, /

## Query

```dax
EVALUATE
ROW(
    "TypeError",     ISERROR("a" + 1),
    "NoError",       ISERROR(1 + 1),
    "Infinity",      ISERROR(1 / 0),
    "BlankIsError",  ISERROR(BLANK()),
    "IfErrorType",   IFERROR("a" + 1, -1),
    "IfErrorInf",    IFERROR(1 / 0, -1),
    "IfErrorOk",     IFERROR(2 * 3, -1)
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[TypeError]	[NoError]	[Infinity]	[BlankIsError]	[IfErrorType]	[IfErrorInf]	[IfErrorOk]
true	false	true	false	-1	-1	6
```

Notes: No errors.
