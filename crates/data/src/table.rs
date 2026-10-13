//! Loaded tables: immutable, reference-counted Arrow columns, encoded narrow (ADR 0005).

use std::sync::Arc;

use arrow_array::builder::StringDictionaryBuilder;
use arrow_array::types::{Int8Type, Int16Type, Int32Type};
use arrow_array::{
    Array, ArrayRef, Decimal32Array, Decimal64Array, DictionaryArray, Int8Array, Int16Array,
    Int32Array, Int64Array, PrimitiveArray, RecordBatch, StringArray,
};
use arrow_buffer::{NullBuffer, NullBufferBuilder};
use arrow_schema::{DataType, Field, Schema, SchemaRef};

use crate::{Error, Result};

/// A loaded table. Cloning shares the column buffers; nothing is ever mutated in place, so a
/// clone (e.g. an undo snapshot) costs a few pointers, not the data.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    name: Arc<str>,
    batch: RecordBatch,
    /// Whether loading folded text that differed only by case (`fold.rs`): the data is then no
    /// longer its Query's result as Power Query sees it. Not saved with the Data cache.
    folded: bool,
}

impl Table {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The columns. Text is `Dictionary(Int8|Int16|Int32, Utf8)`; whole numbers are the
    /// narrowest of `Int8`..`Int64` that holds every value.
    pub fn batch(&self) -> &RecordBatch {
        &self.batch
    }

    pub fn rows(&self) -> usize {
        self.batch.num_rows()
    }

    /// Whether [`crate::fold_case`] changed any of this table's text.
    pub fn folded(&self) -> bool {
        self.folded
    }

    /// This table with `columns` (same types, same rows) in place of its own, marked folded.
    pub(crate) fn with_folded(&self, columns: Vec<ArrayRef>) -> Result<Table> {
        let batch = RecordBatch::try_new(self.batch.schema(), columns)
            .map_err(|e| self.error(e.to_string()))?;
        Ok(Table {
            name: Arc::clone(&self.name),
            batch,
            folded: true,
        })
    }

    /// This table with column `index` replaced by `column` (a plain Arrow array, as a step
    /// gives: encoded narrow here), keeping its name; the other columns are shared. At most
    /// `limit` bytes for the new column.
    pub fn with_column(&self, index: usize, column: ArrayRef, limit: usize) -> Result<Table> {
        let schema = self.batch.schema();
        let field = schema
            .fields()
            .get(index)
            .ok_or_else(|| self.error(format!("there is no column {index}")))?;
        let plain = Arc::new(Schema::new(vec![Field::new(
            field.name(),
            column.data_type().clone(),
            true,
        )]));
        let one = RecordBatch::try_new(Arc::clone(&plain), vec![column])
            .map_err(|e| self.error(e.to_string()))?;
        let mut b = TableBuilder::new(&self.name, plain, limit);
        b.push(&one)?;
        let encoded = b.finish()?;
        let (Some(new_field), Some(new_column)) = (
            encoded.batch.schema().fields().first().cloned(),
            encoded.batch.columns().first().cloned(),
        ) else {
            return Err(self.error("the converted column is missing".into()));
        };
        let mut fields: Vec<_> = schema.fields().iter().cloned().collect();
        let mut columns = self.batch.columns().to_vec();
        if let (Some(f), Some(c)) = (fields.get_mut(index), columns.get_mut(index)) {
            *f = new_field;
            *c = new_column;
        }
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
            .map_err(|e| self.error(e.to_string()))?;
        Ok(Table {
            name: Arc::clone(&self.name),
            batch,
            folded: self.folded,
        })
    }

    /// This table under another name (a Table rename); the columns are shared.
    pub fn renamed(&self, name: &str) -> Table {
        Table {
            name: Arc::from(name),
            batch: self.batch.clone(),
            folded: self.folded,
        }
    }

    /// This table with column `old` named `new` (a model column rename); the data is shared.
    pub fn with_column_renamed(&self, old: &str, new: &str) -> Result<Table> {
        let schema = self.batch.schema();
        let index = schema
            .index_of(old)
            .map_err(|_| self.error(format!("there is no column `{old}`")))?;
        let mut fields: Vec<_> = schema.fields().iter().cloned().collect();
        if let Some(f) = fields.get_mut(index) {
            *f = Arc::new(f.as_ref().clone().with_name(new));
        }
        let batch =
            RecordBatch::try_new(Arc::new(Schema::new(fields)), self.batch.columns().to_vec())
                .map_err(|e| self.error(e.to_string()))?;
        Ok(Table {
            name: Arc::clone(&self.name),
            batch,
            folded: self.folded,
        })
    }

