//! Merged Queries: `Table.NestedJoin` + `Table.ExpandTableColumn` as one hash join.
//!
//! Semantics chosen (parity uncertainties for M1.10 cases in the VM):
//! - Null keys match each other (M's `null = null` is true). The public docs don't say; this is
//!   the commonly reported Power Query behaviour, unlike SQL.
//! - Keys compare with M's `=`: whole numbers equal decimals (`1 = 1.0`), different types never
//!   match (`1` vs `"1"`), text is case-sensitive.
//! - Row order: this table's rows in order, each repeated once per match in the other table's
//!   order; Left/Full outer keep an unmatched row once with nulls. Full outer then appends the
//!   other table's unmatched rows in order, with this table's columns null.
//! - The new column must not already exist (Power Query's dialog would pick another name), and
//!   an expanded name that collides is an error, as `Table.ExpandTableColumn` raises one.
//! - The other table's key columns are not added unless expanded, as in Power Query.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Arc;

use arrow_array::{Array, ArrayRef, RecordBatch, RecordBatchOptions, UInt32Array};
use arrow_schema::{Field, Schema};
use arrow_select::take::take;

use crate::cell::Cells;
use crate::step::{JoinKind, Merge};
use crate::{Error, Result};

const STEP: &str = "Merged Queries";

fn err(message: String) -> Error {
    Error::Step {
        step: STEP,
        message,
    }
}

