//! Optional `CRAFT_FONTS_DIR` discovery for bundled Hebrew font fallback without committing binaries.

use std::path::{Path, PathBuf};

const FONT_NAME: &str = "NotoSansHebrew-Regular.ttf";

fn absolutize(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_hebrew_craft_font)");
    println!("cargo::rerun-if-env-changed=CRAFT_FONTS_DIR");

    let Ok(dir) = std::env::var("CRAFT_FONTS_DIR") else {
        return;
    };
    let dir = dir.trim();
    if dir.is_empty() {
        return;
    }
    let abs_dir = absolutize(Path::new(dir));
    println!("cargo::rerun-if-changed={}", abs_dir.display());
    let font_path = abs_dir.join(FONT_NAME);
    if font_path.is_file() {
        let abs = std::fs::canonicalize(&font_path).unwrap_or(font_path);
        println!("cargo::rerun-if-changed={}", abs.display());
        println!("cargo::rustc-env=HEBREW_CRAFT_FONT_PATH={}", abs.display());
        println!("cargo::rustc-cfg=has_hebrew_craft_font");
    }
}
