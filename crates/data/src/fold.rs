//! Case-insensitive text at load (ADR 0006, M3.10): in the Model, text values that differ only
//! by case are one value, spelled as the first row that has it (CONTEXT.md, Column). Power Query
//! (Preview, merges within a load) stays case-sensitive: only the load folds.
//!
//! **The fold** is `str::to_lowercase`: Unicode's default (locale-invariant) lowercase mapping,
//! no accent folding. So `"É"`/`"é"` are one value, `"e"`/`"é"` are two; `"ẞ"`/`"ß"` are one,
//! but `"SS"`/`"ß"` are two (lowercasing never expands `ß`; full case folding would); a final
//! capital sigma lowercases to `ς` (`"ΟΔΟΣ"` is `"οδος"`).
//!
//! **Cost.** One pass over each dictionary's values hashing their lowercase (8 bytes each,
//! sorted to find duplicates); with none, the column is returned as it was (same buffers). Only
//! a column with a duplicate re-keys its rows, and only the values whose hash repeats are
//! compared exactly.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{ArrowDictionaryKeyType, Int8Type, Int16Type, Int32Type};
use arrow_array::{Array, ArrayRef, DictionaryArray, PrimitiveArray, StringArray};
use arrow_buffer::ArrowNativeType;
use arrow_schema::DataType;

use crate::{Error, Result, Table};

/// `table` with each dictionary text column's values that differ only by case made one, kept
/// as the first row spells it; the same table (buffers shared) when no column has any. The new
/// columns may use at most `limit` bytes.
pub fn fold_case(table: Table, limit: usize) -> Result<Table> {
    let error = |message: String| Error::Arrow {
        table: table.name().to_string(),
        message,
    };
    let mut columns: Option<Vec<ArrayRef>> = None;
    let mut used = 0usize;
    for (i, column) in table.batch().columns().iter().enumerate() {
        let folded = match column.data_type() {
            DataType::Dictionary(key, value) if **value == DataType::Utf8 => match **key {
                DataType::Int8 => fold::<Int8Type>(column, limit.saturating_sub(used)),
                DataType::Int16 => fold::<Int16Type>(column, limit.saturating_sub(used)),
                DataType::Int32 => fold::<Int32Type>(column, limit.saturating_sub(used)),
                _ => Ok(None),
            },
            _ => Ok(None),
        };
        let folded = match folded {
            Ok(f) => f,
            Err(Fold::Cap) => {
                return Err(Error::MemoryCap {
                    table: table.name().to_string(),
                    limit,
                });
            }
            Err(Fold::Bad(message)) => return Err(error(message)),
        };
        if let Some(new) = folded {
            used = used.saturating_add(new.get_array_memory_size());
            let all = columns.get_or_insert_with(|| table.batch().columns().to_vec());
            if let Some(slot) = all.get_mut(i) {
                *slot = new;
            }
        }
    }
    match columns {
        None => Ok(table),
        Some(columns) => table.with_folded(columns),
    }
}

enum Fold {
    /// The folded column would pass the limit.
    Cap,
    /// The column isn't a valid dictionary (a key out of range…).
    Bad(String),
}

/// `table` marked folded without checking: for data that was folded when it loaded (the Data
/// cache), so a Preview never merges against it.
pub fn assume_folded(table: Table) -> Result<Table> {
    let columns = table.batch().columns().to_vec();
    table.with_folded(columns)
}

