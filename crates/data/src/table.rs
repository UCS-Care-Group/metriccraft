//! Loaded tables: immutable, reference-counted Arrow columns, encoded narrow (ADR 0005).

use std::sync::Arc;

use arrow_array::builder::StringDictionaryBuilder;
use arrow_array::types::{Int8Type, Int16Type, Int32Type};
use arrow_array::{
    Array, ArrayRef, DictionaryArray, Int8Array, Int16Array, Int32Array, Int64Array,
    PrimitiveArray, RecordBatch, StringArray,
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
            Col::Int { values, .. } => values.capacity() * 8 + values.len() / 8,
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
        let (got, want): (Vec<(String, DataType)>, Vec<(String, DataType)>) =
            (columns(&batch.schema()), columns(&self.schema));
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
