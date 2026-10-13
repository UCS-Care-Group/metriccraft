//! The DAX evaluator (ADR 0006): a **formula engine** ([`eval`]) interpreting the
//! `opendash_dax` AST, over a **storage tier** ([`storage`]) that scans the encoded columns of
//! one Table under a row mask.
//!
//! - **Filter context** ([`FilterContext`]): a list of [`Filter`]s, each a single-column set of
//!   allowed values (dictionary-code bitsets for text) or a row set of one Table. A Table's
//!   visible rows ([`eval::Evaluator::mask`]) are its own filters ANDed with the filters of the
//!   Tables it reaches through active Relationships (many → one), propagated one → many as a
//!   key semijoin: the *expanded table* rule, single direction (spec D-2).
//! - **Row context**: a stack of (Table, row) frames; `T[C]` reads the innermost frame on `T`.
//! - **Errors** ([`DaxError`]) carry the span of the failing sub-expression.
//! - **Bounds**: evaluation depth [`MAX_EVAL_DEPTH`], Measure chains [`MAX_MEASURE_CHAIN`],
//!   Relationship paths [`MAX_RELATIONSHIP_DEPTH`]; intermediate tables count against the memory
//!   cap left after the loaded data; the cancel flag is checked between scans and every
//!   [`storage::CHUNK`] rows.
//!
//! **Adding a DAX function** is a one-place change in [`functions`]: see its module doc.

pub mod eval;
pub mod functions;
pub mod storage;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::AtomicBool;

use opendash_core::Value;
use opendash_data::Table;
use opendash_dax::{Query, Span};

use crate::Model;
pub use eval::{Evaluator, Rows, RowsColumn, TableVal, Val};
pub use storage::{Bitset, Filter, Key};
use storage::{Col, DbColumn, DbTable};

/// Nested evaluation levels (expression nesting plus the Measures it references), so a deep
/// expression can't overflow the stack.
pub const MAX_EVAL_DEPTH: usize = 512;
/// Measures referencing Measures, deepest chain (ADR 0006).
pub const MAX_MEASURE_CHAIN: usize = 64;
/// Relationships followed from one Table when propagating filters.
pub const MAX_RELATIONSHIP_DEPTH: usize = 64;

/// What went wrong, for callers that treat them differently (`ISERROR` catches only `Eval`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The text doesn't parse.
    Syntax,
    /// A DAX error: a type mismatch, an overflow, an unknown name.
    Eval,
    /// A bound was hit (depth, Measure chain, Relationship path).
    Limit,
    /// Intermediate tables would exceed the memory cap.
    Memory,
    /// The cancel flag was set.
    Cancelled,
}

/// A DAX error with where in the expression it happened (spec X-3).
#[derive(Clone, Debug, PartialEq)]
pub struct DaxError {
    pub kind: ErrorKind,
    pub span: Span,
    pub message: String,
}

impl DaxError {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Eval,
            span,
            message: message.into(),
        }
    }

    pub fn limit(span: Span, message: impl Into<String>) -> Self {
        Self {
            kind: ErrorKind::Limit,
            ..Self::new(span, message)
        }
    }

    pub fn cancelled() -> Self {
        Self {
            kind: ErrorKind::Cancelled,
            ..Self::new(Span::default(), "the query was cancelled")
        }
    }
}

impl fmt::Display for DaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.span.start)
    }
}

impl std::error::Error for DaxError {}

pub type Result<T> = std::result::Result<T, DaxError>;

/// A Relationship by index, for propagation.
#[derive(Clone, Debug)]
pub struct DbRelationship {
    pub from_table: usize,
    pub from_column: usize,
    pub to_table: usize,
    pub to_column: usize,
    pub active: bool,
}

/// The model's data as the evaluator reads it: every loaded Table (by the model's names) and
/// the Relationships between them, resolved to indices.
#[derive(Clone, Debug)]
pub struct Database {
    pub tables: Vec<DbTable>,
    pub relationships: Vec<DbRelationship>,
    /// Bytes held by the loaded data (distinct buffers): the memory cap minus this is what
    /// intermediate tables may use.
    pub loaded_bytes: usize,
}

