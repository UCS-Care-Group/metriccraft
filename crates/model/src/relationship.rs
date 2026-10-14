//! Relationships (CONTEXT.md, spec D-2): many-to-one, single direction (filters flow from the
//! one side to the many side), at most one **active** per pair of Tables.
//!
//! Rules, from Power BI's documentation where it has them:
//! - The two columns are in different Tables (Power BI can't relate a table to itself).
//! - The one side (`to_column`) holds unique, non-blank values in the loaded data, compared as
//!   the engine does: text ignoring case and trailing spaces. A load whose new data breaks this
//!   fails, naming the Table and Column (Analysis Services' message).
//! - **Data types are not checked**: Power BI lets a text column relate to a whole-number one
//!   ("filters won't find any matches"), so any pair of types is accepted. Auto-detect only
//!   pairs columns of the same type.
//! - A new (or edited) active Relationship on a pair that already has an active one is made
//!   inactive, as Power BI's dialog does; activating one there fails until the other is
//!   deactivated.
//! - After a load, a Relationship whose Table or Column is gone is dropped (and reported).
//!
//! **Auto-detect** (after Get Data, Close & Apply and Refresh when
//! [`Model::autodetect_relationships`] is on, or on demand): for each pair of Tables with no
//! Relationship yet (and, after a load, at least one of them new), every column name the two
//! share, with the same data type (not True/False), whose values are keys (unique, non-blank,
//! at least one row) on exactly one side, gives a Relationship from the other side. Unique on
//! both sides would be one-to-one, out of scope: skipped. Several on one pair: the first (in
//! the first Table's column order) is active, the rest inactive, as Power BI does.
//!
//! TODO(M3): Power BI also refuses an active Relationship that makes the filter path between
//! two Tables ambiguous (two active paths); RELATED and filter propagation need that rule.
//! Only the per-pair rule is enforced now, so auto-detect can still create such a path.
//!
//! Parity uncertainties, to observe in the VM:
//! - whether a blank on the one side is allowed (we refuse it, with the blank-values message);
//! - case folding beyond ASCII and whether other trailing whitespace counts;
//! - the exact wording of the active-conflict message;
//! - whether a refresh drops a Relationship whose column is gone silently or with a warning;
//! - whether auto-detect matches column names ignoring case (we match them exactly).

use std::collections::HashSet;
use std::fmt;

use arrow_array::Array;
use opendash_data::Table;
use opendash_data::step::ColumnType;

use crate::table::Result;
use crate::table::as_text;
use crate::{Error, Model};

/// A many-to-one link from `from_table[from_column]` (many side) to `to_table[to_column]`
/// (one side, unique values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relationship {
    pub from_table: String,
    pub from_column: String,
    pub to_table: String,
    pub to_column: String,
    /// Inactive Relationships filter only when a DAX expression activates them.
    pub active: bool,
}

impl Relationship {
    /// An active Relationship.
    pub fn new(from_table: &str, from_column: &str, to_table: &str, to_column: &str) -> Self {
        Self {
            from_table: from_table.into(),
            from_column: from_column.into(),
            to_table: to_table.into(),
            to_column: to_column.into(),
            active: true,
        }
    }

    /// Whether `other` links the same two Tables, either way round.
    pub fn same_pair(&self, other: &Relationship) -> bool {
        let (a, b) = (&self.from_table, &self.to_table);
        (*a == other.from_table && *b == other.to_table)
            || (*a == other.to_table && *b == other.from_table)
    }

    /// Whether `other` links the same two columns, either way round.
    pub fn same_columns(&self, other: &Relationship) -> bool {
        let ends = |r: &Relationship| {
            let mut e = [
                (r.from_table.clone(), r.from_column.clone()),
                (r.to_table.clone(), r.to_column.clone()),
            ];
            e.sort();
            e
        };
        ends(self) == ends(other)
    }

    /// Whether it uses Table `table`.
    pub fn uses(&self, table: &str) -> bool {
        self.from_table == table || self.to_table == table
    }

