//! M3.11: the storage fast paths (code-bitset column filters, distinct codes, `DISTINCTCOUNT`
//! over whole numbers, the chunked scans) against the general per-row path, on randomised
//! tables: text (dictionaries folded or not, with case variants and null entries), whole
//! numbers (narrow and wide ranges), decimals, dates, nulls, sliced arrays, several chunks.

use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::types::{Int8Type, Int16Type, Int32Type};
use arrow_array::{
    Array, ArrayRef, Date32Array, Decimal64Array, DictionaryArray, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, PrimitiveArray, RecordBatch, StringArray,
};
use opendash_core::Value;
use opendash_data::{Table, TableBuilder};

use super::*;
use crate::Model;
use crate::dax::{Database, Options, run_query_text};

/// xorshift64*: deterministic test data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// Row counts around word and chunk boundaries.
const SIZES: [usize; 7] = [0, 1, 63, 64, 65, 1_000, CHUNK + 130];

/// A non-folded dictionary: case variants, a trailing space, a null entry.
fn spellings() -> StringArray {
    StringArray::from(vec![
        Some("Store"),
        Some("store"),
        Some("STORE"),
        Some("Web"),
        None,
        Some("web "),
        Some("Phone"),
    ])
}

fn keys<T: arrow_array::ArrowPrimitiveType>(
    rng: &mut Rng,
    n: usize,
    codes: u64,
    nulls: u64,
) -> PrimitiveArray<T>
where
    T::Native: TryFrom<u64>,
{
    (0..n)
        .map(|_| {
            if rng.chance(nulls) {
                None
            } else {
                T::Native::try_from(rng.below(codes)).ok()
            }
        })
        .collect()
}

/// Dictionary columns of every key width; `used` codes of the dictionary appear (fewer than
/// its length: unused entries).
fn dict_cols(rng: &mut Rng, n: usize) -> Vec<Col> {
    let wide: StringArray = (0..300)
        .map(|i| {
            Some(if i % 7 == 0 {
                format!("V{i}")
            } else {
                format!("v{i}")
            })
        })
        .collect();
    let mut out = Vec::new();
    for nulls in [0, 10] {
        let used = if rng.chance(50) { 7 } else { 4 };
        let a =
            DictionaryArray::<Int8Type>::try_new(keys(rng, n, used, nulls), Arc::new(spellings()))
                .unwrap();
        out.push(Col::new(&(Arc::new(a) as ArrayRef)));
        let a =
            DictionaryArray::<Int16Type>::try_new(keys(rng, n, 300, nulls), Arc::new(wide.clone()))
                .unwrap();
        out.push(Col::new(&(Arc::new(a) as ArrayRef)));
        let a =
            DictionaryArray::<Int32Type>::try_new(keys(rng, n, 7, nulls), Arc::new(spellings()))
                .unwrap();
        out.push(Col::new(&(Arc::new(a) as ArrayRef)));
    }
    // Sliced: the keys and their validity start mid-word.
    if n > 10 {
        let a =
            DictionaryArray::<Int8Type>::try_new(keys(rng, n + 5, 7, 10), Arc::new(spellings()))
                .unwrap();
        out.push(Col::new(&(Arc::new(a.slice(5, n)) as ArrayRef)));
    }
    out
}

fn number_cols(rng: &mut Rng, n: usize) -> Vec<Col> {
    let mut out = Vec::new();
    let narrow: Int64Array = (0..n)
        .map(|_| (!rng.chance(10)).then(|| rng.below(500) as i64 - 250))
        .collect();
    out.push(Col::new(&(Arc::new(narrow) as ArrayRef)));
    let wide: Int64Array = (0..n)
        .map(|_| (!rng.chance(5)).then(|| (rng.next() >> 1) as i64 - (1 << 62)))
        .collect();
    out.push(Col::new(&(Arc::new(wide) as ArrayRef)));
    let extremes: Int64Array = (0..n)
        .map(|_| Some([i64::MIN, i64::MAX, 0][rng.below(3) as usize]))
        .collect();
    out.push(Col::new(&(Arc::new(extremes) as ArrayRef)));
    let i8s: Int8Array = (0..n)
        .map(|_| (!rng.chance(10)).then(|| rng.below(256) as i64 as i8))
        .collect();
    out.push(Col::new(&(Arc::new(i8s) as ArrayRef)));
    let i16s: Int16Array = (0..n).map(|_| Some(rng.below(40) as i16)).collect();
    out.push(Col::new(&(Arc::new(i16s) as ArrayRef)));
    let i32s: Int32Array = (0..n + 3)
        .map(|_| (!rng.chance(20)).then(|| rng.below(100_000) as i32))
        .collect();
    out.push(Col::new(&(Arc::new(i32s.slice(3, n)) as ArrayRef)));
    let dates: Date32Array = (0..n)
        .map(|_| (!rng.chance(10)).then(|| 19_000 + rng.below(30) as i32))
        .collect();
    out.push(Col::new(&(Arc::new(dates) as ArrayRef)));
    let floats: Float64Array = (0..n)
        .map(|_| (!rng.chance(10)).then(|| rng.below(1000) as f64 / 8.0))
        .collect();
    out.push(Col::new(&(Arc::new(floats) as ArrayRef)));
    let cents: Decimal64Array = (0..n)
        .map(|_| (!rng.chance(10)).then(|| rng.below(100_000) as i64))
        .collect::<Decimal64Array>()
        .with_precision_and_scale(18, 2)
        .unwrap();
    out.push(Col::new(&(Arc::new(cents) as ArrayRef)));
    out
}