impl Database {
    /// Readers over `data` (the loaded Tables; the columns are shared, not copied) and
    /// `model`'s Relationships between them. A Relationship whose Table or Column isn't loaded
    /// is left out.
    pub fn new(model: &Model, data: &[Table]) -> Self {
        let tables: Vec<DbTable> = data
            .iter()
            .map(|t| {
                let batch = t.batch();
                let schema = batch.schema();
                DbTable {
                    name: t.name().to_string(),
                    rows: t.rows(),
                    columns: schema
                        .fields()
                        .iter()
                        .zip(batch.columns())
                        .map(|(f, a)| DbColumn {
                            name: f.name().clone(),
                            data: Col::new(a),
                        })
                        .collect(),
                }
            })
            .collect();
        let find = |table: &str, column: &str| {
            let t = tables.iter().position(|t| t.name == table)?;
            let c = tables
                .get(t)?
                .columns
                .iter()
                .position(|c| c.name == column)?;
            Some((t, c))
        };
        let relationships = model
            .relationships
            .iter()
            .filter_map(|r| {
                let (from_table, from_column) = find(&r.from_table, &r.from_column)?;
                let (to_table, to_column) = find(&r.to_table, &r.to_column)?;
                Some(DbRelationship {
                    from_table,
                    from_column,
                    to_table,
                    to_column,
                    active: r.active,
                })
            })
            .collect();
        let mut seen = HashSet::new();
        let loaded_bytes = data
            .iter()
            .flat_map(Table::buffers)
            .filter(|(addr, _)| seen.insert(*addr))
            .map(|(_, bytes)| bytes)
            .fold(0usize, usize::saturating_add);
        Self {
            tables,
            relationships,
            loaded_bytes,
        }
    }

    /// The Table named `name`, ignoring case.
    pub fn table(&self, name: &str) -> Option<usize> {
        self.tables
            .iter()
            .position(|t| crate::same_name(&t.name, name))
    }
}

/// The filter context: filters ANDed together. Cheap to clone (filters are shared).
#[derive(Clone, Debug, Default)]
pub struct FilterContext {
    pub filters: Vec<std::sync::Arc<Filter>>,
}

impl FilterContext {
    /// This context with `filter` added.
    pub fn with(&self, filter: Filter) -> Self {
        let mut c = self.clone();
        c.filters.push(std::sync::Arc::new(filter));
        c
    }
}

/// How a query runs.
#[derive(Clone, Copy, Debug)]
pub struct Options<'c> {
    /// Set (from another thread) to stop the query with [`ErrorKind::Cancelled`].
    pub cancel: Option<&'c AtomicBool>,
    /// The memory cap (`opendash_data::default_memory_cap`): intermediate tables may use what
    /// the loaded data leaves of it.
    pub memory_cap: usize,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            cancel: None,
            memory_cap: opendash_data::default_memory_cap(),
        }
    }
}

/// One `EVALUATE`'s result.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl ResultTable {
    /// Column `name`'s values, top to bottom.
    pub fn column(&self, name: &str) -> Option<Vec<&Value>> {
        let i = self.columns.iter().position(|c| c == name)?;
        self.rows.iter().map(|r| r.get(i)).collect()
    }
}

/// Runs a parsed DAX query: `DEFINE` (Measures, query `VAR`s), then each `EVALUATE` (with its
/// `ORDER BY`).
pub fn run_query(db: &Database, query: &Query, options: Options<'_>) -> Result<Vec<ResultTable>> {
    Evaluator::new(db, options).run(query)
}

/// Parses and runs DAX query text; a parse error is an [`ErrorKind::Syntax`] error.
pub fn run_query_text(db: &Database, text: &str, options: Options<'_>) -> Result<Vec<ResultTable>> {
    let query = opendash_dax::parse_query(text).map_err(|e| DaxError {
        kind: ErrorKind::Syntax,
        span: e.span,
        message: e.message,
    })?;
    run_query(db, &query, options)
}