    fn error(&self, message: String) -> Error {
        Error::Arrow {
            table: self.name.to_string(),
            message,
        }
    }

    /// Bytes held by the column buffers.
    pub fn memory(&self) -> usize {
        self.batch.get_array_memory_size()
    }

    /// The column buffers' allocations, as `(address, bytes)`. Clones of a table (undo
    /// snapshots) share them: counting distinct addresses gives the memory really held.
    pub fn buffers(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for column in self.batch.columns() {
            buffers(column.as_ref(), &mut out);
        }
        out
    }
}

/// Appends `array`'s buffers (values, offsets, validity, and its children's: a dictionary's
/// values) to `out`. Recursion follows the type's nesting, which our tables keep to one level.
fn buffers(array: &dyn Array, out: &mut Vec<(usize, usize)>) {
    let data = array.to_data();
    let nulls = data.nulls().map(NullBuffer::buffer);
    for b in data.buffers().iter().chain(nulls) {
        out.push((b.data_ptr().as_ptr().addr(), b.capacity()));
    }
    for child in data.child_data() {
        buffers(arrow_array::make_array(child.clone()).as_ref(), out);
    }
}

/// Default memory cap for loaded data: half of physical RAM (1 GB if unknown), 1 GB on the web.
pub fn default_memory_cap() -> usize {
    const GB: usize = 1 << 30;
    #[cfg(target_arch = "wasm32")]
    {
        GB
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        match usize::try_from(sys.total_memory() / 2) {
            Ok(0) | Err(_) => GB,
            Ok(half) => half,
        }
    }
}

/// Builds a [`Table`] from a stream of plain Arrow batches (`Utf8`, `Int64`, anything else kept
/// as is), encoding as it goes so peak memory is about the finished table plus one batch.
pub struct TableBuilder {
    name: String,
    schema: SchemaRef,
    columns: Vec<Col>,
    limit: usize,
}

enum Col {
    Text {
        dict: Box<StringDictionaryBuilder<Int32Type>>,
        rows: usize,
        distinct: usize,
        bytes: usize,
    },
    // ponytail: held at i64 until finish, then narrowed (peak + one 8-byte column). Track the
    // width while appending if that peak matters.
    Int {
        values: Vec<i64>,
        nulls: NullBufferBuilder,
    },
    /// Fixed decimal (`Decimal64(_, 4)`): the same, narrowed to `Decimal32` or kept.
    Fixed {
        values: Vec<i64>,
        nulls: NullBufferBuilder,
    },
    Other(Vec<ArrayRef>),
}

impl Col {
    fn memory(&self) -> usize {
        match self {
            // Keys + distinct strings + offsets and the builder's hash table (~16 B each).
            Col::Text {
                rows,
                distinct,
                bytes,
                ..
            } => rows * 4 + bytes + distinct * 20,
            Col::Int { values, .. } | Col::Fixed { values, .. } => {
                values.capacity() * 8 + values.len() / 8
            }
            Col::Other(chunks) => chunks.iter().map(|c| c.get_array_memory_size()).sum(),
        }
    }
}

impl TableBuilder {
    /// `limit` is the memory this table may use: the cap minus everything already loaded.
    pub fn new(name: &str, schema: SchemaRef, limit: usize) -> Self {
        let columns = schema
            .fields()
            .iter()
            .map(|f| match f.data_type() {
                DataType::Utf8 => Col::Text {
                    dict: Box::default(),
                    rows: 0,
                    distinct: 0,
                    bytes: 0,
                },
                DataType::Int64 => Col::Int {
                    values: Vec::new(),
                    nulls: NullBufferBuilder::new(0),
                },
                DataType::Decimal64(_, 4) => Col::Fixed {
                    values: Vec::new(),
                    nulls: NullBufferBuilder::new(0),
                },
                _ => Col::Other(Vec::new()),
            })
            .collect();
        Self {
            name: name.into(),
            schema,
            columns,
            limit,
        }
    }

