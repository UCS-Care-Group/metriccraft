//! Snapshot tests (ADR 0007): a Scene's text form is compared with a committed file; PNGs are
//! written only for eyeballing, to the gitignored `observations/snapshots/`.
//!
//! - `ODASH_SNAPSHOT_UPDATE=1` rewrites the committed text instead of comparing.
//! - `ODASH_SNAPSHOT_PNG=<dir>` also renders each checked Scene to `<dir>/<name>.png`.
//!
//! `cargo xtask snapshot [--update]` sets both for every crate's snapshot tests: those whose
//! name has `snapshot` in it (its filter). [`check`] fails in a test named otherwise, so none is
//! left out.

use std::path::Path;

use crate::draw::Scene;

/// Compares `scene` with `<dir>/<name>.txt`. A missing file is recorded and reported as a
/// failure, so a new snapshot is always looked at once.
pub fn check(dir: &Path, name: &str, scene: &Scene) -> Result<(), String> {
    // The test harness names each test's thread after the test.
    if let Some(test) = std::thread::current().name()
        && test != "main"
        && !test.contains("snapshot")
    {
        return Err(format!(
            "test `{test}` checks snapshot {name}: put `snapshot` in its name so \
             `cargo xtask snapshot` runs it"
        ));
    }
    let text = scene.to_text();
    let file = dir.join(format!("{name}.txt"));
    #[cfg(feature = "png")]
    if let Some(png_dir) = std::env::var_os("ODASH_SNAPSHOT_PNG") {
        let png_dir = Path::new(&png_dir);
        let bytes = crate::png::render(scene, 2.0, crate::Theme::light().background)?;
        std::fs::create_dir_all(png_dir).map_err(|e| e.to_string())?;
        std::fs::write(png_dir.join(format!("{name}.png")), bytes).map_err(|e| e.to_string())?;
    }
    let update = std::env::var_os("ODASH_SNAPSHOT_UPDATE").is_some_and(|v| v == "1");
    match std::fs::read_to_string(&file) {
        Ok(old) if old == text => Ok(()),
        Ok(_) if update => write(&file, &text),
        Ok(old) => Err(format!(
            "snapshot {name} changed (run `cargo xtask snapshot --update` if intended):\n{}",
            diff(&old, &text)
        )),
        Err(_) => {
            write(&file, &text)?;
            if update {
                Ok(())
            } else {
                Err(format!(
                    "new snapshot {name} recorded at {}",
                    file.display()
                ))
            }
        }
    }
}

fn write(file: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(file, text).map_err(|e| e.to_string())
}

/// The first differing lines, for the failure message.
fn diff(old: &str, new: &str) -> String {
    let (o, n): (Vec<_>, Vec<_>) = (old.lines().collect(), new.lines().collect());
    let mut out = String::new();
    for i in 0..o.len().max(n.len()) {
        let (a, b) = (o.get(i), n.get(i));
        if a != b {
            out.push_str(&format!(
                "  line {}:\n  - {}\n  + {}\n",
                i + 1,
                a.unwrap_or(&""),
                b.unwrap_or(&"")
            ));
            if out.lines().count() > 30 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::{Align, Cmd, Weight};
    use crate::theme::Theme;

    /// `cargo xtask snapshot` filters tests by `snapshot` in their name: a check in a test
    /// named otherwise fails, so it can't be left out of `--update`.
    #[test]
    fn checks_only_in_tests_named_snapshot() {
        let dir = std::env::temp_dir().join(format!("odash-snapshot-guard-{}", std::process::id()));
        let run = |name: &str| {
            let dir = dir.clone();
            std::thread::Builder::new()
                .name(name.into())
                .spawn(move || check(&dir, "snapshot_guard", &Scene::new(10.0, 10.0)))
                .unwrap()
                .join()
                .unwrap()
        };
        let e = run("layout::chart_tests::line_years").unwrap_err();
        assert!(e.contains("put `snapshot` in its name"), "{e}");
        // Named for xtask: it gets as far as recording the new snapshot (an error unless
        // `--update` is on).
        if let Err(e) = run("layout::chart_tests::line_years_snapshot") {
            assert!(e.starts_with("new snapshot snapshot_guard"), "{e}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A sample of every command kind, for eyeballing the renderer.
    #[test]
    fn sample_scene() {
        let th = Theme::light();
        let mut s = Scene::new(240.0, 120.0);
        s.push(Cmd::Text {
            x: 8.0,
            y: 20.0,
            text: "Sales by Region".into(),
            size: th.title_size,
            weight: Weight::Bold,
            color: th.text,
            align: Align::Start,
            tag: None,
        });
        for (i, h) in [60.0, 35.0, 80.0].into_iter().enumerate() {
            let x = 30.0 + 60.0 * i as f32;
            s.push(Cmd::Rect {
                x,
                y: 110.0 - h,
                w: 40.0,
                h,
                fill: Some(th.series(i)),
                stroke: None,
                tag: Some(crate::Tag {
                    series: 0,
                    point: i as u32,
                }),
            });
        }
        s.push(Cmd::Line {
            x1: 20.0,
            y1: 110.0,
            x2: 230.0,
            y2: 110.0,
            color: th.axis,
            width: 1.0,
        });
        s.push(Cmd::Path {
            points: vec![(50.0, 60.0), (110.0, 85.0), (170.0, 40.0)],
            color: th.text,
            width: 2.0,
            tag: None,
        });
        s.push(Cmd::PushClip {
            x: 150.0,
            y: 0.0,
            w: 90.0,
            h: 30.0,
        });
        s.push(Cmd::Text {
            x: 232.0,
            y: 20.0,
            text: "£1.2M clipped text".into(),
            size: th.label_size,
            weight: Weight::Regular,
            color: th.text_muted,
            align: Align::End,
            tag: None,
        });
        s.push(Cmd::PopClip);
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots");
        check(&dir, "sample", &s).unwrap();
    }
}
