//! Test-only fuzz harness for the fuzz tests of this crate and of `metriccraft-data` (which
//! includes this file by path): a deterministic xorshift generator, and a per-case net that
//! records panics and hangs with their input instead of stopping at the first.
//!
//! Soak knobs (environment), all optional; the default run stays quick:
//! - `MCRAFT_FUZZ_SCALE=N` multiplies each test's random case count (default 1);
//! - `MCRAFT_FUZZ_SEED=S` (decimal or `0x` hex) reseeds the generator;
//! - `MCRAFT_FUZZ_OUT=dir` is where failing inputs are written (default: the temp dir);
//! - `MCRAFT_FUZZ_SLOW_MS=ms` is the per-case time limit counted as a hang (default 5000);
//! - `MCRAFT_FUZZ_TRACE=1` writes every input to `<out>/<test>.current` before running it, so
//!   an abort (allocation failure under `ulimit -v`) or a true hang leaves its input behind.
//!
//! `MCRAFT_FUZZ_SCALE=100 MCRAFT_FUZZ_SEED=7 cargo test --release -p metriccraft-data never_panic`

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Fuzz {
    name: &'static str,
    pub scale: usize,
    seed: u64,
    state: u64,
    cases: usize,
    failures: usize,
    slow: Duration,
    out: PathBuf,
    trace: bool,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

impl Fuzz {
    pub fn new(name: &'static str, default_seed: u64) -> Self {
        let seed = env("MCRAFT_FUZZ_SEED")
            .map(|s| match s.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16).unwrap(),
                None => s.parse().unwrap(),
            })
            .unwrap_or(default_seed);
        Self {
            name,
            scale: env("MCRAFT_FUZZ_SCALE").map_or(1, |s| s.parse().unwrap()),
            seed,
            // xorshift has a fixed point at 0.
            state: seed.max(1),
            cases: 0,
            failures: 0,
            slow: Duration::from_millis(
                env("MCRAFT_FUZZ_SLOW_MS").map_or(5000, |s| s.parse().unwrap()),
            ),
            out: env("MCRAFT_FUZZ_OUT").map_or_else(std::env::temp_dir, PathBuf::from),
            trace: env("MCRAFT_FUZZ_TRACE").is_some(),
        }
    }

    /// `n` cases, times the soak scale.
    pub fn scaled(&self, n: usize) -> usize {
        n.saturating_mul(self.scale)
    }

    pub fn next(&mut self) -> u64 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// Runs one case on `input`; a panic or a case over the time limit is a failure, saved.
    pub fn case(&mut self, input: &[u8], f: impl FnOnce()) {
        self.cases += 1;
        if self.trace {
            std::fs::write(self.out.join(format!("{}.current", self.name)), input).unwrap();
        }
        let start = Instant::now();
        let panicked = panic::catch_unwind(AssertUnwindSafe(f)).is_err();
        let took = start.elapsed();
        if panicked || took > self.slow {
            self.failures += 1;
            let path = self.out.join(format!(
                "{}-{:x}-{}.{}",
                self.name,
                self.seed,
                self.cases,
                if panicked { "panic" } else { "slow" }
            ));
            std::fs::write(&path, input).unwrap();
            eprintln!(
                "FUZZ {} case {}: {took:?}, input in {}",
                self.name,
                self.cases,
                path.display()
            );
        }
    }

    /// Logs the run and fails the test if any case failed.
    pub fn finish(self) {
        eprintln!(
            "FUZZ {} seed={:#x} scale={} cases={} failures={}",
            self.name, self.seed, self.scale, self.cases, self.failures
        );
        assert_eq!(
            self.failures,
            0,
            "{}: failing inputs saved in {}",
            self.name,
            self.out.display()
        );
    }
}
