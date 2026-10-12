//! Model Tables and Columns (CONTEXT.md, spec D-1): the properties Power BI's Column tools and
//! Properties pane set, kept beside the loaded data (`opendash_data::Table`, matched by name).
//!
//! **After every load** ([`ModelTable::load`]) a Table's columns are matched to the previous
//! ones by name: a column that is still there keeps its properties (and its data-type
//! override, re-applied to the new data); a new column gets the defaults; a column that is gone
//! loses its properties, as in Power BI, and a sort by column pointing at it is cleared.
//!
//! **Data-type override.** A Column's type comes from its Query (`query_type`) unless changed in
//! the Model (`data_type` differs). The change converts the loaded data with the Changed Type
//! kernel in the Query's Culture; the Query is untouched.
//!
//! Parity uncertainties, to settle with M1.10-style cases in the VM:
//! - a type change with a value that doesn't convert fails, keeping the old type, with
//!   `Cannot convert value '<v>' of type <T> to type <U>.` (Analysis Services' wording, as we
//!   recall it; type names Text, Number, Currency, Integer, Date, DateTime, True/False);
//! - the same failure on a later load (an override re-applied to new data) fails the load;
//! - changing the type converts the loaded data, not the Query's output (a decimal overridden to
//!   whole and back keeps the rounded values until the next load);
//! - summarize by defaults to sum for numbers and none otherwise; a type change resets it to the
//!   new type's default when it was the old type's default, or when it was sum/average and the
//!   new type isn't a number;
//! - the sort-by message is our recollection of Power BI's.

use std::collections::HashMap;
use std::fmt;

use arrow_array::{Array, ArrayRef, StringArray};
use arrow_cast::cast;
use arrow_schema::DataType;
use opendash_data::Table;
use opendash_data::culture::Culture;
use opendash_data::step::{ColumnType, change_type};

use crate::Relationship;

/// A model error, naming the Table and Column.
#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    pub table: String,
    /// Empty for an error about the Table itself.
    pub column: String,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.column.is_empty() {
            write!(f, "`{}`: {}", self.table, self.message)
        } else {
            write!(f, "`{}`[`{}`]: {}", self.table, self.column, self.message)
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;

/// The model: one [`ModelTable`] per loaded Table, same name, and the Relationships between
/// them (`relationship.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    pub tables: Vec<ModelTable>,
    /// In Manage relationships order.
    pub relationships: Vec<Relationship>,
    /// Power BI's *Autodetect new relationships after data is loaded* (Options > Current file >
    /// Data Load); on by default.
    pub autodetect_relationships: bool,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            tables: Vec::new(),
            relationships: Vec::new(),
            autodetect_relationships: true,
        }
    }
}

impl Model {
    pub fn table(&self, name: &str) -> Option<&ModelTable> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn table_mut(&mut self, name: &str) -> Option<&mut ModelTable> {
        self.tables.iter_mut().find(|t| t.name == name)
    }

    /// Sets (or adds) `table`'s model.
    pub fn put(&mut self, table: ModelTable) {
        match self.table_mut(&table.name) {
            Some(t) => *t = table,
            None => self.tables.push(table),
        }
    }

    pub fn remove(&mut self, name: &str) {
        self.tables.retain(|t| t.name != name);
    }
}

/// A Table's model properties. `columns` follow the loaded Table's column order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelTable {
    pub name: String,
    pub hidden: bool,
    pub description: String,
    pub columns: Vec<Column>,
}

/// A Column's data type and display properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Column {
    pub name: String,
    /// The type in the Model: `query_type` unless overridden.
    pub data_type: ColumnType,
    /// The type the Query gave at the last load.
    pub query_type: ColumnType,
    pub hidden: bool,
    pub description: String,
    /// A Power BI format string (`"#,0.00"`); empty for the default. Rendered in M3.8.
    pub format_string: String,
    pub summarize_by: SummarizeBy,
    /// Another column of the same Table whose order this one sorts by.
    pub sort_by: Option<String>,
}

