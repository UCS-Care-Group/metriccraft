# Parity case 0042 — FORMAT: named and custom codes, four locales

M3.8: `FORMAT` named formats and custom numeric and date/time codes, with the report's
regional settings (English (United Kingdom)) and the `locale_name` argument for en-US, de-DE and
fr-FR. If a column errors, note the message verbatim and remove it so the rest can be copied.
Workflow: `docs/parity.md`.

Covers: FORMAT, ISBLANK, MIN

Things to look at while copying (our guesses, see `crates/core/src/format.rs`):
- the character fr-FR groups thousands with (U+202F narrow no-break space or U+00A0) and the one
  before `€` in de-DE / fr-FR — the copy keeps it; paste it verbatim;
- whether en-US negative currency is `($1,234.50)` or `-$1,234.50`;
- how many digits General Number shows for `1/3` and `0.1 + 0.2` (15 or 17 significant);
- whether `-0.001` with `0.00` keeps its minus sign.

## Dataset

Empty cells are blanks.

Table: Days — Day: date, At: datetime

```tsv
Day	At
2026-01-01	2026-01-01 06:00:00
2026-01-31	2026-01-02 18:30:00
2024-02-29	
```

## Query

```dax
EVALUATE
ROW(
    "GeneralNumber",   FORMAT(1234.5, "General Number"),
    "GeneralThird",    FORMAT(1 / 3, "General Number"),
    "GeneralSum",      FORMAT(0.1 + 0.2, "General Number"),
    "GeneralBig",      FORMAT(1.5E+20, "General Number"),
    "GeneralSmall",    FORMAT(0.00001, "General Number"),
    "GeneralNeg",      FORMAT(-42, "General Number"),
    "Currency",        FORMAT(1234.5, "Currency"),
    "CurrencyNeg",     FORMAT(-1234.5, "Currency"),
    "Fixed",           FORMAT(1234.567, "Fixed"),
    "Standard",        FORMAT(1234.567, "Standard"),
    "StandardNeg",     FORMAT(-1234.567, "Standard"),
    "Percent",         FORMAT(0.256, "Percent"),
    "Scientific",      FORMAT(1234.5, "Scientific"),
    "YesNo",           FORMAT(2, "Yes/No"),
    "YesNoZero",       FORMAT(0, "Yes/No"),
    "TrueFalse",       FORMAT(1, "True/False"),
    "OnOff",           FORMAT(0, "On/Off"),
    "Grouped",         FORMAT(1234567.891, "#,##0.00"),
    "Placeholders",    FORMAT(5, "000.0#"),
    "NoLeadingZero",   FORMAT(0.5, "#.##"),
    "Thousands",       FORMAT(1234567, "#,##0,"),
    "Millions",        FORMAT(1234567, "0.0,,"),
    "PercentCode",     FORMAT(0.5, "0%"),
    "ExpCode",         FORMAT(123456, "0.00E+00"),
    "ExpLower",        FORMAT(0.0012, "0.0e-0"),
    "Escaped",         FORMAT(42, "0\k"),
    "Quoted",          FORMAT(42, "0"" units"""),
    "SectionPos",      FORMAT(1.5, "0.00;(0.00);""zero"""),
    "SectionNeg",      FORMAT(-1.5, "0.00;(0.00);""zero"""),
    "SectionZero",     FORMAT(0, "0.00;(0.00);""zero"""),
    "EmptyNegSection", FORMAT(-1.5, "0.00;"),
    "RoundHalf",       FORMAT(2.5, "0"),
    "RoundNegHalf",    FORMAT(-2.5, "0"),
    "RoundDecimal",    FORMAT(2.675, "0.00"),
    "NegToZero",       FORMAT(-0.001, "0.00"),
    "Blank",           FORMAT(BLANK(), "0.00"),
    "BlankIsBlank",    ISBLANK(FORMAT(BLANK(), "0.00")),
    "Infinity",        FORMAT(1 / 0, "#,##0.00"),
    "Text",            FORMAT("abc", "0.00"),
    "TextNumber",      FORMAT("1.5", "0.00"),
    "GeneralDate",     FORMAT(MIN(Days[At]), "General Date"),
    "MidnightDate",    FORMAT(MIN(Days[Day]), "General Date"),
    "LongDate",        FORMAT(MIN(Days[At]), "Long Date"),
    "MediumDate",      FORMAT(MIN(Days[At]), "Medium Date"),
    "ShortDate",       FORMAT(MIN(Days[At]), "Short Date"),
    "LongTime",        FORMAT(MIN(Days[At]), "Long Time"),
    "MediumTime",      FORMAT(MIN(Days[At]), "Medium Time"),
    "ShortTime",       FORMAT(MIN(Days[At]), "Short Time"),
    "DateLong",        FORMAT(MIN(Days[At]), "dddd d mmmm yyyy"),
    "DateShort",       FORMAT(MIN(Days[At]), "ddd dd mmm yy"),
    "DateIso",         FORMAT(MIN(Days[At]), "yyyy-mm-dd hh:nn:ss"),
    "DateSlashes",     FORMAT(MIN(Days[At]), "d/m/yyyy"),
    "TimeAmPm",        FORMAT(MIN(Days[At]), "h:mm AM/PM"),
    "MinSec",          FORMAT(MIN(Days[At]), "mm:ss"),
    "Quarter",         FORMAT(MIN(Days[At]), """Q""q yyyy"),
    "DayWeek",         FORMAT(MIN(Days[At]), "y w ww"),
    "NumberAsDate",    FORMAT(1, "Short Date"),
    "CurrencyUS",      FORMAT(1234.5, "Currency", "en-US"),
    "CurrencyUSNeg",   FORMAT(-1234.5, "Currency", "en-US"),
    "CurrencyDE",      FORMAT(1234.5, "Currency", "de-DE"),
    "CurrencyDENeg",   FORMAT(-1234.5, "Currency", "de-DE"),
    "CurrencyFR",      FORMAT(1234.5, "Currency", "fr-FR"),
    "StandardDE",      FORMAT(1234567.891, "Standard", "de-DE"),
    "StandardFR",      FORMAT(1234567.891, "Standard", "fr-FR"),
    "GeneralDE",       FORMAT(1234.5, "General Number", "de-DE"),
    "ShortDateUS",     FORMAT(MIN(Days[At]), "Short Date", "en-US"),
    "ShortDateDE",     FORMAT(MIN(Days[At]), "Short Date", "de-DE"),
    "ShortDateFR",     FORMAT(MIN(Days[At]), "Short Date", "fr-FR"),
    "LongDateUS",      FORMAT(MIN(Days[At]), "Long Date", "en-US"),
    "LongDateDE",      FORMAT(MIN(Days[At]), "Long Date", "de-DE"),
    "LongDateFR",      FORMAT(MIN(Days[At]), "Long Date", "fr-FR"),
    "LongTimeUS",      FORMAT(MIN(Days[At]), "Long Time", "en-US"),
    "GeneralDateUS",   FORMAT(MIN(Days[At]), "General Date", "en-US"),
    "MonthFR",         FORMAT(MIN(Days[At]), "mmm", "fr-FR"),
    "DayDE",           FORMAT(MIN(Days[At]), "ddd", "de-DE")
)
```

