//! Engine benchmarks. Run via `cargo xtask bench`; prints `BENCH <name> <ns per iteration>`.

use std::hint::black_box;
use std::time::{Duration, Instant};

use opendash_engine::App;
use serde_json::Value;

fn main() {
    let mut app = App::new();
    bench("engine/dispatch_unknown", || {
        let _ = black_box(app.run(black_box("no.such.command"), &Value::Null));
    });
}

// ponytail: copied per bench file until a second crate has benches; then move it to a shared
// dev-dependency crate.
/// Median of 15 samples, each sized to run for about 10 ms.
fn bench(name: &str, mut f: impl FnMut()) {
    let mut iters: u32 = 1;
    loop {
        let start = Instant::now();
        for _ in 0..iters {
            f();
        }
        if start.elapsed() >= Duration::from_millis(10) || iters >= 1 << 30 {
            break;
        }
        iters *= 2;
    }
    let mut samples: Vec<f64> = (0..15)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..iters {
                f();
            }
            start.elapsed().as_nanos() as f64 / f64::from(iters)
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    if let Some(median) = samples.get(samples.len() / 2) {
        println!("BENCH {name} {median:.1}");
    }
}