impl Column {
    /// A new column with Power BI's defaults.
    pub fn new(name: &str, ty: ColumnType) -> Self {
        Self {
            name: name.into(),
            data_type: ty,
            query_type: ty,
            hidden: false,
            description: String::new(),
            format_string: String::new(),
            summarize_by: SummarizeBy::default_for(ty),
            sort_by: None,
        }
    }

    /// Whether the Model overrides the Query's type.
    pub fn overridden(&self) -> bool {
        self.data_type != self.query_type
    }
}

/// How a visual aggregates a column by default (Power BI's Summarization).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummarizeBy {
    None,
    Sum,
    Average,
    Min,
    Max,
    Count,
    DistinctCount,
}

impl SummarizeBy {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Sum,
        Self::Average,
        Self::Min,
        Self::Max,
        Self::Count,
        Self::DistinctCount,
    ];

    /// The name commands and `.odm` use (TMDL's): `none`, `sum`, `average`, `min`, `max`,
    /// `count`, `distinctCount`.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sum => "sum",
            Self::Average => "average",
            Self::Min => "min",
            Self::Max => "max",
            Self::Count => "count",
            Self::DistinctCount => "distinctCount",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Power BI's default: sum for numbers, none otherwise.
    pub fn default_for(ty: ColumnType) -> Self {
        if ty.is_numeric() {
            Self::Sum
        } else {
            Self::None
        }
    }

    /// Whether a column of type `ty` may be summarized this way (sum and average need numbers).
    pub fn allowed_for(self, ty: ColumnType) -> bool {
        ty.is_numeric() || !matches!(self, Self::Sum | Self::Average)
    }
}

