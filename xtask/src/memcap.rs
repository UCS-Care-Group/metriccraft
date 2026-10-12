//! Memory cap for test processes (`cargo xtask test`).
//!
//! A runaway test must fail on its own, not OOM-kill the host. `cargo test` runs every test
//! binary through the target's `runner`; we point that runner at util-linux
//! `prlimit --as=<bytes> --`, so only the test executables get an `RLIMIT_AS` cap — rustc and
//! the linker (which reserve large address space) are untouched. Doctests run through rustdoc and
//! may stay uncapped on toolchains that don't pass the runner to them.

use std::process::Command;

/// Default per-test-process address-space cap, in MiB (`ODASH_TEST_MEM_MB` overrides).
const DEFAULT_MB: u64 = 4096;

/// Environment variable `(name, value)` that makes cargo run test binaries under the cap, or
/// `None` (with a printed notice) when the cap can't be applied on this host.
pub fn runner_env() -> Option<(String, String)> {
    if !cfg!(target_os = "linux") {
        eprintln!("note: test memory cap skipped (Linux only)");
        return None;
    }
    let mb = match std::env::var("ODASH_TEST_MEM_MB") {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(mb) if mb > 0 => mb,
            _ => {
                eprintln!(
                    "warning: ODASH_TEST_MEM_MB={v:?} is not a positive integer; using {DEFAULT_MB}"
                );
                DEFAULT_MB
            }
        },
        Err(_) => DEFAULT_MB,
    };
    let prlimit_ok = Command::new("prlimit")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !prlimit_ok {
        eprintln!("warning: `prlimit` (util-linux) not found; tests run WITHOUT a memory cap");
        return None;
    }
    let Some(triple) = host_triple() else {
        eprintln!("warning: no host triple from `rustc -vV`; tests run WITHOUT a memory cap");
        return None;
    };
    let bytes = mb.saturating_mul(1024 * 1024);
    let name = format!(
        "CARGO_TARGET_{}_RUNNER",
        triple.to_uppercase().replace(['-', '.'], "_")
    );
    eprintln!("note: test processes capped at {mb} MiB address space (ODASH_TEST_MEM_MB)");
    Some((name, format!("prlimit --as={bytes} --")))
}

/// The host target triple, from `rustc -vV`.
fn host_triple() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = Command::new(rustc).arg("-vV").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_string())
}

#[cfg(test)]
mod tests {
    /// Self-check for the cap, ignored by default. Run capped, it must FAIL (abort on allocation
    /// failure): `ODASH_TEST_MEM_MB=512 cargo xtask test -p xtask -- --ignored memcap`.
    /// Uncapped it passes (the zeroed allocation is lazy and never touched).
    #[test]
    #[ignore = "demonstrates the test memory cap; expected to fail when run capped"]
    fn memcap_runaway_allocation_is_stopped() {
        let v = vec![0u8; 8 << 30];
        assert_eq!(std::hint::black_box(&v).len(), 8 << 30);
    }
}