    fn error(&self, message: String) -> Error {
        Error {
            table: self.from_table.clone(),
            column: self.from_column.clone(),
            message,
        }
    }
}

impl fmt::Display for Relationship {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{}] -> {}[{}]",
            self.from_table, self.from_column, self.to_table, self.to_column
        )
    }
}

impl Model {
    /// Adds `r` after checking it against the model and `tables` (the loaded data). An active
    /// `r` on a pair that already has an active Relationship is added inactive; the returned
    /// notice says so.
    pub fn add_relationship(
        &mut self,
        mut r: Relationship,
        tables: &[Table],
    ) -> Result<Option<String>> {
        self.check_relationship(&r, tables, None)?;
        let notice = self.demote(&mut r, None);
        self.relationships.push(r);
        Ok(notice)
    }

    /// Replaces Relationship `index` with `r`: the same checks as adding, itself excluded.
    pub fn edit_relationship(
        &mut self,
        index: usize,
        mut r: Relationship,
        tables: &[Table],
    ) -> Result<Option<String>> {
        self.check_relationship(&r, tables, Some(index))?;
        let notice = self.demote(&mut r, Some(index));
        let slot = self
            .relationships
            .get_mut(index)
            .ok_or_else(|| r.error(format!("there is no relationship {index}")))?;
        *slot = r;
        Ok(notice)
    }

    /// Removes and returns Relationship `index`, if there is one.
    pub fn remove_relationship(&mut self, index: usize) -> Option<Relationship> {
        (index < self.relationships.len()).then(|| self.relationships.remove(index))
    }

    /// Makes Relationship `index` active or inactive. Activating it fails while its pair has
    /// another active one: as in Power BI, deactivate that one first.
    pub fn set_relationship_active(&mut self, index: usize, active: bool) -> Result {
        let r = self
            .relationships
            .get(index)
            .cloned()
            .ok_or_else(|| Error {
                table: String::new(),
                column: String::new(),
                message: format!("there is no relationship {index}"),
            })?;
        if active && self.active_on_pair(&r, Some(index)) {
            return Err(r.error(conflict(&r)));
        }
        if let Some(slot) = self.relationships.get_mut(index) {
            slot.active = active;
        }
        Ok(())
    }

    /// After a load: drops (and returns) the Relationships whose Table or Column is gone, then
    /// checks the one side of every Relationship into a Table in `loaded`. A failure fails the
    /// load (the caller changes nothing).
    pub fn relationships_after_load(
        &mut self,
        tables: &[Table],
        loaded: &[String],
    ) -> Result<Vec<Relationship>> {
        let (keep, gone): (Vec<_>, Vec<_>) = std::mem::take(&mut self.relationships)
            .into_iter()
            .partition(|r| {
                self.has(&r.from_table, &r.from_column) && self.has(&r.to_table, &r.to_column)
            });
        self.relationships = keep;
        for name in loaded {
            self.check_keys(tables, name)?;
        }
        Ok(gone)
    }

    /// Checks the one side of every Relationship into Table `table` (e.g. after a load or a
    /// type change of its data).
    pub fn check_keys(&self, tables: &[Table], table: &str) -> Result {
        for r in self.relationships.iter().filter(|r| r.to_table == table) {
            let data = find(tables, table).ok_or_else(|| r.error(no_table(table)))?;
            check_key(data, &r.to_column)?;
        }
        Ok(())
    }

    /// Auto-detect (see the module doc) over the pairs with at least one Table `new` picks.
    /// Adds and returns the Relationships found.
    pub fn autodetect(
        &mut self,
        tables: &[Table],
        new: impl Fn(&str) -> bool,
    ) -> Vec<Relationship> {
        let mut added = Vec::new();
        let names: Vec<String> = self.tables.iter().map(|t| t.name.clone()).collect();
        for (i, a) in names.iter().enumerate() {
            for b in names.iter().skip(i.saturating_add(1)) {
                if !(new(a) || new(b)) {
                    continue;
                }
                let probe = Relationship::new(a, "", b, "");
                if self.relationships.iter().any(|r| r.same_pair(&probe)) {
                    continue;
                }
                let found = self.candidates(tables, a, b);
                for (k, mut r) in found.into_iter().enumerate() {
                    r.active = k == 0;
                    self.relationships.push(r.clone());
                    added.push(r);
                }
            }
        }
        added
    }