/// The folded column, or `None` if no two values differ only by case.
fn fold<K: ArrowDictionaryKeyType>(
    column: &ArrayRef,
    limit: usize,
) -> std::result::Result<Option<ArrayRef>, Fold> {
    let Some(dict) = column.as_dictionary_opt::<K>() else {
        return Ok(None);
    };
    let Some(values) = dict.values().as_string_opt::<i32>() else {
        return Ok(None);
    };
    let mut buf = String::new();
    // ponytail: FNV-1a over the lowercase bytes, ASCII lowered on the fly (no copy); SipHash
    // cost ~25% of a 1M-row import. Collisions only cost an exact compare below.
    let hash = |s: &str, buf: &mut String| {
        let fnv = |h: u64, b: u8| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        let start = 0xcbf2_9ce4_8422_2325;
        if s.is_ascii() {
            s.bytes().fold(start, |h, b| fnv(h, b.to_ascii_lowercase()))
        } else {
            lower(s, buf).bytes().fold(start, fnv)
        }
    };
    let mut hashes: Vec<u64> = values.iter().flatten().map(|s| hash(s, &mut buf)).collect();
    hashes.sort_unstable();
    let repeated: Vec<u64> = hashes
        .windows(2)
        .filter_map(|w| match w {
            [a, b] if a == b => Some(*a),
            _ => None,
        })
        .collect();
    drop(hashes);
    if repeated.is_empty() {
        return Ok(None);
    }
    // `rep[i]`: the first value (in dictionary order) with value `i`'s lowercase.
    let mut rep: Vec<usize> = (0..values.len()).collect();
    let mut first: HashMap<String, usize> = HashMap::new();
    let mut merged = false;
    for (i, s) in values.iter().enumerate() {
        let Some(s) = s else { continue };
        if repeated.binary_search(&hash(s, &mut buf)).is_err() {
            continue;
        }
        let key = lower(s, &mut buf).to_string();
        let r = *first.entry(key).or_insert(i);
        if let Some(slot) = rep.get_mut(i) {
            *slot = r;
        }
        merged |= r != i;
    }
    if !merged {
        // Only hash collisions, no two values really alike.
        return Ok(None);
    }
    drop(first);
    let keys = dict.keys();
    let need = keys
        .len()
        .saturating_mul(std::mem::size_of::<K::Native>())
        .saturating_add(values.get_array_memory_size());
    if need > limit {
        return Err(Fold::Cap);
    }
    // New values in order of first row; each spelled as that row has it.
    const NONE: usize = usize::MAX;
    let mut new_of: Vec<usize> = vec![NONE; values.len()];
    let mut spelled: Vec<Option<&str>> = Vec::new();
    let mut new_keys: Vec<K::Native> = Vec::with_capacity(keys.len());
    for (row, &k) in keys.values().iter().enumerate() {
        if keys.is_null(row) {
            new_keys.push(K::Native::usize_as(0));
            continue;
        }
        let old = k.as_usize();
        let group = *rep
            .get(old)
            .ok_or_else(|| Fold::Bad(format!("dictionary key {old} is out of range")))?;
        let slot = new_of
            .get_mut(group)
            .ok_or_else(|| Fold::Bad(format!("dictionary key {old} is out of range")))?;
        if *slot == NONE {
            *slot = spelled.len();
            spelled.push(if values.is_null(old) {
                None
            } else {
                Some(values.value(old))
            });
        }
        let new = K::Native::from_usize(*slot)
            .ok_or_else(|| Fold::Bad("too many distinct values".into()))?;
        new_keys.push(new);
    }
    let values: ArrayRef = Arc::new(StringArray::from(spelled));
    let keys = PrimitiveArray::<K>::new(new_keys.into(), keys.nulls().cloned());
    let folded =
        DictionaryArray::<K>::try_new(keys, values).map_err(|e| Fold::Bad(e.to_string()))?;
    Ok(Some(Arc::new(folded)))
}

/// `s` lowercased (`str::to_lowercase`), borrowing `s` when it has nothing to lower.
fn lower<'a>(s: &'a str, buf: &'a mut String) -> &'a str {
    if s.is_ascii() {
        if !s.bytes().any(|b| b.is_ascii_uppercase()) {
            return s;
        }
        buf.clear();
        buf.push_str(s);
        buf.make_ascii_lowercase();
        return buf;
    }
    *buf = s.to_lowercase();
    buf
}

#[cfg(test)]
mod tests {
    use arrow_array::{Int64Array, RecordBatch};

    use super::*;
    use crate::TableBuilder;