    /// Appends one batch; errors if its column names or types don't match the schema, or the
    /// table outgrows its limit.
    pub fn push(&mut self, batch: &RecordBatch) -> Result {
        let columns = |s: &Schema| {
            s.fields()
                .iter()
                .map(|f| (f.name().clone(), f.data_type().clone()))
                .collect()
        };
        type Columns = Vec<(String, DataType)>;
        let (got, want): (Columns, Columns) = (columns(&batch.schema()), columns(&self.schema));
        if got != want {
            return Err(self.error(format!("batch columns {got:?} don't match {want:?}")));
        }
        for (col, array) in self.columns.iter_mut().zip(batch.columns()) {
            match col {
                Col::Text {
                    dict,
                    rows,
                    distinct,
                    bytes,
                } => {
                    let Some(strings) = array.as_any().downcast_ref::<StringArray>() else {
                        continue;
                    };
                    for s in strings {
                        *rows += 1;
                        let Some(s) = s else {
                            dict.append_null();
                            continue;
                        };
                        let key = dict.append(s).map_err(|e| Error::Arrow {
                            table: self.name.clone(),
                            message: e.to_string(),
                        })?;
                        if usize::try_from(key).is_ok_and(|k| k == *distinct) {
                            *distinct += 1;
                            *bytes += s.len();
                        }
                    }
                }
                Col::Int { values, nulls } => {
                    let Some(ints) = array.as_any().downcast_ref::<Int64Array>() else {
                        continue;
                    };
                    values.extend_from_slice(ints.values());
                    match ints.nulls() {
                        Some(n) => nulls.append_buffer(n),
                        None => nulls.append_n_non_nulls(ints.len()),
                    }
                }
                Col::Fixed { values, nulls } => {
                    let Some(fixed) = array.as_any().downcast_ref::<Decimal64Array>() else {
                        continue;
                    };
                    values.extend_from_slice(fixed.values());
                    match fixed.nulls() {
                        Some(n) => nulls.append_buffer(n),
                        None => nulls.append_n_non_nulls(fixed.len()),
                    }
                }
                Col::Other(chunks) => chunks.push(Arc::clone(array)),
            }
        }
        let used: usize = self.columns.iter().map(Col::memory).sum();
        if used > self.limit {
            return Err(Error::MemoryCap {
                table: self.name.clone(),
                limit: self.limit,
            });
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Table> {
        let mut fields = Vec::with_capacity(self.columns.len());
        let mut arrays = Vec::with_capacity(self.columns.len());
        for (field, col) in self.schema.fields().iter().zip(self.columns) {
            let array: ArrayRef = match col {
                Col::Text { mut dict, .. } => {
                    narrow_keys(dict.finish()).map_err(|message| Error::Arrow {
                        table: self.name.clone(),
                        message,
                    })?
                }
                Col::Int { values, mut nulls } => narrow_ints(values, nulls.finish()),
                Col::Fixed { values, mut nulls } => narrow_fixed(values, nulls.finish()),
                Col::Other(chunks) => {
                    let refs: Vec<&dyn Array> = chunks.iter().map(|c| c.as_ref()).collect();
                    if refs.is_empty() {
                        arrow_array::new_empty_array(field.data_type())
                    } else {
                        arrow_select::concat::concat(&refs).map_err(|e| Error::Arrow {
                            table: self.name.clone(),
                            message: e.to_string(),
                        })?
                    }
                }
            };
            fields.push(Field::new(
                field.name(),
                array.data_type().clone(),
                field.is_nullable(),
            ));
            arrays.push(array);
        }
        let batch = RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| {
            Error::Arrow {
                table: self.name.clone(),
                message: e.to_string(),
            }
        })?;
        Ok(Table {
            name: self.name.into(),
            batch,
            folded: false,
        })
    }

    fn error(&self, message: String) -> Error {
        Error::Arrow {
            table: self.name.clone(),
            message,
        }
    }
}

/// Re-keys a dictionary with the narrowest key type that indexes every distinct value.
fn narrow_keys(dict: DictionaryArray<Int32Type>) -> std::result::Result<ArrayRef, String> {
    let n = dict.values().len();
    let keys = dict.keys();
    let values = Arc::clone(dict.values());
    let rekey =
        |r: std::result::Result<ArrayRef, arrow_schema::ArrowError>| r.map_err(|e| e.to_string());
    if n <= 128 {
        let k = Int8Array::new(
            keys.values()
                .iter()
                .map(|&k| i8::try_from(k).unwrap_or_default())
                .collect(),
            keys.nulls().cloned(),
        );
        rekey(DictionaryArray::<Int8Type>::try_new(k, values).map(|d| Arc::new(d) as ArrayRef))
    } else if n <= 32_768 {
        let k = Int16Array::new(
            keys.values()
                .iter()
                .map(|&k| i16::try_from(k).unwrap_or_default())
                .collect(),
            keys.nulls().cloned(),
        );
        rekey(DictionaryArray::<Int16Type>::try_new(k, values).map(|d| Arc::new(d) as ArrayRef))
    } else {
        Ok(Arc::new(dict))
    }
}

/// Stores fixed decimals (× 10⁴) as `Decimal32(9, 4)` when every value fits an `i32`, else
/// `Decimal64(18, 4)`. Arrow accepts any value of the storage type: precision isn't validated.
fn narrow_fixed(values: Vec<i64>, nulls: Option<NullBuffer>) -> ArrayRef {
    let fits = values.iter().all(|v| i32::try_from(*v).is_ok());
    if fits {
        let narrow: Vec<i32> = values
            .iter()
            .map(|&v| i32::try_from(v).unwrap_or_default())
            .collect();
        Arc::new(
            Decimal32Array::new(narrow.into(), nulls).with_data_type(DataType::Decimal32(9, 4)),
        )
    } else {
        Arc::new(Decimal64Array::new(values.into(), nulls).with_data_type(crate::step::FIXED))
    }
}

/// Stores whole numbers at the narrowest width that holds them all (null slots included).
fn narrow_ints(values: Vec<i64>, nulls: Option<NullBuffer>) -> ArrayRef {
    let lo = values.iter().copied().min().unwrap_or(0);
    let hi = values.iter().copied().max().unwrap_or(0);
    let fits = |min: i64, max: i64| lo >= min && hi <= max;
    if fits(i8::MIN.into(), i8::MAX.into()) {
        Arc::new(Int8Array::new(
            values
                .iter()
                .map(|&v| i8::try_from(v).unwrap_or_default())
                .collect(),
            nulls,
        ))
    } else if fits(i16::MIN.into(), i16::MAX.into()) {
        Arc::new(Int16Array::new(
            values
                .iter()
                .map(|&v| i16::try_from(v).unwrap_or_default())
                .collect(),
            nulls,
        ))
    } else if fits(i32::MIN.into(), i32::MAX.into()) {
        Arc::new(Int32Array::new(
            values
                .iter()
                .map(|&v| i32::try_from(v).unwrap_or_default())
                .collect(),
            nulls,
        ))
    } else {
        Arc::new(PrimitiveArray::<arrow_array::types::Int64Type>::new(
            values.into(),
            nulls,
        ))
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::Float64Array;

    use super::*;

    fn schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("Region", DataType::Utf8, true),
            Field::new("Qty", DataType::Int64, true),
            Field::new("Amount", DataType::Float64, true),
        ]))
    }

    fn batch(regions: Vec<Option<&str>>, qty: Vec<Option<i64>>, amount: Vec<f64>) -> RecordBatch {
        RecordBatch::try_new(
            schema(),
            vec![
                Arc::new(StringArray::from(regions)),
                Arc::new(Int64Array::from(qty)),
                Arc::new(Float64Array::from(amount)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn encodes_narrow_across_batches() {
        let mut b = TableBuilder::new("Sales", schema(), usize::MAX);
        b.push(&batch(
            vec![Some("North"), None],
            vec![Some(1), None],
            vec![1.5, 2.0],
        ))
        .unwrap();
        b.push(&batch(
            vec![Some("North"), Some("South")],
            vec![Some(300), Some(-2)],
            vec![3.0, 4.0],
        ))
        .unwrap();
        let t = b.finish().unwrap();
        assert_eq!(t.rows(), 4);
        let types: Vec<_> = t
            .batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.data_type().clone())
            .collect();
        assert_eq!(
            types,
            [
                DataType::Dictionary(Box::new(DataType::Int8), Box::new(DataType::Utf8)),
                DataType::Int16,
                DataType::Float64
            ]
        );
        let region = t
            .batch()
            .column(0)
            .as_any()
            .downcast_ref::<DictionaryArray<Int8Type>>()
            .unwrap();
        assert_eq!(region.values().len(), 2, "North stored once");
        assert!(region.is_null(1));
        let qty = t
            .batch()
            .column(1)
            .as_any()
            .downcast_ref::<Int16Array>()
            .unwrap();
        assert_eq!(
            qty.iter().collect::<Vec<_>>(),
            [Some(1), None, Some(300), Some(-2)]
        );
    }

    #[test]
    fn fixed_decimals_narrow_to_decimal32_when_they_fit() {
        for (v, want) in [
            (2_147_483_647, DataType::Decimal32(9, 4)),
            (2_147_483_648, crate::step::FIXED),
        ] {
            let column = crate::step::fixed_array(vec![Some(v), None, Some(-5)]);
            let batch = RecordBatch::try_from_iter([("Price", column)]).unwrap();
            let mut b = TableBuilder::new("T", batch.schema(), usize::MAX);
            b.push(&batch).unwrap();
            let t = b.finish().unwrap();
            let c = t.batch().column(0);
            assert_eq!(c.data_type(), &want);
            assert!(c.is_null(1));
            let wide = arrow_cast::cast(c, &crate::step::FIXED).unwrap();
            let wide = wide
                .as_any()
                .downcast_ref::<arrow_array::Decimal64Array>()
                .unwrap();
            assert_eq!(wide.iter().collect::<Vec<_>>(), [Some(v), None, Some(-5)]);
        }
    }

    #[test]
    fn with_column_encodes_the_new_column_and_shares_the_rest() {
        let mut b = TableBuilder::new("Sales", schema(), usize::MAX);
        b.push(&batch(vec![Some("a")], vec![Some(1)], vec![2.5]))
            .unwrap();
        let t = b.finish().unwrap();
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![7]));
        let t2 = t.with_column(2, ints, usize::MAX).unwrap();
        assert_eq!(t2.name(), "Sales");
        assert_eq!(t2.batch().column(2).data_type(), &DataType::Int8);
        assert_eq!(t2.batch().schema().field(2).name(), "Amount");
        assert_eq!(t.buffers()[0], t2.buffers()[0], "Region shared");
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![7]));
        assert!(t.with_column(9, ints, usize::MAX).is_err());
    }

    #[test]
    fn widths_follow_the_range() {
        for (v, want) in [
            (127, DataType::Int8),
            (-129, DataType::Int16),
            (40_000, DataType::Int32),
            (i64::MAX, DataType::Int64),
        ] {
            assert_eq!(narrow_ints(vec![0, v], None).data_type(), &want, "{v}");
        }
    }

    #[test]
    fn many_distinct_strings_get_wider_keys() {
        let s: Vec<String> = (0..200).map(|i| i.to_string()).collect();
        let mut b = TableBuilder::new("T", schema(), usize::MAX);
        b.push(&batch(
            s.iter().map(|s| Some(s.as_str())).collect(),
            vec![Some(0); 200],
            vec![0.0; 200],
        ))
        .unwrap();
        let t = b.finish().unwrap();
        assert_eq!(
            t.batch().schema().field(0).data_type(),
            &DataType::Dictionary(Box::new(DataType::Int16), Box::new(DataType::Utf8))
        );
    }

    #[test]
    fn memory_cap_names_the_table() {
        let mut b = TableBuilder::new("Sales", schema(), 1_000);
        let n = 100;
        let r = (0..20)
            .map(|_| b.push(&batch(vec![Some("x"); n], vec![Some(1); n], vec![0.0; n])))
            .find(Result::is_err);
        let err = r.unwrap().unwrap_err();
        assert_eq!(
            err,
            Error::MemoryCap {
                table: "Sales".into(),
                limit: 1_000
            }
        );
        assert!(err.to_string().contains("Sales"));
    }

    #[test]
    fn mismatched_batch_is_an_error() {
        let mut b = TableBuilder::new("Sales", schema(), usize::MAX);
        let other = RecordBatch::try_new(
            Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)])),
            vec![Arc::new(Int64Array::from(vec![1]))],
        )
        .unwrap();
        assert!(b.push(&other).is_err());
    }

    #[test]
    fn a_batch_with_other_column_names_is_an_error() {
        let mut b = TableBuilder::new("Sales", schema(), usize::MAX);
        let renamed = Arc::new(Schema::new(vec![
            Field::new("Column1", DataType::Utf8, true),
            Field::new("Qty", DataType::Int64, true),
            Field::new("Amount", DataType::Float64, true),
        ]));
        let columns = batch(vec![Some("x")], vec![Some(1)], vec![0.0])
            .columns()
            .to_vec();
        let other = RecordBatch::try_new(renamed, columns).unwrap();
        let e = b.push(&other).unwrap_err();
        assert!(e.to_string().contains("Column1"), "{e}");
    }

    #[test]
    fn empty_table_finishes() {
        let t = TableBuilder::new("Empty", schema(), usize::MAX)
            .finish()
            .unwrap();
        assert_eq!(t.rows(), 0);
    }

    #[test]
    fn default_cap_is_positive() {
        assert!(default_memory_cap() > 0);
    }
}
