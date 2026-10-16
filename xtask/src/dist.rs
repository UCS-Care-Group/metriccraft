//! `cargo xtask dist [--target T]…`: the release archive for this OS in `target/dist/`
//! (plan M6.1): both binaries, the licences and the third-party licence texts, ≤ 50 MB.
//!
//! On macOS pass `--target aarch64-apple-darwin --target x86_64-apple-darwin` for a universal
//! `.app`. Third-party licences come from `cargo about` (`about.toml`, `about.hbs`).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, cargo};

/// Spec N-8: "Installer ≤ 50 MB".
const MAX_BYTES: u64 = 50_000_000;
const BINS: &[&str] = &["metriccraft", "metriccraft-cli"];
const DOCS: &[&str] = &[
    "LICENSE-MIT",
    "LICENSE-APACHE",
    "NOTICE",
    "ATTRIBUTION.md",
    "README.md",
];

pub fn run(args: &[String]) -> Result {
    let targets: Vec<&str> = args
        .windows(2)
        .filter(|w| w.first().is_some_and(|a| a == "--target"))
        .filter_map(|w| w.get(1).map(String::as_str))
        .collect();
    let version = version(
        std::env::var("GITHUB_REF_NAME").ok().as_deref(),
        env!("CARGO_PKG_VERSION"),
    )?;
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let arch = if targets.len() > 1 {
        "universal"
    } else {
        targets
            .first()
            .and_then(|t| t.split('-').next())
            .unwrap_or(std::env::consts::ARCH)
    };
    let name = format!("metriccraft-{version}-{os}-{arch}");
    let target_dir = PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap_or("target".into()));
    let dist = target_dir.join("dist");
    let stage = dist.join(&name);
    if stage.exists() {
        std::fs::remove_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;
    }
    std::fs::create_dir_all(&stage).map_err(|e| format!("{}: {e}", stage.display()))?;

    let mut build = vec!["build", "--release", "--locked"];
    for b in BINS {
        build.extend(["-p", b]);
    }
    let release_dirs: Vec<PathBuf> = if targets.is_empty() {
        cargo(&build)?;
        vec![target_dir.join("release")]
    } else {
        targets
            .iter()
            .map(|t| {
                let mut a = build.clone();
                a.extend(["--target", t]);
                cargo(&a).map(|()| target_dir.join(t).join("release"))
            })
            .collect::<Result<_>>()?
    };

    let exe = std::env::consts::EXE_SUFFIX;
    for bin in BINS {
        let dest = if os == "macos" && *bin == "metriccraft" {
            let contents = stage.join("MetricCraft.app").join("Contents");
            mkdir(&contents.join("MacOS"))?;
            write(&contents.join("Info.plist"), &info_plist(&version))?;
            contents.join("MacOS").join(bin)
        } else {
            stage.join(format!("{bin}{exe}"))
        };
        let built: Vec<PathBuf> = release_dirs
            .iter()
            .map(|d| d.join(format!("{bin}{exe}")))
            .collect();
        match built.as_slice() {
            [one] => copy(one, &dest)?,
            many => {
                let mut lipo = Command::new("lipo");
                lipo.arg("-create").args(many).arg("-output").arg(&dest);
                status(&mut lipo)?;
            }
        }
    }
    for doc in DOCS {
        copy(Path::new(doc), &stage.join(doc))?;
    }
    // Licence texts of the bundled fonts (ATTRIBUTION.md): Inter, and those egui embeds.
    mkdir(&stage.join("licenses"))?;
    copy(
        &Path::new("crates")
            .join("visuals")
            .join("fonts")
            .join("OFL.txt"),
        &stage.join("licenses").join("Inter-OFL.txt"),
    )?;
    let fonts = Path::new("licenses").join("egui-default-fonts");
    mkdir(&stage.join(&fonts))?;
    for f in std::fs::read_dir(&fonts).map_err(|e| format!("{}: {e}", fonts.display()))? {
        let f = f.map_err(|e| format!("{}: {e}", fonts.display()))?;
        copy(&f.path(), &stage.join(&fonts).join(f.file_name()))?;
    }
    status(
        Command::new(std::env::var("CARGO").unwrap_or("cargo".into())).args([
            "about",
            "generate",
            "--locked",
            "about.hbs",
            "-o",
            &stage.join("THIRD-PARTY-LICENSES.html").to_string_lossy(),
        ]),
    )?;

    // bsdtar (Windows' System32 tar, macOS tar) writes zip by suffix and keeps exec bits.
    let archive = if os == "linux" {
        format!("{name}.tar.gz")
    } else {
        format!("{name}.zip")
    };
    let path = dist.join(&archive);
    let mut tar = Command::new("tar");
    tar.current_dir(&dist);
    if os == "linux" {
        tar.args(["-czf", &archive, &name]);
    } else {
        tar.args(["-a", "-cf", &archive, &name]);
    }
    status(&mut tar)?;
    let size = std::fs::metadata(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    println!("{}: {:.1} MB", path.display(), size as f64 / 1e6);
    if size > MAX_BYTES {
        return Err(format!(
            "{archive} is {size} bytes, over {MAX_BYTES} (spec N-8)"
        ));
    }
    Ok(())
}

/// The version to name the archive: the Cargo version, which a release tag must match
/// (`v0.1.0-preview.1` ⇔ `version = "0.1.0-preview.1"`) so the binaries report what the tag says.
fn version(tag: Option<&str>, cargo: &str) -> Result<String> {
    match tag.and_then(|t| t.strip_prefix('v')) {
        Some(v) if v != cargo => Err(format!(
            "tag v{v} does not match the workspace version {cargo}; bump Cargo.toml first"
        )),
        _ => Ok(cargo.to_string()),
    }
}

fn info_plist(version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>MetricCraft</string>
  <key>CFBundleDisplayName</key><string>MetricCraft</string>
  <key>CFBundleIdentifier</key><string>org.metriccraft.MetricCraft</string>
  <key>CFBundleExecutable</key><string>metriccraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>CFBundleVersion</key><string>{version}</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
"#
    )
}

fn status(cmd: &mut Command) -> Result {
    let s = cmd.status().map_err(|e| format!("running {cmd:?}: {e}"))?;
    if s.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} failed"))
    }
}

fn mkdir(p: &Path) -> Result {
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, s: &str) -> Result {
    std::fs::write(p, s).map_err(|e| format!("{}: {e}", p.display()))
}

fn copy(from: &Path, to: &Path) -> Result {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("copying {} to {}: {e}", from.display(), to.display()))
}

#[cfg(test)]
mod tests {
    use super::version;

    #[test]
    fn a_release_tag_must_match_the_cargo_version() {
        assert_eq!(version(None, "0.1.0").as_deref(), Ok("0.1.0"));
        assert_eq!(version(Some("main"), "0.1.0").as_deref(), Ok("0.1.0"));
        assert_eq!(
            version(Some("v0.1.0-preview.1"), "0.1.0-preview.1").as_deref(),
            Ok("0.1.0-preview.1")
        );
        assert!(version(Some("v0.1.0"), "0.0.0").is_err());
    }
}