## Observed

Power BI Desktop version: 2.158.1304.0 64-bit (September 2026) · Date: 2026-10-14 · Regional settings: English (United Kingdom)

```tsv
[GeneralNumber]	[GeneralThird]	[GeneralSum]	[GeneralBig]	[GeneralSmall]	[GeneralNeg]	[Currency]	[CurrencyNeg]	[Fixed]	[Standard]	[StandardNeg]	[Percent]	[Scientific]	[YesNo]	[YesNoZero]	[TrueFalse]	[OnOff]	[Grouped]	[Placeholders]	[NoLeadingZero]	[Thousands]	[Millions]	[PercentCode]	[ExpCode]	[ExpLower]	[Escaped]	[Quoted]	[SectionPos]	[SectionNeg]	[SectionZero]	[EmptyNegSection]	[RoundHalf]	[RoundNegHalf]	[RoundDecimal]	[NegToZero]	[Blank]	[BlankIsBlank]	[Infinity]	[Text]	[TextNumber]	[GeneralDate]	[MidnightDate]	[LongDate]	[MediumDate]	[ShortDate]	[LongTime]	[MediumTime]	[ShortTime]	[DateLong]	[DateShort]	[DateIso]	[DateSlashes]	[TimeAmPm]	[MinSec]	[Quarter]	[DayWeek]	[NumberAsDate]	[CurrencyUS]	[CurrencyUSNeg]	[CurrencyDE]	[CurrencyDENeg]	[CurrencyFR]	[StandardDE]	[StandardFR]	[GeneralDE]	[ShortDateUS]	[ShortDateDE]	[ShortDateFR]	[LongDateUS]	[LongDateDE]	[LongDateFR]	[LongTimeUS]	[GeneralDateUS]	[MonthFR]	[DayDE]
1234.5	0.333333333333333	0.3	1.5E+20	0.00001	-42	£1,234.50	-£1,234.50	1234.57	1,234.57	-1,234.57	25.60%	1.23E+03	Yes	No	True	Off	1,234,567.89	005.0	.5	1,235	1234567.0	50%	1.23E+05	1.2e-3	42k	42 units	1.50	(1.50)	zero	-1.50	3	-3	2.68	0.00		false	inf	abc	1.5	01/01/2026 06:00:00	29/02/2024	01 January 2026	01-Jan-26	01/01/2026	06:00:00	06:00 AM	06:00	Thursday 1 January 2026	Thu 01 Jan 26	2026-01-01 06:00:00	1/1/2026	6:00 AM	01:00	Q1 2026	1 4 1	31/12/1899	$1,234.50	($1,234.50)	1.234,50 €	-1.234,50 €	1 234,50 €	1.234.567,89	1 234 567,89	1234,5	1/1/2026	01.01.2026	01/01/2026	Thursday, January 1, 2026	Donnerstag, 1. Januar 2026	jeudi 1 janvier 2026	6:00:00 AM	1/1/2026 6:00:00 AM	janv	Do
```

Notes: No errors. All 75 columns copied. Observed on the three-row Days table of case 0041 (the
original one-row table was replaced to match what was run).