/// Joins `right` (the other Query's result) into `left` for the Query `table`. The output may use
/// at most `limit` bytes; a larger result (e.g. duplicate keys multiplying rows) is
/// [`Error::MemoryCap`] naming `table`, found by counting before anything is allocated.
// ponytail: the hash table is always built on the other table. ADR 0005's "holds only its smaller
// side" would build on whichever is smaller; worth it once merges show up in memory benches.
pub(crate) fn join(
    left: &RecordBatch,
    right: &RecordBatch,
    m: &Merge,
    table: &str,
    limit: usize,
) -> Result<RecordBatch> {
    if m.keys.is_empty() || m.keys.len() != m.other_keys.len() {
        return Err(err(format!(
            "{} key columns can't be matched with {}",
            m.keys.len(),
            m.other_keys.len()
        )));
    }
    let lschema = left.schema();
    let rschema = right.schema();
    let lnames: Vec<&str> = lschema.fields().iter().map(|f| f.name().as_str()).collect();
    let rindex = |name: &str| {
        rschema
            .index_of(name)
            .map_err(|_| err(format!("`{}` has no column `{name}`", m.query)))
    };
    let key_cells = |batch: &RecordBatch, names: &[String], of: &str| {
        names
            .iter()
            .map(|n| {
                let i = batch
                    .schema()
                    .index_of(n)
                    .map_err(|_| err(format!("`{of}` has no column `{n}`")))?;
                Cells::new(batch.column(i))
                    .ok_or_else(|| err(format!("`{n}` can't be a merge key yet")))
            })
            .collect::<Result<Vec<_>>>()
    };
    let lkeys = key_cells(left, &m.keys, table)?;
    let rkeys = key_cells(right, &m.other_keys, &m.query)?;

    // Output columns: this table's, then the expanded ones.
    if m.column.is_empty() || lnames.contains(&m.column.as_str()) {
        return Err(err(format!("the column `{}` already exists", m.column)));
    }
    if m.expand.is_empty() {
        return Err(err("expand at least one column".into()));
    }
    let mut names: Vec<&str> = lnames.clone();
    let mut expanded = Vec::with_capacity(m.expand.len());
    for (from, to) in &m.expand {
        let i = rindex(from)?;
        if to.is_empty() || names.contains(&to.as_str()) {
            return Err(err(format!("the column `{to}` already exists")));
        }
        names.push(to);
        expanded.push((i, to));
    }

    // Hash the other table's keys: row lists per key hash, verified with M's `=` on probe.
    let rrows = right.num_rows();
    let lrows = left.num_rows();
    if u32::try_from(rrows).is_err() || u32::try_from(lrows).is_err() {
        return Err(Error::MemoryCap {
            table: table.into(),
            limit,
        });
    }
    let hash = |cells: &[Cells], row: usize| {
        let mut h = DefaultHasher::new();
        for c in cells {
            c.get(row).hash_into(&mut h);
        }
        h.finish()
    };
    let mut buckets: HashMap<u64, Vec<u32>> = HashMap::new();
    for row in 0..rrows {
        buckets
            .entry(hash(&rkeys, row))
            .or_default()
            .push(u32::try_from(row).unwrap_or(u32::MAX));
    }
    let same = |lrow: usize, rrow: usize| {
        lkeys
            .iter()
            .zip(&rkeys)
            .all(|(l, r)| l.get(lrow).equals(r.get(rrow)))
    };
    let matches = |lrow: usize| {
        buckets
            .get(&hash(&lkeys, lrow))
            .into_iter()
            .flatten()
            .copied()
            .filter(move |&r| same(lrow, r as usize))
    };

    // Bytes per output row: two indices plus this row's share of each side's columns.
    let share = |bytes: usize, rows: usize| bytes.div_ceil(rows.max(1));
    let right_bytes: usize = expanded
        .iter()
        .map(|&(i, _)| right.column(i).get_array_memory_size())
        .fold(0, usize::saturating_add);
    let per_row = 8usize
        .saturating_add(share(left.get_array_memory_size(), lrows))
        .saturating_add(share(right_bytes, rrows));
    let cap = Error::MemoryCap {
        table: table.into(),
        limit,
    };
    let max_rows = (limit / per_row).min(u32::MAX as usize);

    // Count first, stopping as soon as the result can't fit.
    let mut out = 0usize;
    let mut matched = vec![
        false;
        if m.kind == JoinKind::FullOuter {
            rrows
        } else {
            0
        }
    ];
    for lrow in 0..lrows {
        let mut n = 0usize;
        for r in matches(lrow) {
            n = n.saturating_add(1);
            if let Some(seen) = matched.get_mut(r as usize) {
                *seen = true;
            }
            if out.saturating_add(n) > max_rows {
                return Err(cap);
            }
        }
        if n == 0 && m.kind != JoinKind::Inner {
            n = 1;
        }
        out = out.saturating_add(n);
        if out > max_rows {
            return Err(cap);
        }
    }
    out = out.saturating_add(matched.iter().filter(|&&seen| !seen).count());
    if out > max_rows {
        return Err(cap);
    }

    let mut li: Vec<Option<u32>> = Vec::with_capacity(out);
    let mut ri: Vec<Option<u32>> = Vec::with_capacity(out);
    for lrow in 0..lrows {
        let l = u32::try_from(lrow).ok();
        let before = ri.len();
        for r in matches(lrow) {
            li.push(l);
            ri.push(Some(r));
        }
        if ri.len() == before && m.kind != JoinKind::Inner {
            li.push(l);
            ri.push(None);
        }
    }
    for (r, _) in matched.iter().enumerate().filter(|(_, seen)| !**seen) {
        li.push(None);
        ri.push(u32::try_from(r).ok());
    }
    let (li, ri) = (UInt32Array::from(li), UInt32Array::from(ri));

    let pick = |c: &ArrayRef, idx: &UInt32Array| take(c, idx, None).map_err(|e| err(e.to_string()));
    let mut fields: Vec<Field> = lschema
        .fields()
        .iter()
        .map(|f| f.as_ref().clone().with_nullable(true))
        .collect();
    let mut columns = left
        .columns()
        .iter()
        .map(|c| pick(c, &li))
        .collect::<Result<Vec<_>>>()?;
    for (i, to) in expanded {
        let column = pick(right.column(i), &ri)?;
        fields.push(Field::new(to, column.data_type().clone(), true));
        columns.push(column);
    }
    let options = RecordBatchOptions::new().with_row_count(Some(li.len()));
    RecordBatch::try_new_with_options(Arc::new(Schema::new(fields)), columns, &options)
        .map_err(|e| err(e.to_string()))
}

#[cfg(test)]
mod tests {
    use arrow_array::{Float64Array, Int8Array, Int64Array, StringArray};

    use super::*;

