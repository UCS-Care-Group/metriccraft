//! Sources: CSV, Parquet, XLSX (spec S-1). Each yields plain Arrow batches of at most
//! [`BATCH_ROWS`] rows (`Utf8`, `Int64`, `Float64`, `Boolean`, `Date32`,
//! `Timestamp(Microsecond, None)`), which [`load`] streams into a [`Table`] (ADR 0005).

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;

use crate::{Result, Table, TableBuilder};

pub mod parquet;

/// Rows per batch read from a Source.
pub const BATCH_ROWS: usize = 8192;

/// Streams batches into a Table, stopping at the first error or once `limit` bytes are used.
pub fn load(
    name: &str,
    schema: SchemaRef,
    batches: impl IntoIterator<Item = Result<RecordBatch>>,
    limit: usize,
) -> Result<Table> {
    let mut b = TableBuilder::new(name, schema, limit);
    for batch in batches {
        b.push(&batch?)?;
    }
    b.finish()
}
