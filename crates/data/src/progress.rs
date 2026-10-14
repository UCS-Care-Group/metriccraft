//! A load's progress and cancel switch (spec U-3, ADR 0008), shared with the thread that shows
//! it: rows read per Query so far, and a flag the load checks on every batch it reads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use crate::{Error, Result};

#[derive(Debug, Default)]
pub struct Progress {
    cancel: AtomicBool,
    rows: Mutex<Vec<(String, u64)>>,
}

impl Progress {
    /// Asks the load running now to stop; it fails with [`Error::Cancelled`] at its next batch.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Clears the counts and the flag, for a new load.
    pub fn reset(&self) {
        self.cancel.store(false, Ordering::Relaxed);
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Rows read so far, per Query, in the order they started.
    pub fn rows(&self) -> Vec<(String, u64)> {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Counts `n` more rows read for `query`; fails if cancelled.
    pub(crate) fn read(&self, query: &str, n: usize) -> Result {
        {
            let mut rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
            let n = u64::try_from(n).unwrap_or(u64::MAX);
            match rows.iter_mut().find(|(q, _)| q == query) {
                Some((_, r)) => *r = r.saturating_add(n),
                None => rows.push((query.to_string(), n)),
            }
        }
        self.check()
    }

    /// Fails if cancelled.
    pub(crate) fn check(&self) -> Result {
        if self.cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}