fn masks(rng: &mut Rng, n: usize) -> Vec<Option<Bitset>> {
    let mut out = vec![
        None,
        Some(Bitset::new(n, false)),
        Some(Bitset::new(n, true)),
    ];
    for density in [3, 50] {
        let mut m = Bitset::new(n, false);
        for r in 0..n {
            if rng.chance(density) {
                m.set(r);
            }
        }
        out.push(Some(m));
    }
    out
}

fn visible(n: usize, mask: Option<&Bitset>) -> Vec<usize> {
    (0..n).filter(|&r| mask.is_none_or(|m| m.get(r))).collect()
}

/// The general path's distinct values: per row, in first-seen order, keyed ignoring case.
fn general_distinct(col: &Col, n: usize, mask: Option<&Bitset>) -> Vec<Value> {
    let mut seen = HashSet::new();
    visible(n, mask)
        .into_iter()
        .map(|r| col.value(r))
        .filter(|v| seen.insert(Key::of(v, false)))
        .collect()
}

#[test]
fn scan_visits_the_masked_rows_in_order() {
    let mut rng = Rng(7);
    for n in SIZES {
        for mask in masks(&mut rng, n) {
            let mut got = Vec::new();
            scan(n, mask.as_ref(), Cancel::default(), |r| {
                got.push(r);
                Ok(())
            })
            .unwrap();
            assert_eq!(got, visible(n, mask.as_ref()), "{n} rows");
            // A scan shorter than the mask stops at its end.
            let short = n / 2;
            let mut got = Vec::new();
            scan(short, mask.as_ref(), Cancel::default(), |r| {
                got.push(r);
                Ok(())
            })
            .unwrap();
            assert_eq!(got, visible(short, mask.as_ref()));
        }
    }
}

#[test]
fn code_scans_match_the_per_row_codes() {
    let mut rng = Rng(11);
    for n in SIZES {
        for col in dict_cols(&mut rng, n) {
            let len = col.dictionary().map_or(0, Array::len);
            for mask in masks(&mut rng, n) {
                let mut got = Vec::new();
                each_code(&col, n, mask.as_ref(), Cancel::default(), |r, c| {
                    got.push((r, c));
                    Ok(())
                })
                .unwrap();
                let want: Vec<_> = visible(n, mask.as_ref())
                    .into_iter()
                    .map(|r| (r, col.code(r)))
                    .collect();
                assert_eq!(got, want);
            }
            for blank in [false, true] {
                let mut allowed = Bitset::new(len, false);
                for c in 0..len {
                    if rng.chance(40) {
                        allowed.set(c);
                    }
                }
                // Past the column's end too: those rows are BLANK.
                let rows = n + 70;
                let got = code_mask(&col, rows, &allowed, blank, Cancel::default()).unwrap();
                let mut want = Bitset::new(rows, false);
                for r in 0..rows {
                    if col.code(r).map_or(blank, |c| allowed.get(c)) {
                        want.set(r);
                    }
                }
                assert_eq!(got, want, "{n} rows, blank {blank}");
            }
        }
    }
}

#[test]
fn distinct_values_match_the_general_path() {
    let mut rng = Rng(13);
    for n in SIZES {
        let mut cols = dict_cols(&mut rng, n);
        cols.extend(number_cols(&mut rng, n));
        for col in &cols {
            for mask in masks(&mut rng, n) {
                let got = distinct_values(col, n, mask.as_ref(), Cancel::default()).unwrap();
                assert_eq!(got, general_distinct(col, n, mask.as_ref()), "{n} rows");
                // Past the column's end: BLANK, once.
                let got = distinct_values(col, n + 3, None, Cancel::default()).unwrap();
                assert_eq!(got, general_distinct(col, n + 3, None));
            }
        }
    }
}