impl ModelTable {
    /// The model for a freshly loaded `table`, carrying over `previous`'s properties (see the
    /// module doc), and the table with data-type overrides applied. Fails (naming the column)
    /// if an override can't convert the new data or a sort by column no longer holds; the
    /// caller's load then changes nothing. `limit`: bytes the converted columns may use.
    pub fn load(
        previous: Option<&ModelTable>,
        mut table: Table,
        culture: &Culture,
        limit: usize,
    ) -> Result<(ModelTable, Table)> {
        let name = table.name().to_string();
        let schema = table.batch().schema();
        let mut columns = Vec::with_capacity(schema.fields().len());
        for (i, field) in schema.fields().iter().enumerate() {
            let ty = ColumnType::of_data_type(field.data_type()).unwrap_or(ColumnType::Text);
            let old = previous.and_then(|p| p.column(field.name()));
            let column = match old {
                None => Column::new(field.name(), ty),
                Some(old) => {
                    let mut c = old.clone();
                    c.query_type = ty;
                    if old.overridden() {
                        if old.data_type != ty {
                            table = convert(&table, i, ty, old.data_type, culture, limit)?;
                        }
                    } else {
                        c.data_type = ty;
                        c.summarize_by = summarize_after(old.summarize_by, old.data_type, ty);
                    }
                    c
                }
            };
            columns.push(column);
        }
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        for c in &mut columns {
            if c.sort_by.as_ref().is_some_and(|by| !names.contains(by)) {
                c.sort_by = None;
            }
        }
        let model = ModelTable {
            name,
            hidden: previous.is_some_and(|p| p.hidden),
            description: previous.map(|p| p.description.clone()).unwrap_or_default(),
            columns,
        };
        for c in &model.columns {
            if let Some(by) = &c.sort_by {
                model.check_sort(&table, &c.name, by)?;
            }
        }
        Ok((model, table))
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// The column `name`, or an error naming it.
    pub fn column_mut(&mut self, name: &str) -> Result<&mut Column> {
        let table = self.name.clone();
        self.columns
            .iter_mut()
            .find(|c| c.name == name)
            .ok_or_else(|| Error {
                table,
                column: name.into(),
                message: "there is no such column".into(),
            })
    }

    /// Changes `column`'s type in the Model (Column tools > Data type): converts its data in
    /// `table` and returns the converted table. A value that doesn't convert fails the change
    /// and nothing changes.
    pub fn set_data_type(
        &mut self,
        table: &Table,
        column: &str,
        ty: ColumnType,
        culture: &Culture,
        limit: usize,
    ) -> Result<Table> {
        let index = table
            .batch()
            .schema()
            .index_of(column)
            .map_err(|_| self.error(column, "there is no such column".into()))?;
        let c = self.column_mut(column)?;
        if c.data_type == ty {
            return Ok(table.clone());
        }
        let from = c.data_type;
        let converted = convert(table, index, from, ty, culture, limit)?;
        c.summarize_by = summarize_after(c.summarize_by, from, ty);
        c.data_type = ty;
        Ok(converted)
    }

    /// Sets (or with `None` clears) `column`'s sort by column: another column of this table
    /// with one value for each value of `column` in `table`'s data.
    pub fn set_sort_by(&mut self, table: &Table, column: &str, by: Option<&str>) -> Result {
        self.column_mut(column)?;
        if let Some(by) = by {
            if by == column {
                return Err(self.error(column, "a column can't be sorted by itself".into()));
            }
            let other = self.column(by).ok_or_else(|| {
                self.error(column, format!("there is no column `{by}` to sort by"))
            })?;
            if other.sort_by.as_deref() == Some(column) {
                return Err(self.error(
                    column,
                    format!("`{by}` is sorted by `{column}`: sorting can't go in a circle"),
                ));
            }
            self.check_sort(table, column, by)?;
        }
        self.column_mut(column)?.sort_by = by.map(str::to_string);
        Ok(())
    }

    /// Checks `by` has one value for each value of `column` in `table`.
    fn check_sort(&self, table: &Table, column: &str, by: &str) -> Result {
        let batch = table.batch();
        let text = |name: &str| -> Result<StringArray> {
            let col = batch
                .column_by_name(name)
                .ok_or_else(|| self.error(name, "there is no such column".into()))?;
            as_text(col).ok_or_else(|| self.error(name, "can't be compared".into()))
        };
        let (values, keys) = (text(column)?, text(by)?);
        let mut seen: HashMap<Option<&str>, Option<&str>> = HashMap::new();
        for (v, k) in values.iter().zip(keys.iter()) {
            if *seen.entry(v).or_insert(k) != k {
                return Err(self.error(
                    column,
                    format!(
                        "Cannot sort '{column}' by '{by}'. There can't be more than one value \
                         in '{by}' for the same value in '{column}'. Please choose a different \
                         column for sorting or update the data in '{by}'."
                    ),
                ));
            }
        }
        Ok(())
    }

    fn error(&self, column: &str, message: String) -> Error {
        Error {
            table: self.name.clone(),
            column: column.into(),
            message,
        }
    }
}

/// The summarization after a type change from `from` to `to` (see the module doc).
fn summarize_after(s: SummarizeBy, from: ColumnType, to: ColumnType) -> SummarizeBy {
    if s == SummarizeBy::default_for(from) || !s.allowed_for(to) {
        SummarizeBy::default_for(to)
    } else {
        s
    }
}

/// `table` with column `index` (of type `from`) converted to `to`; an error at the first value
/// that doesn't convert.
fn convert(
    table: &Table,
    index: usize,
    from: ColumnType,
    to: ColumnType,
    culture: &Culture,
    limit: usize,
) -> Result<Table> {
    let schema = table.batch().schema();
    let name = schema
        .fields()
        .get(index)
        .map(|f| f.name().clone())
        .unwrap_or_default();
    let err = |message: String| Error {
        table: table.name().into(),
        column: name.clone(),
        message,
    };
    let old = table
        .batch()
        .columns()
        .get(index)
        .ok_or_else(|| err("there is no such column".into()))?;
    let (new, errors) = change_type(old, to, culture).map_err(err)?;
    if errors > 0 {
        let bad = (0..old.len()).find(|&i| old.is_valid(i) && new.is_null(i));
        let value = bad
            .and_then(|i| as_text(&old.slice(i, 1)))
            .and_then(|t| t.iter().next().flatten().map(str::to_string))
            .unwrap_or_default();
        return Err(err(format!(
            "Cannot convert value '{value}' of type {} to type {}.",
            type_name(from),
            type_name(to)
        )));
    }
    table
        .with_column(index, new, limit)
        .map_err(|e| err(e.to_string()))
}

/// The type's name in Analysis Services' conversion errors.
fn type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Text => "Text",
        ColumnType::Decimal => "Number",
        ColumnType::FixedDecimal => "Currency",
        ColumnType::Whole => "Integer",
        ColumnType::Date => "Date",
        ColumnType::DateTime => "DateTime",
        ColumnType::Boolean => "True/False",
    }
}