    /// Auto-detect's Relationships between Tables `a` and `b`.
    fn candidates(&self, tables: &[Table], a: &str, b: &str) -> Vec<Relationship> {
        let (Some(ma), Some(mb), Some(da), Some(db)) = (
            self.table(a),
            self.table(b),
            find(tables, a),
            find(tables, b),
        ) else {
            return Vec::new();
        };
        let key = |t: &Table, c: &str| t.rows() > 0 && check_key(t, c).is_ok();
        let mut found = Vec::new();
        for ca in &ma.columns {
            let Some(cb) = mb.column(&ca.name) else {
                continue;
            };
            if ca.data_type != cb.data_type || ca.data_type == ColumnType::Boolean {
                continue;
            }
            match (key(da, &ca.name), key(db, &cb.name)) {
                (false, true) => found.push(Relationship::new(a, &ca.name, b, &cb.name)),
                (true, false) => found.push(Relationship::new(b, &cb.name, a, &ca.name)),
                _ => {}
            }
        }
        found
    }

    /// Checks `r` as a new Relationship (`except`: the one it replaces).
    fn check_relationship(
        &self,
        r: &Relationship,
        tables: &[Table],
        except: Option<usize>,
    ) -> Result {
        if r.from_table == r.to_table {
            return Err(r.error("a relationship can't link a table to itself".into()));
        }
        for (table, column) in [(&r.from_table, &r.from_column), (&r.to_table, &r.to_column)] {
            let t = self.table(table).ok_or_else(|| r.error(no_table(table)))?;
            t.column(column).ok_or_else(|| Error {
                table: table.clone(),
                column: column.clone(),
                message: "there is no such column".into(),
            })?;
        }
        let others = self
            .relationships
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != except);
        if let Some((_, other)) = others.clone().find(|(_, o)| o.same_columns(r)) {
            return Err(r.error(format!("there is already a relationship {other}")));
        }
        let data = find(tables, &r.to_table).ok_or_else(|| r.error(no_table(&r.to_table)))?;
        check_key(data, &r.to_column)
    }

    /// Makes an active `r` inactive if its pair already has an active Relationship.
    fn demote(&self, r: &mut Relationship, except: Option<usize>) -> Option<String> {
        if !r.active || !self.active_on_pair(r, except) {
            return None;
        }
        r.active = false;
        Some(format!(
            "{r} is inactive: there is already an active relationship between `{}` and `{}`",
            r.from_table, r.to_table
        ))
    }

    fn active_on_pair(&self, r: &Relationship, except: Option<usize>) -> bool {
        self.relationships
            .iter()
            .enumerate()
            .any(|(i, o)| Some(i) != except && o.active && o.same_pair(r))
    }

    fn has(&self, table: &str, column: &str) -> bool {
        self.table(table)
            .is_some_and(|t| t.column(column).is_some())
    }
}

/// Power BI's refusal to activate a second Relationship on a pair (as we recall it).
fn conflict(r: &Relationship) -> String {
    let (a, b) = (&r.from_table, &r.to_table);
    format!(
        "You can't create a direct active relationship between {a} and {b} because an active \
         set of relationships already exists between these tables. To make this relationship \
         active, deactivate or delete one of the relationships between {a} and {b} first."
    )
}

fn no_table(table: &str) -> String {
    format!("there is no table `{table}`")
}

fn find<'a>(tables: &'a [Table], name: &str) -> Option<&'a Table> {
    tables.iter().find(|t| t.name() == name)
}

