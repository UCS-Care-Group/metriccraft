# Parity case 0036 — ISERROR and IFERROR

Spec X-3/X-4 (M3.5): type errors and infinity are caught; IFERROR's value when there's no error. Workflow: `docs/parity.md`.

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