/// `column` as text, any type.
// ponytail: a transient text copy of both columns for the sort-by check; compare typed values
// if it shows up in load benches.
pub(crate) fn as_text(column: &ArrayRef) -> Option<StringArray> {
    let text = cast(column, &DataType::Utf8).ok()?;
    text.as_any().downcast_ref::<StringArray>().cloned()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Decimal32Array, Float64Array, Int64Array, RecordBatch};
    use opendash_data::TableBuilder;

    use super::*;

    fn us() -> &'static Culture {
        Culture::get("en-US").unwrap()
    }

    fn table(columns: Vec<(&str, ArrayRef)>) -> Table {
        let batch = RecordBatch::try_from_iter(columns).unwrap();
        let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        b.finish().unwrap()
    }

    fn text(v: &[&str]) -> ArrayRef {
        Arc::new(StringArray::from(v.to_vec()))
    }

    fn sales() -> Table {
        table(vec![
            ("Month", text(&["Jan", "Feb", "Jan"])),
            ("MonthNo", Arc::new(Int64Array::from(vec![1, 2, 1]))),
            ("Price", Arc::new(Float64Array::from(vec![0.1, 0.2, 1.5]))),
        ])
    }

    fn load(previous: Option<&ModelTable>, t: Table) -> Result<(ModelTable, Table)> {
        ModelTable::load(previous, t, us(), usize::MAX)
    }

    #[test]
    fn new_columns_get_power_bis_defaults() {
        let (m, _) = load(None, sales()).unwrap();
        let types: Vec<_> = m
            .columns
            .iter()
            .map(|c| (c.name.as_str(), c.data_type, c.summarize_by))
            .collect();
        assert_eq!(
            types,
            [
                ("Month", ColumnType::Text, SummarizeBy::None),
                ("MonthNo", ColumnType::Whole, SummarizeBy::Sum),
                ("Price", ColumnType::Decimal, SummarizeBy::Sum),
            ]
        );
        assert!(m.columns.iter().all(|c| !c.overridden() && !c.hidden));
    }

    #[test]
    fn properties_survive_a_load_and_go_with_their_column() {
        let (mut m, t) = load(None, sales()).unwrap();
        m.hidden = true;
        m.description = "Orders".into();
        let c = m.column_mut("MonthNo").unwrap();
        c.hidden = true;
        c.format_string = "0".into();
        c.summarize_by = SummarizeBy::None;
        m.set_sort_by(&t, "Month", Some("MonthNo")).unwrap();
        let (again, _) = load(Some(&m), sales()).unwrap();
        assert_eq!(again, m);
        // MonthNo gone: its properties and the sort by it go too.
        let fewer = table(vec![("Month", text(&["Jan"])), ("Extra", text(&["x"]))]);
        let (after, _) = load(Some(&m), fewer).unwrap();
        assert_eq!(after.column("Month").unwrap().sort_by, None);
        assert_eq!(
            after.column("Extra").unwrap(),
            &Column::new("Extra", ColumnType::Text)
        );
        assert!(after.hidden);
        // Back again: defaults.
        let (back, _) = load(Some(&after), sales()).unwrap();
        assert_eq!(
            back.column("MonthNo").unwrap(),
            &Column::new("MonthNo", ColumnType::Whole)
        );
    }

    #[test]
    fn sort_by_needs_one_value_per_value() {
        let (mut m, t) = load(None, sales()).unwrap();
        let before = m.clone();
        for (column, by) in [
            ("Month", "Month"),
            ("Month", "Nope"),
            ("Nope", "Month"),
            ("Month", "Price"),
        ] {
            assert!(
                m.set_sort_by(&t, column, Some(by)).is_err(),
                "{column} by {by}"
            );
            assert_eq!(m, before);
        }
        let e = m.set_sort_by(&t, "Month", Some("Price")).unwrap_err();
        assert!(
            e.to_string().contains("more than one value in 'Price'"),
            "{e}"
        );
        m.set_sort_by(&t, "Month", Some("MonthNo")).unwrap();
        assert!(
            m.set_sort_by(&t, "MonthNo", Some("Month")).is_err(),
            "circle"
        );
        m.set_sort_by(&t, "Month", None).unwrap();
        assert_eq!(m, before);
        // A load whose data breaks the rule fails, naming the table and column.
        m.set_sort_by(&t, "Month", Some("MonthNo")).unwrap();
        let broken = table(vec![
            ("Month", text(&["Jan", "Jan"])),
            ("MonthNo", Arc::new(Int64Array::from(vec![1, 2]))),
        ]);
        let e = load(Some(&m), broken).unwrap_err();
        assert_eq!((e.table.as_str(), e.column.as_str()), ("Sales", "Month"));
    }

    #[test]
    fn a_type_override_converts_the_data_and_is_reapplied_after_a_load() {
        let (mut m, t) = load(None, sales()).unwrap();
        let t = m
            .set_data_type(&t, "Price", ColumnType::FixedDecimal, us(), usize::MAX)
            .unwrap();
        let price = t.batch().column_by_name("Price").unwrap();
        let fixed = price.as_any().downcast_ref::<Decimal32Array>().unwrap();
        assert_eq!(fixed.values(), &[1_000, 2_000, 15_000]);
        assert_eq!(fixed.value(0) + fixed.value(1), 3_000, "exact");
        let c = m.column("Price").unwrap();
        assert!(c.overridden());
        assert_eq!(c.query_type, ColumnType::Decimal);
        assert_eq!(c.summarize_by, SummarizeBy::Sum);
        // Reloaded: the Query still gives decimal, the override converts it again.
        let (again, t2) = load(Some(&m), sales()).unwrap();
        assert_eq!(again, m);
        assert_eq!(t2.batch().column(2).data_type(), &DataType::Decimal32(9, 4));
        // To text: summarize by resets; the data is text.
        let t3 = m
            .set_data_type(&t2, "MonthNo", ColumnType::Text, us(), usize::MAX)
            .unwrap();
        assert_eq!(m.column("MonthNo").unwrap().summarize_by, SummarizeBy::None);
        assert!(
            opendash_data::step::ColumnType::of_data_type(t3.batch().column(1).data_type())
                == Some(ColumnType::Text)
        );
    }

    #[test]
    fn a_type_change_that_fails_keeps_the_old_type() {
        let (mut m, t) = load(None, sales()).unwrap();
        let before = m.clone();
        let e = m
            .set_data_type(&t, "Month", ColumnType::Whole, us(), usize::MAX)
            .unwrap_err();
        assert_eq!(
            e.message,
            "Cannot convert value 'Jan' of type Text to type Integer."
        );
        assert_eq!(m, before);
        // An override the new data can't take fails the load.
        let t = table(vec![("Code", text(&["1", "2"]))]);
        let (mut m, t) = load(None, t).unwrap();
        m.set_data_type(&t, "Code", ColumnType::Whole, us(), usize::MAX)
            .unwrap();
        let bad = table(vec![("Code", text(&["1", "x"]))]);
        let e = load(Some(&m), bad).unwrap_err();
        assert!(e.message.contains("'x'"), "{e}");
    }

    #[test]
    fn summarize_by_names_round_trip() {
        for s in SummarizeBy::ALL {
            assert_eq!(SummarizeBy::from_name(s.name()), Some(s));
        }
        assert!(!SummarizeBy::Sum.allowed_for(ColumnType::Text));
        assert!(SummarizeBy::Count.allowed_for(ColumnType::Date));
    }
}