/// Checks `column` of `table` holds unique, non-blank values (text: ignoring case and
/// trailing spaces), with Analysis Services' messages.
// ponytail: compares through a transient text copy, like the sort-by check; hash typed values
// if it shows up in load benches.
fn check_key(table: &Table, column: &str) -> Result {
    let err = |message: String| Error {
        table: table.name().into(),
        column: column.into(),
        message,
    };
    let values = table
        .batch()
        .column_by_name(column)
        .ok_or_else(|| err("there is no such column".into()))?;
    let is_text = ColumnType::of_data_type(values.data_type()) == Some(ColumnType::Text);
    let text = as_text(values).ok_or_else(|| err("can't be compared".into()))?;
    let rule = "and this is not allowed for columns on the one side of a many-to-one \
                relationship or for columns that are used as the primary key of a table.";
    let name = table.name();
    let mut seen = HashSet::with_capacity(text.len());
    for v in text.iter() {
        let Some(v) = v else {
            return Err(err(format!(
                "Column '{column}' in Table '{name}' contains blank values {rule}"
            )));
        };
        let key = if is_text {
            opendash_core::fold(v.trim_end_matches(' '))
        } else {
            v.to_string()
        };
        if !seen.insert(key) {
            return Err(err(format!(
                "Column '{column}' in Table '{name}' contains a duplicate value '{v}' {rule}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
    use opendash_data::TableBuilder;
    use opendash_data::culture::Culture;

    use super::*;
    use crate::ModelTable;

    fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
        let batch = RecordBatch::try_from_iter(columns).unwrap();
        let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        b.finish().unwrap()
    }

    fn text(v: &[Option<&str>]) -> ArrayRef {
        Arc::new(StringArray::from(v.to_vec()))
    }

    fn int(v: &[i64]) -> ArrayRef {
        Arc::new(Int64Array::from(v.to_vec()))
    }

    /// A model of `tables`, with no relationships and auto-detect not run.
    fn model(tables: &[Table]) -> Model {
        let mut m = Model::default();
        for t in tables {
            let us = Culture::get("en-US").unwrap();
            let (mt, _) = ModelTable::load(None, t.clone(), us, usize::MAX).unwrap();
            m.put(mt);
        }
        m
    }

    fn star() -> Vec<Table> {
        vec![
            table(
                "Sales",
                vec![
                    (
                        "Region",
                        text(&[Some("North"), Some("North"), Some("South")]),
                    ),
                    ("Code", int(&[1, 1, 2])),
                    ("Qty", int(&[3, 4, 5])),
                ],
            ),
            table(
                "Regions",
                vec![
                    ("Region", text(&[Some("North"), Some("South")])),
                    ("Code", int(&[1, 2])),
                ],
            ),
        ]
    }

    #[test]
    fn keys_ignore_case_and_trailing_spaces_and_refuse_blanks() {
        let ok = table("T", vec![("K", text(&[Some("a"), Some(" a"), Some("b")]))]);
        assert!(check_key(&ok, "K").is_ok(), "leading spaces count");
        for (values, word) in [
            (vec![Some("a"), Some("A")], "duplicate value 'A'"),
            (vec![Some("a"), Some("a  ")], "duplicate value 'a  '"),
            (vec![Some("a"), None], "contains blank values"),
        ] {
            let t = table("T", vec![("K", text(&values))]);
            let e = check_key(&t, "K").unwrap_err();
            assert!(e.message.contains(word), "{e}");
            assert!(e.message.starts_with("Column 'K' in Table 'T'"), "{e}");
        }
    }

    #[test]
    fn a_second_active_relationship_on_a_pair_is_added_inactive() {
        let t = star();
        let mut m = model(&t);
        let n = m
            .add_relationship(
                Relationship::new("Sales", "Region", "Regions", "Region"),
                &t,
            )
            .unwrap();
        assert_eq!(n, None);
        let n = m
            .add_relationship(Relationship::new("Sales", "Code", "Regions", "Code"), &t)
            .unwrap();
        assert!(n.is_some_and(|n| n.contains("inactive")));
        assert!(!m.relationships[1].active);
        let e = m.set_relationship_active(1, true).unwrap_err();
        assert!(
            e.message.starts_with("You can't create a direct active"),
            "{e}"
        );
        m.set_relationship_active(0, false).unwrap();
        m.set_relationship_active(1, true).unwrap();
        assert!(!m.relationships[0].active && m.relationships[1].active);
    }

    #[test]
    fn bad_relationships_are_refused() {
        let t = star();
        let mut m = model(&t);
        let before = m.clone();
        for r in [
            Relationship::new("Sales", "Region", "Sales", "Code"),
            Relationship::new("Sales", "Nope", "Regions", "Region"),
            Relationship::new("Nope", "Region", "Regions", "Region"),
            // The one side has duplicates.
            Relationship::new("Regions", "Region", "Sales", "Region"),
        ] {
            assert!(m.add_relationship(r.clone(), &t).is_err(), "{r}");
            assert_eq!(m, before);
        }
        m.add_relationship(
            Relationship::new("Sales", "Region", "Regions", "Region"),
            &t,
        )
        .unwrap();
        let again = Relationship::new("Sales", "Region", "Regions", "Region");
        assert!(m.add_relationship(again, &t).is_err(), "same columns twice");
        // Different types are allowed, as in Power BI.
        m.add_relationship(Relationship::new("Sales", "Qty", "Regions", "Region"), &t)
            .unwrap();
    }

    #[test]
    fn autodetect_finds_the_obvious_keys_and_nothing_ambiguous() {
        let t = star();
        let mut m = model(&t);
        let added = m.autodetect(&t, |_| true);
        let shown: Vec<_> = added.iter().map(|r| (r.to_string(), r.active)).collect();
        assert_eq!(
            shown,
            [
                ("Sales[Region] -> Regions[Region]".to_string(), true),
                ("Sales[Code] -> Regions[Code]".to_string(), false),
            ]
        );
        assert!(
            m.autodetect(&t, |_| true).is_empty(),
            "the pair has relationships"
        );
        // Unique on both sides (one-to-one) or neither: nothing. Different types: nothing.
        let both = vec![
            table(
                "A",
                vec![
                    ("K", int(&[1, 2])),
                    ("N", int(&[1, 1])),
                    ("T", int(&[1, 2])),
                ],
            ),
            table(
                "B",
                vec![
                    ("K", int(&[2, 1])),
                    ("N", int(&[5, 5])),
                    ("T", text(&[Some("1"), Some("2")])),
                ],
            ),
        ];
        let mut m = model(&both);
        assert!(m.autodetect(&both, |_| true).is_empty());
        // Only pairs with a new table are looked at.
        let t = star();
        let mut m = model(&t);
        assert!(m.autodetect(&t, |_| false).is_empty());
        m.autodetect_relationships = false;
        assert_eq!(
            m.autodetect(&t, |n| n == "Regions").len(),
            2,
            "manual run ignores the switch"
        );
    }

    #[test]
    fn a_load_drops_relationships_to_gone_columns_and_checks_keys() {
        let t = star();
        let mut m = model(&t);
        m.autodetect(&t, |_| true);
        let regions = table(
            "Regions",
            vec![("Region", text(&[Some("North"), Some("South")]))],
        );
        let us = Culture::get("en-US").unwrap();
        let (mt, _) =
            ModelTable::load(m.table("Regions"), regions.clone(), us, usize::MAX).unwrap();
        m.put(mt);
        let t2 = vec![t[0].clone(), regions];
        let gone = m
            .relationships_after_load(&t2, &["Regions".into()])
            .unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].to_string(), "Sales[Code] -> Regions[Code]");
        assert_eq!(m.relationships.len(), 1);
        let dup = table(
            "Regions",
            vec![("Region", text(&[Some("North"), Some("north")]))],
        );
        let e = m.check_keys(&[t[0].clone(), dup], "Regions").unwrap_err();
        assert_eq!((e.table.as_str(), e.column.as_str()), ("Regions", "Region"));
    }
}