    fn table(text: &[Option<&str>]) -> Table {
        let batch = RecordBatch::try_from_iter([
            (
                "Name",
                Arc::new(StringArray::from(text.to_vec())) as ArrayRef,
            ),
            (
                "N",
                Arc::new(Int64Array::from_iter_values(0..text.len() as i64)) as ArrayRef,
            ),
        ])
        .unwrap();
        let mut b = TableBuilder::new("T", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        b.finish().unwrap()
    }

    /// The `Name` column's rows, and its dictionary's values.
    fn names(t: &Table) -> (Vec<Option<String>>, Vec<String>) {
        let c = t.batch().column(0);
        let d = c.as_dictionary::<Int8Type>();
        let values = d.values().as_string::<i32>();
        let rows = d
            .keys()
            .iter()
            .map(|k| k.map(|k| values.value(k as usize).to_string()))
            .collect();
        (rows, values.iter().flatten().map(String::from).collect())
    }

    fn fold_names(text: &[Option<&str>]) -> (Vec<Option<String>>, Vec<String>) {
        names(&fold_case(table(text), usize::MAX).unwrap())
    }

    fn some(v: &[&str]) -> Vec<Option<String>> {
        v.iter().map(|s| Some((*s).to_string())).collect()
    }

    #[test]
    fn differing_only_by_case_is_one_value_spelled_as_the_first_row() {
        let (rows, values) =
            fold_names(&[Some("abc"), Some("ABC"), Some("x"), Some("Abc"), Some("X")]);
        assert_eq!(rows, some(&["abc", "abc", "x", "abc", "x"]));
        assert_eq!(values, ["abc", "x"]);
        let (rows, _) = fold_names(&[Some("North"), Some("NORTH"), Some("north")]);
        assert_eq!(rows, some(&["North", "North", "North"]));
    }

    #[test]
    fn no_collision_returns_the_same_buffers() {
        let t = table(&[Some("abc"), Some("abd"), None, Some("é")]);
        let before = Arc::clone(t.batch().column(0));
        let folded = fold_case(t, usize::MAX).unwrap();
        assert!(Arc::ptr_eq(&before, folded.batch().column(0)));
        assert!(!folded.folded());
    }

    #[test]
    fn a_fold_marks_the_table_and_keeps_other_columns_shared() {
        let t = table(&[Some("a"), Some("A")]);
        let n = Arc::clone(t.batch().column(1));
        let folded = fold_case(t, usize::MAX).unwrap();
        assert!(folded.folded());
        assert!(Arc::ptr_eq(&n, folded.batch().column(1)));
        assert_eq!(
            folded.batch().schema().field(0).data_type(),
            &DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
            "the narrow key type is kept"
        );
    }

    #[test]
    fn nulls_stay_null() {
        let (rows, values) = fold_names(&[None, Some("B"), None, Some("b")]);
        assert_eq!(rows, [None, Some("B".into()), None, Some("B".into())]);
        assert_eq!(values, ["B"]);
    }

    #[test]
    fn unicode_lowercase_without_accent_folding() {
        let (rows, _) = fold_names(&[Some("É"), Some("é"), Some("e"), Some("E")]);
        assert_eq!(rows, some(&["É", "É", "e", "e"]), "accents still differ");
        // `to_lowercase` never expands ß, so "SS" stays apart; capital ẞ lowers to ß.
        let (rows, _) = fold_names(&[Some("ß"), Some("SS"), Some("ẞ"), Some("ss")]);
        assert_eq!(rows, some(&["ß", "SS", "ß", "SS"]));
        let (rows, _) = fold_names(&[Some("ΟΔΟΣ"), Some("οδος"), Some("Straße"), Some("STRAßE")]);
        assert_eq!(rows, some(&["ΟΔΟΣ", "ΟΔΟΣ", "Straße", "Straße"]));
    }

    #[test]
    fn wider_keys_fold_too() {
        let many: Vec<String> = (0..300).map(|i| format!("v{i}")).collect();
        let mut text: Vec<Option<&str>> = many.iter().map(|s| Some(s.as_str())).collect();
        text.push(Some("V7"));
        let folded = fold_case(table(&text), usize::MAX).unwrap();
        let c = folded.batch().column(0);
        let d = c.as_dictionary::<Int16Type>();
        assert_eq!(d.values().len(), 300);
        let last = d.keys().value(300) as usize;
        assert_eq!(d.values().as_string::<i32>().value(last), "v7");
    }

    #[test]
    fn a_fold_over_the_limit_is_the_memory_cap_error() {
        let e = fold_case(table(&[Some("a"), Some("A")]), 1).unwrap_err();
        assert!(matches!(e, Error::MemoryCap { .. }), "{e:?}");
        // No collision: nothing new is held, so no limit applies.
        assert!(fold_case(table(&[Some("a"), Some("b")]), 0).is_ok());
    }

    #[test]
    fn a_hostile_dictionary_is_an_error_or_unchanged_never_a_panic() {
        // Unreferenced and null dictionary values, a key into a null value.
        let values: ArrayRef = Arc::new(StringArray::from(vec![Some("A"), None, Some("a")]));
        let keys = PrimitiveArray::<Int8Type>::from(vec![Some(2), Some(1), None, Some(0)]);
        let dict: ArrayRef = Arc::new(DictionaryArray::try_new(keys, values).unwrap());
        let batch = RecordBatch::try_from_iter([("Name", dict)]).unwrap();
        let mut b = TableBuilder::new("T", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        let folded = fold_case(b.finish().unwrap(), usize::MAX).unwrap();
        let c = folded.batch().column(0);
        let d = c.as_dictionary::<Int8Type>();
        let got: Vec<Option<&str>> = d
            .downcast_dict::<StringArray>()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(got, [Some("a"), None, None, Some("a")]);
    }
}
