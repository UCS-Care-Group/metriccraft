# Parity case 0033 — IF and SWITCH with BLANK and missing branches

Spec X-1 (M3.5): a missing else is BLANK; a BLANK condition is false; SWITCH with no match and no default is BLANK; untaken branches are not evaluated. Workflow: `docs/parity.md`.

Covers: IF, SWITCH, BLANK, ISBLANK, /

## Query

```dax
EVALUATE
ROW(
    "IfTrue",          IF(1 < 2, "yes", "no"),
    "IfNoElse",        ISBLANK(IF(1 > 2, "yes")),
    "IfBlankCond",     IF(BLANK(), "yes", "no"),
    "IfZeroCond",      IF(0, "yes", "no"),
    "IfUntaken",       IF(TRUE(), 1, "a" + 1),
    "SwitchMatch",     SWITCH(2, 1, "one", 2, "two", "other"),
    "SwitchDefault",   SWITCH(9, 1, "one", 2, "two", "other"),
    "SwitchNoDefault", ISBLANK(SWITCH(9, 1, "one")),
    "SwitchTrue",      SWITCH(TRUE(), 1 > 2, "a", 2 > 1, "b", "c"),
    "SwitchBlank",     SWITCH(BLANK(), 0, "zero", "other")
)
```