    fn ints(v: &[Option<i64>]) -> ArrayRef {
        Arc::new(Int64Array::from(v.to_vec()))
    }

    fn texts(v: &[Option<&str>]) -> ArrayRef {
        Arc::new(StringArray::from(v.to_vec()))
    }

    fn batch(cols: Vec<(&str, ArrayRef)>) -> RecordBatch {
        RecordBatch::try_from_iter(cols).unwrap()
    }

    fn merge(kind: JoinKind, expand: &[&str]) -> Merge {
        Merge {
            keys: vec!["Id".into()],
            query: "Other".into(),
            other_keys: vec!["Id".into()],
            kind,
            column: "Other".into(),
            expand: expand
                .iter()
                .map(|c| (c.to_string(), format!("Other.{c}")))
                .collect(),
        }
    }

    /// Each column of `b` as text cells, for comparing against hand-computed tables.
    fn rows(b: &RecordBatch) -> Vec<Vec<String>> {
        let cols: Vec<Cells> = b.columns().iter().map(|c| Cells::new(c).unwrap()).collect();
        (0..b.num_rows())
            .map(|r| {
                cols.iter()
                    .map(|c| match c.get(r) {
                        crate::cell::Cell::Null => "null".into(),
                        crate::cell::Cell::Int(i) => i.to_string(),
                        crate::cell::Cell::Number(f) => f.to_string(),
                        crate::cell::Cell::Text(s) => s.into(),
                        other => format!("{other:?}"),
                    })
                    .collect()
            })
            .collect()
    }

    fn table(want: &[&[&str]]) -> Vec<Vec<String>> {
        want.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    /// Duplicate keys on both sides, a null key on both sides, unmatched rows on both sides.
    fn sides() -> (RecordBatch, RecordBatch) {
        let left = batch(vec![
            ("Id", ints(&[Some(1), Some(2), None, Some(1), Some(9)])),
            (
                "L",
                texts(&[Some("a"), Some("b"), Some("c"), Some("d"), Some("e")]),
            ),
        ]);
        let right = batch(vec![
            ("Id", ints(&[Some(1), None, Some(1), Some(7)])),
            ("R", texts(&[Some("x"), Some("n"), Some("y"), Some("z")])),
        ]);
        (left, right)
    }

    fn run(kind: JoinKind) -> Vec<Vec<String>> {
        let (l, r) = sides();
        let out = join(&l, &r, &merge(kind, &["R"]), "T", usize::MAX).unwrap();
        let names: Vec<_> = out
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, ["Id", "L", "Other.R"]);
        rows(&out)
    }

    #[test]
    fn left_outer_multiplies_duplicates_and_matches_null_keys() {
        assert_eq!(
            run(JoinKind::LeftOuter),
            table(&[
                &["1", "a", "x"],
                &["1", "a", "y"],
                &["2", "b", "null"],
                &["null", "c", "n"],
                &["1", "d", "x"],
                &["1", "d", "y"],
                &["9", "e", "null"],
            ])
        );
    }

    #[test]
    fn inner_keeps_only_matches() {
        assert_eq!(
            run(JoinKind::Inner),
            table(&[
                &["1", "a", "x"],
                &["1", "a", "y"],
                &["null", "c", "n"],
                &["1", "d", "x"],
                &["1", "d", "y"],
            ])
        );
    }

    #[test]
    fn full_outer_appends_unmatched_other_rows() {
        assert_eq!(
            run(JoinKind::FullOuter),
            table(&[
                &["1", "a", "x"],
                &["1", "a", "y"],
                &["2", "b", "null"],
                &["null", "c", "n"],
                &["1", "d", "x"],
                &["1", "d", "y"],
                &["9", "e", "null"],
                &["null", "null", "z"],
            ])
        );
    }