#[test]
fn first_codes_stop_early_only_when_nothing_is_left_to_see() {
    // Every code early, no nulls: the scan may stop, and must not invent a BLANK.
    let keys = Int8Array::from_iter_values((0..200_000).map(|i| (i % 3) as i8));
    let dict = StringArray::from(vec!["a", "b", "c"]);
    let a = DictionaryArray::<Int8Type>::try_new(keys, Arc::new(dict)).unwrap();
    let col = Col::new(&(Arc::new(a) as ArrayRef));
    let codes = first_codes(&col, 200_000, None, Cancel::default()).unwrap();
    assert_eq!(codes, [Some(0), Some(1), Some(2)]);
    // A null key at the very end is still found.
    let mut v: Vec<Option<i8>> = (0..200_000).map(|i| Some((i % 3) as i8)).collect();
    v.push(None);
    let dict = StringArray::from(vec!["a", "b", "c"]);
    let a = DictionaryArray::<Int8Type>::try_new(Int8Array::from(v), Arc::new(dict)).unwrap();
    let col = Col::new(&(Arc::new(a) as ArrayRef));
    let codes = first_codes(&col, 200_001, None, Cancel::default()).unwrap();
    assert_eq!(codes, [Some(0), Some(1), Some(2), None]);
    // Rows past the column: BLANK.
    let codes = first_codes(&col, 200_005, None, Cancel::default()).unwrap();
    assert_eq!(codes, [Some(0), Some(1), Some(2), None]);
}

#[test]
fn distinct_count_matches_the_general_path() {
    let mut rng = Rng(17);
    for n in SIZES {
        let mut cols = dict_cols(&mut rng, n);
        cols.extend(number_cols(&mut rng, n));
        for col in &cols {
            for mask in masks(&mut rng, n) {
                let got = distinct_count(col, n, mask.as_ref(), Cancel::default()).unwrap();
                let want = general_distinct(col, n, mask.as_ref()).len();
                assert_eq!(got, want, "{n} rows, {:?}", col.kind());
            }
            let got = distinct_count(col, n + 2, None, Cancel::default()).unwrap();
            assert_eq!(got, general_distinct(col, n + 2, None).len());
        }
    }
}