    #[test]
    fn multi_key_and_mixed_widths() {
        // Narrow ints on one side, decimals on the other: `1 = 1.0` in M.
        let left = batch(vec![
            ("A", Arc::new(Int8Array::from(vec![1, 1, 2])) as ArrayRef),
            ("B", texts(&[Some("p"), Some("q"), Some("p")])),
        ]);
        let right = batch(vec![
            (
                "X",
                Arc::new(Float64Array::from(vec![1.0, 2.0, 1.5])) as ArrayRef,
            ),
            ("Y", texts(&[Some("q"), Some("p"), Some("p")])),
            ("V", ints(&[Some(10), Some(20), Some(30)])),
        ]);
        let m = Merge {
            keys: vec!["A".into(), "B".into()],
            other_keys: vec!["X".into(), "Y".into()],
            expand: vec![("V".into(), "V".into()), ("Y".into(), "Other.Y".into())],
            ..merge(JoinKind::LeftOuter, &[])
        };
        let out = join(&left, &right, &m, "T", usize::MAX).unwrap();
        assert_eq!(
            rows(&out),
            table(&[
                &["1", "p", "null", "null"],
                &["1", "q", "10", "q"],
                &["2", "p", "20", "p"],
            ])
        );
    }

    #[test]
    fn text_and_number_keys_never_match() {
        let left = batch(vec![("Id", ints(&[Some(1)]))]);
        let right = batch(vec![("Id", texts(&[Some("1")])), ("R", ints(&[Some(5)]))]);
        let out = join(
            &left,
            &right,
            &merge(JoinKind::Inner, &["R"]),
            "T",
            usize::MAX,
        )
        .unwrap();
        assert_eq!(out.num_rows(), 0);
    }

    #[test]
    fn collisions_and_bad_specs_are_errors() {
        let (l, r) = sides();
        let cases: Vec<(Merge, &str)> = vec![
            (
                Merge {
                    expand: vec![("R".into(), "L".into())],
                    ..merge(JoinKind::Inner, &[])
                },
                "`L` already exists",
            ),
            (
                Merge {
                    column: "L".into(),
                    ..merge(JoinKind::Inner, &["R"])
                },
                "`L` already exists",
            ),
            (merge(JoinKind::Inner, &[]), "expand at least one"),
            (merge(JoinKind::Inner, &["Nope"]), "no column `Nope`"),
            (
                Merge {
                    other_keys: vec![],
                    ..merge(JoinKind::Inner, &["R"])
                },
                "can't be matched",
            ),
            (
                Merge {
                    keys: vec!["Nope".into()],
                    ..merge(JoinKind::Inner, &["R"])
                },
                "`T` has no column `Nope`",
            ),
        ];
        for (m, want) in cases {
            let e = join(&l, &r, &m, "T", usize::MAX).unwrap_err();
            assert!(e.to_string().contains(want), "{e}");
            assert!(e.to_string().starts_with("Merged Queries: "), "{e}");
        }
    }

    #[test]
    fn a_cross_product_past_the_cap_is_an_error_not_an_allocation() {
        // 100k × 100k rows on one key = 10^10 rows: refused while counting.
        let n = 100_000;
        let side = |name: &str| {
            batch(vec![
                ("Id", ints(&vec![Some(1); n])),
                (name, ints(&vec![Some(2); n])),
            ])
        };
        let e = join(
            &side("L"),
            &side("R"),
            &merge(JoinKind::Inner, &["R"]),
            "Sales",
            64 << 20,
        )
        .unwrap_err();
        assert_eq!(
            e,
            Error::MemoryCap {
                table: "Sales".into(),
                limit: 64 << 20
            }
        );
        // Under the cap it runs.
        let small = |name: &str| {
            batch(vec![
                ("Id", ints(&[Some(1); 3])),
                (name, ints(&[Some(2); 3])),
            ])
        };
        let out = join(
            &small("L"),
            &small("R"),
            &merge(JoinKind::Inner, &["R"]),
            "Sales",
            64 << 20,
        )
        .unwrap();
        assert_eq!(out.num_rows(), 9);
    }

    #[test]
    fn empty_sides() {
        let (l, r) = sides();
        let m = merge(JoinKind::FullOuter, &["R"]);
        assert_eq!(
            join(&l.slice(0, 0), &r, &m, "T", usize::MAX)
                .unwrap()
                .num_rows(),
            4
        );
        assert_eq!(
            join(&l, &r.slice(0, 0), &m, "T", usize::MAX)
                .unwrap()
                .num_rows(),
            5
        );
    }
}