#[test]
fn number_scans_match_the_per_row_values() {
    let mut rng = Rng(19);
    for n in SIZES {
        for col in number_cols(&mut rng, n) {
            for mask in masks(&mut rng, n) {
                let mut got = Vec::new();
                each_number(&col, n, mask.as_ref(), Cancel::default(), |x| {
                    got.push(x);
                    Ok(())
                })
                .unwrap();
                let want: Vec<Number> = visible(n, mask.as_ref())
                    .into_iter()
                    .filter_map(|r| match col.value(r) {
                        Value::Integer(i) => Some(Number::Int(i)),
                        Value::Currency(c) => Some(Number::Currency(c)),
                        Value::Decimal(f) | Value::DateTime(f) => Some(Number::Float(f)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(got, want);
            }
        }
    }
}

#[test]
fn cancel_stops_the_fast_scans() {
    let flag = std::sync::atomic::AtomicBool::new(true);
    let cancel = Cancel(Some(&flag));
    let mut rng = Rng(23);
    let n = CHUNK + 1;
    for col in dict_cols(&mut rng, n) {
        let allowed = Bitset::new(7, true);
        assert!(code_mask(&col, n, &allowed, true, cancel).is_err());
        assert!(distinct_values(&col, n, None, cancel).is_err());
        assert!(distinct_count(&col, n, None, cancel).is_err());
    }
    for col in number_cols(&mut rng, n) {
        assert!(distinct_count(&col, n, None, cancel).is_err());
        assert!(each_number(&col, n, None, cancel, |_| Ok(())).is_err());
    }
}

// ---- through DAX: the boolean CALCULATE filter against FILTER over the Table ----

fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    b.finish().unwrap()
}

fn query(db: &Database, q: &str) -> Vec<Vec<Value>> {
    let options = Options {
        cancel: None,
        memory_cap: usize::MAX,
    };
    run_query_text(db, q, options)
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .remove(0)
        .rows
}

#[test]
fn calculate_column_filters_match_filter_over_the_table() {
    let mut rng = Rng(29);
    // Not folded at load: the dictionary keeps every spelling.
    let words = ["Store", "store", "STORE", "Web", "web", "Phone"];
    for n in [1, 200, CHUNK + 300] {
        let channel: StringArray = (0..n)
            .map(|_| (!rng.chance(10)).then(|| words[rng.below(6) as usize]))
            .collect();
        let amount: Float64Array = (0..n)
            .map(|_| (!rng.chance(10)).then(|| rng.below(1000) as f64 / 4.0))
            .collect();
        let customer: Int64Array = (0..n)
            .map(|_| (!rng.chance(5)).then(|| rng.below(50) as i64))
            .collect();
        let qty: Int64Array = (0..n).map(|_| Some(rng.below(9) as i64)).collect();
        let t = table(
            "T",
            vec![
                ("Channel", Arc::new(channel) as ArrayRef),
                ("Amount", Arc::new(amount)),
                ("Customer", Arc::new(customer)),
                ("Qty", Arc::new(qty)),
            ],
        );
        let db = Database::new(&Model::default(), &[t]);
        assert!(
            db.tables[0].columns[0].data.dictionary().is_some(),
            "text loads as a dictionary"
        );
        let big = n > CHUNK;
        for (i, cond) in [
            "T[Channel] = \"store\"",
            "T[Channel] = \"WEB\"",
            "T[Channel] <> \"Store\"",
            "T[Channel] IN {\"store\", \"phone\"}",
            "NOT (T[Channel] IN {\"STORE\"})",
            "ISBLANK(T[Channel])",
            "T[Customer] = 7",
            "T[Customer] IN {1, 2, 3}",
            "T[Customer] > 40",
            "T[Qty] >= 5",
        ]
        .into_iter()
        .enumerate()
        {
            if big && i % 3 != 0 {
                continue;
            }
            for (j, measure) in [
                "SUM(T[Amount])",
                "COUNTROWS(T)",
                "DISTINCTCOUNT(T[Customer])",
                "DISTINCTCOUNT(T[Channel])",
                "SUM(T[Qty])",
            ]
            .into_iter()
            .enumerate()
            {
                if big && j % 2 != 0 {
                    continue;
                }
                let fast = query(
                    &db,
                    &format!("EVALUATE ROW(\"v\", CALCULATE({measure}, {cond}))"),
                );
                let general = query(
                    &db,
                    &format!("EVALUATE ROW(\"v\", CALCULATE({measure}, FILTER(T, {cond})))"),
                );
                assert_eq!(fast, general, "{n} rows: CALCULATE({measure}, {cond})");
            }
        }
        // ALL over the column: distinct spellings in first-seen order, ignoring case.
        let col = &db.tables[0].columns[0].data;
        let want: Vec<Vec<Value>> = general_distinct(col, n, None)
            .into_iter()
            .map(|v| vec![v])
            .collect();
        assert_eq!(query(&db, "EVALUATE ALL(T[Channel])"), want);
    }
}

#[test]
fn value_filters_match_the_per_row_keys() {
    let mut rng = Rng(31);
    for n in SIZES {
        let mut cols = dict_cols(&mut rng, n);
        cols.extend(number_cols(&mut rng, n));
        for col in cols {
            let t = DbTable {
                name: "T".into(),
                columns: vec![DbColumn {
                    name: "C".into(),
                    data: col,
                }],
                rows: n + 40,
            };
            let col = &t.columns[0].data;
            for _ in 0..4 {
                // Some of the column's values (BLANK maybe among them), and some it lacks.
                let mut values: Vec<Value> = (0..1 + rng.below(20))
                    .map(|_| col.value(rng.below(n.max(1) as u64) as usize))
                    .collect();
                values.extend([
                    Value::Integer(-1),
                    Value::Decimal(2.5),
                    Value::text("sTORE"),
                ]);
                if rng.chance(50) {
                    values.retain(|v| !v.is_blank());
                }
                let filter = Filter::values(&t, 0, 0, &values);
                let got = filter.rows(&t, Cancel::default()).unwrap();
                let keys: HashSet<Option<Key>> = values.iter().map(|v| Key::of(v, false)).collect();
                let mut want = Bitset::new(t.rows, false);
                for r in 0..t.rows {
                    if keys.contains(&Key::of(&col.value(r), false)) {
                        want.set(r);
                    }
                }
                assert_eq!(got, want, "{n} rows, {:?}", col.kind());
            }
        }
    }
}
