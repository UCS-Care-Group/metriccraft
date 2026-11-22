//! Localization (`en` / `he`), BCP 47 tag normalization, and visual RTL helpers for egui 0.36.

use std::sync::{OnceLock, RwLock};

use metriccraft_engine::Command;
use metriccraft_engine::param::Kind;

use crate::strings::Str;

/// Supported UI languages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lang {
    #[default]
    English,
    Hebrew,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::English, Lang::Hebrew];

    /// Whether a Hebrew-capable fallback font was found and registered for this build.
    /// Always `false` on WASM so the web UI never falls back to missing-glyph boxes.
    pub fn hebrew_font_available() -> bool {
        crate::hebrew_font_available()
    }

    /// Languages selectable in the UI for this build target.
    pub fn available() -> &'static [Lang] {
        if Self::hebrew_font_available() {
            &Self::ALL
        } else {
            &[Lang::English]
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Lang::English => "en",
            Lang::Hebrew => "he",
        }
    }

    pub const fn display_name(self) -> Str {
        match self {
            Lang::English => Str::LangEnglish,
            Lang::Hebrew => Str::LangHebrew,
        }
    }

    pub const fn is_rtl(self) -> bool {
        matches!(self, Lang::Hebrew)
    }

    /// Parses a BCP 47 / POSIX locale tag (normalizing legacy `iw` to Hebrew and rejecting
    /// private-use `x-*` subtags).
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let clean = strip_posix_suffix(tag)
            .replace('_', "-")
            .to_ascii_lowercase();
        let mut parts = clean.split('-').filter(|s| !s.is_empty());
        let primary = parts.next()?;
        if primary == "x" {
            return None;
        }
        if let Some(script) = script_subtag(&mut parts) {
            if script == "hebr" {
                return Some(Lang::Hebrew);
            }
            if script == "latn" && (primary == "he" || primary == "iw") {
                return None;
            }
        }
        match primary {
            "he" | "iw" => Some(Lang::Hebrew),
            "en" => Some(Lang::English),
            _ => None,
        }
    }
}

fn strip_posix_suffix(tag: &str) -> &str {
    tag.trim().split(['.', '@']).next().unwrap_or_default()
}

/// Returns a 4-letter ISO 15924 script subtag only when it appears in its valid RFC 5646
/// position (before any singleton extension such as `-x-` or `-u-`).
fn script_subtag<'a>(parts: &mut impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let mut extlangs = 0usize;
    for sub in parts {
        if sub.len() == 1 {
            return None;
        }
        if sub.len() == 4 && sub.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Some(sub);
        }
        if extlangs < 3 && sub.len() == 3 && sub.bytes().all(|b| b.is_ascii_alphabetic()) {
            extlangs += 1;
            continue;
        }
        return None;
    }
    None
}

/// Whether `code` names a right-to-left script or language (accepting legacy `iw`, enforcing RFC
/// 5646 script position, and ignoring private-use `x-*` subtags).
pub fn is_rtl_code(code: &str) -> bool {
    let clean = strip_posix_suffix(code)
        .replace('_', "-")
        .to_ascii_lowercase();
    let mut parts = clean.split('-').filter(|s| !s.is_empty());
    let Some(primary) = parts.next() else {
        return false;
    };
    if primary == "x" {
        return false;
    }
    if let Some(script) = script_subtag(&mut parts) {
        return matches!(
            script,
            "arab"
                | "hebr"
                | "syrc"
                | "thaa"
                | "nkoo"
                | "adlm"
                | "mend"
                | "rohg"
                | "samr"
                | "mand"
                | "yezi"
        );
    }
    matches!(
        primary,
        "ar" | "arc"
            | "ckb"
            | "dv"
            | "fa"
            | "he"
            | "iw"
            | "ji"
            | "ps"
            | "sd"
            | "syr"
            | "ug"
            | "ur"
            | "yi"
    )
}

fn current() -> &'static RwLock<Lang> {
    static CUR: OnceLock<RwLock<Lang>> = OnceLock::new();
    CUR.get_or_init(|| RwLock::new(Lang::English))
}

pub fn set_language(lang: Lang) {
    if let Ok(mut guard) = current().write() {
        *guard = lang;
    }
}

pub fn language() -> Lang {
    current().read().map(|g| *g).unwrap_or_default()
}

pub fn is_rtl() -> bool {
    language().is_rtl()
}

/// Logical command label in `lang` (for palette matching and non-egui native surfaces).
/// Every registered command in `COMMANDS` is verified to have a Hebrew entry in tests;
/// `c.label` serves only as a panic-free fallback for unregistered commands.
pub fn command_label_for(lang: Lang, c: &Command) -> &'static str {
    if lang == Lang::English {
        return c.label;
    }
    hebrew_command_label(c.id).unwrap_or(c.label)
}

/// Display command label in `lang` (visually reordered when RTL for egui 0.36).
pub fn command_display_for(lang: Lang, c: &Command) -> String {
    let logical = command_label_for(lang, c);
    if lang.is_rtl() {
        visual_rtl(logical)
    } else {
        logical.to_string()
    }
}

pub fn command_label(c: &Command) -> &'static str {
    command_label_for(language(), c)
}

pub fn command_display(c: &Command) -> String {
    command_display_for(language(), c)
}

/// Translates a ribbon tab or group segment.
pub fn ribbon_segment_for(lang: Lang, segment: &str) -> &'static str {
    if lang == Lang::English {
        return match segment {
            "File" => "File",
            "Home" => "Home",
            "Insert" => "Insert",
            "Modeling" => "Modeling",
            "View" => "View",
            "Help" => "Help",
            "Column tools" => "Column tools",
            "Transform" => "Transform",
            "Power Query Editor: Home" => "Power Query Editor: Home",
            "Data" => "Data",
            "Queries" => "Queries",
            "Close" => "Close",
            "Table" => "Table",
            "Any Column" => "Any Column",
            "Manage Columns" => "Manage Columns",
            "Reduce Rows" => "Reduce Rows",
            "Combine" => "Combine",
            "Formatting" => "Formatting",
            "Properties" => "Properties",
            "Sort" => "Sort",
            "Calculations" => "Calculations",
            "Relationships" => "Relationships",
            "Pages" => "Pages",
            "Visuals" => "Visuals",
            _ => "",
        };
    }
    match segment {
        "File" => "קובץ",
        "Home" => "בית",
        "Insert" => "הוספה",
        "Modeling" => "מידול",
        "View" => "תצוגה",
        "Help" => "עזרה",
        "Column tools" => "כלי עמודה",
        "Transform" => "המרה",
        "Power Query Editor: Home" => "עורך Power Query: בית",
        "Data" => "נתונים",
        "Queries" => "שאילתות",
        "Close" => "סגירה",
        "Table" => "טבלה",
        "Any Column" => "כל עמודה",
        "Manage Columns" => "ניהול עמודות",
        "Reduce Rows" => "צמצום שורות",
        "Combine" => "מיזוג",
        "Formatting" => "עיצוב",
        "Properties" => "מאפיינים",
        "Sort" => "מיון",
        "Calculations" => "חישובים",
        "Relationships" => "קשרים",
        "Pages" => "עמודים",
        "Visuals" => "תצוגות",
        _ => "",
    }
}

/// Translates a ribbon group name in logical order for the active language.
pub fn ribbon_group_label(group: &str) -> &str {
    let seg = ribbon_segment_for(language(), group);
    if seg.is_empty() { group } else { seg }
}

pub fn ribbon_group_display(group: &str) -> String {
    let logical = ribbon_group_label(group);
    if is_rtl() {
        visual_rtl(logical)
    } else {
        logical.to_string()
    }
}

/// Translates a full `"Tab > Group"` ribbon path in logical order (`lang`) or visual RTL.
pub fn ribbon_path_logical(lang: Lang, path: &str) -> String {
    if lang == Lang::English || path.is_empty() {
        return path.to_string();
    }
    path.split(" > ")
        .map(|seg| {
            let t = ribbon_segment_for(lang, seg);
            if t.is_empty() { seg } else { t }
        })
        .collect::<Vec<_>>()
        .join(" > ")
}

pub fn ribbon_path_display(path: &str) -> String {
    let lang = language();
    let logical = ribbon_path_logical(lang, path);
    if lang.is_rtl() {
        visual_rtl(&logical)
    } else {
        logical
    }
}

pub fn kind_word(kind: Kind) -> String {
    let lang = language();
    if lang == Lang::English {
        return kind.word();
    }
    let he = match kind {
        Kind::Text => "טקסט",
        Kind::Choice(words) => return visual_rtl(&words.join(" | ")),
        Kind::Integer => "מספר שלם",
        Kind::Number => "מספר",
        Kind::Bool => "ערך בוליאני",
        Kind::Texts => "רשימת טקסטים",
        Kind::Table => "טבלה",
        Kind::Column => "עמודה",
        Kind::Measure => "מדד",
        Kind::Query => "שאילתה",
        Kind::Page => "עמוד",
        Kind::Visual => "תצוגה",
        Kind::Path => "נתיב",
        Kind::Field => "שדה",
        Kind::Json => "JSON",
    };
    visual_rtl(he)
}

pub fn hebrew_command_label(id: &str) -> Option<&'static str> {
    Some(match id {
        "edit.undo" => "ביטול",
        "edit.redo" => "ביצוע חוזר",
        "file.new" => "חדש",
        "file.open" => "פתיחה",
        "file.save" => "שמירה",
        "file.saveAs" => "שמירה בשם",
        "file.remapSource" => "מיפוי מחדש של נתיב מקור",
        "file.recover" => "שחזור",
        "file.discardRecovery" => "השלך",
        "home.getData" => "ייבוא נתונים",
        "home.refresh" => "רענון",
        "home.closeApply" => "סגירה והחלה",
        "transform.useFirstRowAsHeaders" => "השתמש בשורה הראשונה ככותרות",
        "transform.changeType" => "סוג נתונים",
        "transform.renameColumn" => "שינוי שם",
        "home.removeColumns" => "הסרת עמודות",
        "transform.reorderColumns" => "העברה",
        "transform.filterRows" => "סינון שורות",
        "home.mergeQueries" => "מיזוג שאילתות",
        "query.deleteStep" => "מחיקה",
        "query.enableLoad" => "הפעל טעינה",
        "query.rename" => "שינוי שם",
        "query.delete" => "מחיקה",
        "query.duplicate" => "שכפול",
        "columnTools.dataType" => "סוג נתונים",
        "columnTools.format" => "עיצוב",
        "columnTools.summarization" => "סיכום",
        "columnTools.sortByColumn" => "מיון לפי עמודה",
        "model.setHidden" => "מוסתר",
        "model.setDescription" => "תיאור",
        "model.moveTable" => "העברת טבלה",
        "modeling.renameTable" => "שינוי שם",
        "modeling.renameColumn" => "שינוי שם",
        "modeling.deleteTable" => "מחיקה מהמודל",
        "modeling.newMeasure" => "מדד חדש",
        "modeling.newColumn" => "עמודה חדשה",
        "modeling.editExpression" => "עריכת נוסחה",
        "modeling.delete" => "מחיקה מהמודל",
        "modeling.renameMeasure" => "שינוי שם",
        "modeling.newRelationship" => "קשר חדש",
        "modeling.editRelationship" => "עריכת קשר",
        "modeling.deleteRelationship" => "מחיקת קשר",
        "modeling.setRelationshipActive" => "פעיל",
        "modeling.autodetectRelationships" => "זיהוי אוטומטי",
        "insert.newPage" => "עמוד חדש",
        "page.setCurrent" => "מעבר לעמוד",
        "page.delete" => "מחיקת עמוד",
        "page.rename" => "שינוי שם עמוד",
        "page.move" => "העברת עמוד",
        "insert.newVisual" => "תצוגה חדשה",
        "visual.delete" => "מחיקה",
        "visual.move" => "העברה",
        "visual.resize" => "שינוי גודל",
        "visual.changeType" => "שינוי סוג תצוגה",
        "visual.addField" => "הוספת שדה",
        "visual.removeField" => "הסרת שדה",
        "visual.moveField" => "העברת שדה",
        "visual.sort" => "מיון לפי",
        "visual.format" => "עיצוב",
        "slicer.select" => "בחירת פריטי מסנן",
        "selection.select" => "בחירת נקודת נתונים",
        "selection.clear" => "ניקוי בחירה",
        "options.autodetectRelationships" => "זיהוי אוטומטי של קשרים חדשים לאחר טעינת נתונים",
        "options.autosaveMinutes" => "שמור מידע לשחזור אוטומטי כל N דקות",
        "dax.evaluate" => "הרצת שאילתת DAX",
        "document.describe" => "תיאור מסמך",
        "page.render" => "רינדור עמוד",
        "batch" => "אצווה",
        _ => return None,
    })
}

fn is_rtl_script(c: char) -> bool {
    matches!(
        c as u32,
        0x0590..=0x05FF | 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF
    )
}

fn is_ltr_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '_' | '+'
                | '-'
                | '.'
                | '/'
                | ':'
                | '~'
                | '\\'
                | '⌘'
                | '⌃'
                | '⇧'
                | '⌥'
                | '&'
                | '='
                | '|'
        )
}

fn mirror_bracket(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        other => other,
    }
}

fn find_path_paren_close(
    chars: &[(usize, char)],
    span_start: usize,
    open_idx: usize,
) -> Option<usize> {
    let mut depth = 0usize;
    let mut k = open_idx;
    while let Some(&(_, ch)) = chars.get(k) {
        if matches!(ch, '\n' | '\r') || is_rtl_script(ch) {
            return None;
        }
        if ch == '(' {
            depth += 1;
        } else if ch == ')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                let has_sep = chars[span_start..=k]
                    .iter()
                    .any(|(_, c)| matches!(c, '/' | '\\'));
                let followed_by_path_part = chars.get(k + 1).is_some_and(|(_, next)| {
                    matches!(next, '.' | '/' | '\\') || next.is_ascii_alphanumeric()
                });
                return (has_sep || followed_by_path_part).then_some(k);
            }
        }
        k += 1;
    }
    None
}

/// Splits one logical line into runs so multi-word RTL reverses word order while keeping LTR tokens
/// (Latin words, numbers, file paths, dot-names, and `\u{2066}`..`\u{2069}` isolate spans) intact.
fn mask_atoms(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let Some(&(start, ch)) = chars.get(i) else {
            break;
        };
        if ch == '\u{2066}' || ch == '\u{2067}' || ch == '\u{2068}' {
            // Preserve isolate content literally without internal reordering; strip markers from output.
            let mut j = i + 1;
            let mut depth = 1usize;
            while j < chars.len() && depth > 0 {
                if let Some(&(_, c)) = chars.get(j) {
                    if c == '\u{2066}' || c == '\u{2067}' || c == '\u{2068}' {
                        depth += 1;
                    } else if c == '\u{2069}' {
                        depth -= 1;
                    }
                }
                j += 1;
            }
            let inner_start = chars.get(i + 1).map_or(line.len(), |(b, _)| *b);
            let inner_end = chars
                .get(j - 1)
                .filter(|(_, c)| *c == '\u{2069}')
                .map_or_else(|| chars.get(j).map_or(line.len(), |(b, _)| *b), |(b, _)| *b);
            if let Some(span) = line.get(inner_start..inner_end) {
                out.push(span.to_string());
            }
            i = j;
            continue;
        }
        if ch.is_whitespace() {
            let mut j = i + 1;
            while j < chars.len() && chars.get(j).is_some_and(|(_, c)| c.is_whitespace()) {
                j += 1;
            }
            let end = chars.get(j).map_or(line.len(), |(b, _)| *b);
            if let Some(seg) = line.get(start..end) {
                out.push(seg.to_string());
            }
            i = j;
            continue;
        }
        if ch.is_ascii_alphanumeric()
            || (matches!(ch, '.' | '/' | '~' | '\\')
                && chars
                    .get(i + 1)
                    .is_some_and(|(_, next)| next.is_ascii_alphanumeric() || *next == '/'))
        {
            // Keep multi-word LTR phrases separated by single spaces or slash/dash as one run,
            // plus digit-flanked grouping commas and balanced parentheses inside file paths.
            let mut j = i + 1;
            while j < chars.len() {
                let Some(&(_, c)) = chars.get(j) else {
                    break;
                };
                if c == '('
                    && let Some(close_j) = find_path_paren_close(&chars, i, j)
                {
                    j = close_j + 1;
                } else if is_ltr_token_char(c)
                    || (c == ','
                        && chars
                            .get(j - 1)
                            .is_some_and(|(_, prev)| prev.is_ascii_digit())
                        && chars
                            .get(j + 1)
                            .is_some_and(|(_, next)| next.is_ascii_digit()))
                    || (c == ' '
                        && (chars
                            .get(j + 1)
                            .is_some_and(|(_, next)| next.is_ascii_alphanumeric())
                            || (chars.get(j + 1).is_some_and(|(_, next)| *next == '(')
                                && find_path_paren_close(&chars, i, j + 1).is_some())))
                {
                    j += 1;
                } else {
                    break;
                }
            }
            // Do not swallow trailing sentence punctuation (`.`, `:`, `-`).
            while j > i + 1
                && chars
                    .get(j - 1)
                    .is_some_and(|(_, c)| matches!(c, '.' | ':' | '-' | '/'))
            {
                j -= 1;
            }
            let end = chars.get(j).map_or(line.len(), |(b, _)| *b);
            if let Some(seg) = line.get(start..end) {
                out.push(seg.to_string());
            }
            i = j;
            continue;
        }
        if is_rtl_script(ch) {
            let mut j = i + 1;
            while j < chars.len()
                && chars
                    .get(j)
                    .is_some_and(|(_, c)| is_rtl_script(*c) || matches!(c, '\'' | '"' | '״' | '׳'))
            {
                j += 1;
            }
            let end = chars.get(j).map_or(line.len(), |(b, _)| *b);
            if let Some(seg) = line.get(start..end) {
                out.push(seg.to_string());
            }
            i = j;
            continue;
        }
        // Single punctuation character.
        let end = chars.get(i + 1).map_or(line.len(), |(b, _)| *b);
        if let Some(seg) = line.get(start..end) {
            out.push(mirror_bracket(ch).to_string());
            let _ = seg;
        }
        i += 1;
    }
    out
}

fn push_visual_rtl_line(line: &str, out: &mut String) {
    for tok in mask_atoms(line).into_iter().rev() {
        out.push_str(&tok);
    }
}

/// Converts a logical RTL string to visual left-to-right display order for egui 0.36. Lines with
/// no RTL script characters are left untouched.
pub fn visual_rtl(logical: &str) -> String {
    if !logical.chars().any(is_rtl_script) {
        return logical.to_string();
    }
    let mut out = String::with_capacity(logical.len());
    for (idx, raw_line) in logical.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let (line, cr) = raw_line
            .strip_suffix('\r')
            .map_or((raw_line, false), |l| (l, true));
        if line.chars().any(is_rtl_script) {
            push_visual_rtl_line(line, &mut out);
        } else {
            out.push_str(line);
        }
        if cr {
            out.push('\r');
        }
    }
    out
}

fn wrap_paragraph_logical<'a>(
    ctx: &egui::Context,
    paragraph: &'a str,
    font_id: &egui::FontId,
    max_width: f32,
) -> Vec<&'a str> {
    if paragraph.is_empty() || !max_width.is_finite() {
        return vec![paragraph];
    }
    let max_w = max_width.max(1.0);
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut in_word = false;
    let mut word_start = 0usize;
    for (idx, ch) in paragraph.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                spans.push((word_start, idx));
                in_word = false;
            }
        } else if !in_word {
            word_start = idx;
            in_word = true;
        }
    }
    if in_word {
        spans.push((word_start, paragraph.len()));
    }
    let Some(&(mut line_start, mut line_end)) = spans.first() else {
        return vec![paragraph];
    };
    let mut rows = Vec::new();
    for &(span_start, span_end) in spans.iter().skip(1) {
        let candidate = &paragraph[line_start..span_end];
        let width = ctx.fonts_mut(|f| {
            f.layout_no_wrap(candidate.to_owned(), font_id.clone(), Default::default())
                .rect
                .width()
        });
        if width <= max_w {
            line_end = span_end;
        } else {
            rows.push(&paragraph[line_start..line_end]);
            line_start = span_start;
            line_end = span_end;
        }
    }
    rows.push(&paragraph[line_start..line_end]);
    rows
}

/// Breaks `logical` text at `max_width` first using original logical substrings, then applies
/// visual RTL reordering per row containing RTL characters.
pub fn wrapped_visual_lines(
    ctx: &egui::Context,
    rtl: bool,
    logical: &str,
    font_id: egui::FontId,
    max_width: f32,
) -> String {
    if !rtl || !logical.chars().any(is_rtl_script) {
        return logical.to_owned();
    }
    let mut out = String::with_capacity(logical.len());
    let mut first_row = true;
    for paragraph in logical.split('\n') {
        for row_logical in wrap_paragraph_logical(ctx, paragraph, &font_id, max_width) {
            if !first_row {
                out.push('\n');
            }
            first_row = false;
            if row_logical.chars().any(is_rtl_script) {
                push_visual_rtl_line(row_logical, &mut out);
            } else {
                out.push_str(row_logical);
            }
        }
    }
    out
}

/// Renders a **logical-order** wrapping label, breaking lines at `ui.available_width()` before
/// applying visual RTL reordering per line when `rtl` is true.
pub fn wrapped_label_for(
    ui: &mut egui::Ui,
    rtl: bool,
    logical: &str,
    style: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Response {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let needs_rtl_wrap = rtl && logical.chars().any(is_rtl_script);
    let shown = if needs_rtl_wrap {
        wrapped_visual_lines(
            ui.ctx(),
            true,
            logical,
            font_id.clone(),
            ui.available_width(),
        )
    } else {
        logical.to_owned()
    };
    let rich = style(egui::RichText::new(shown).font(font_id));
    let response = if needs_rtl_wrap {
        ui.add(egui::Label::new(rich).extend())
    } else {
        ui.add(egui::Label::new(rich).wrap())
    };
    crate::icons::a11y_label(response, egui::WidgetType::Label, logical)
}

/// Renders a **logical-order** wrapping label in the active interface language.
pub fn wrapped_label(
    ui: &mut egui::Ui,
    logical: &str,
    style: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Response {
    wrapped_label_for(ui, is_rtl(), logical, style)
}

/// Renders a **logical-order** wrapping label for user data (query, column, filter values),
/// applying visual RTL reordering whenever `logical` contains RTL script characters even if the
/// interface language is English.
pub fn wrapped_data_label(
    ui: &mut egui::Ui,
    logical: &str,
    style: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Response {
    wrapped_label_for(ui, true, logical, style)
}

/// Renders a **logical-order** wrapping selectable label, breaking lines at the button's
/// available text width before applying visual RTL reordering per line when `rtl` is true.
pub fn wrapped_selectable_label_for(
    ui: &mut egui::Ui,
    rtl: bool,
    selected: bool,
    logical: &str,
    italics: bool,
) -> egui::Response {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let needs_rtl_wrap = rtl && logical.chars().any(is_rtl_script);
    let text_width = (ui.available_width() - 2.0 * ui.spacing().button_padding.x).max(1.0);
    let shown = if needs_rtl_wrap {
        wrapped_visual_lines(ui.ctx(), true, logical, font_id.clone(), text_width)
    } else {
        logical.to_owned()
    };
    let mut rich = egui::RichText::new(shown).font(font_id);
    if italics {
        rich = rich.italics();
    }
    let button = if needs_rtl_wrap {
        egui::Button::selectable(selected, rich).wrap_mode(egui::TextWrapMode::Extend)
    } else {
        egui::Button::selectable(selected, rich)
    };
    crate::icons::a11y_label(ui.add(button), egui::WidgetType::SelectableLabel, logical)
}

/// Renders a **logical-order** wrapping selectable label for user data (queries, columns,
/// measures), applying visual RTL reordering whenever `logical` contains RTL script characters.
pub fn wrapped_selectable_label(
    ui: &mut egui::Ui,
    selected: bool,
    logical: &str,
    italics: bool,
) -> egui::Response {
    wrapped_selectable_label_for(ui, true, selected, logical, italics)
}

/// Renders a single-line [`Str`] label visually reordered for egui 0.36 while preserving
/// logical-order text on its AccessKit label.
pub fn str_label(
    ui: &mut egui::Ui,
    s: Str,
    style: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Response {
    crate::icons::a11y_label(
        ui.label(style(egui::RichText::new(s.show()))),
        egui::WidgetType::Label,
        s.text(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriccraft_engine::COMMANDS;

    #[test]
    fn bcp47_and_legacy_iw_normalization_and_rtl_detection() {
        for tag in ["he", "he-IL", "he_IL.UTF-8", "iw", "iw-IL", "en-Hebr"] {
            assert_eq!(Lang::from_tag(tag), Some(Lang::Hebrew), "{tag}");
            assert!(is_rtl_code(tag), "{tag}");
        }
        for tag in [
            "x-he",
            "en-x-he",
            "en-x-arab",
            "ar-Latn",
            "he-Latn",
            "en-US",
        ] {
            assert!(!is_rtl_code(tag), "{tag}");
        }
        assert!(is_rtl_code("zh-Arab-CN"));
        assert!(!is_rtl_code("en-u-ca-hebrew"));
        assert_eq!(Lang::from_tag("x-he"), None);
        assert_eq!(Lang::from_tag("en-x-he"), Some(Lang::English));
    }

    #[test]
    fn every_command_and_str_has_hebrew_translation() {
        for c in COMMANDS {
            let he = command_label_for(Lang::Hebrew, c);
            assert!(
                !he.is_empty() && he != c.label,
                "missing Hebrew translation for command {} ({})",
                c.id,
                c.label
            );
        }
        for s in Str::ALL {
            let en = s.text_for(Lang::English);
            let he = s.text_for(Lang::Hebrew);
            assert!(!he.is_empty(), "{s:?} has empty Hebrew translation");
            if !matches!(
                s,
                Str::AppName
                    | Str::PowerQueryGroup
                    | Str::LangEnglish
                    | Str::LangHebrew
                    | Str::Percent
            ) {
                assert_ne!(en, he, "{s:?} was not translated");
            }
        }
    }

    #[test]
    fn hebrew_visual_rtl_preserves_ltr_tokens_paths_isolates_and_lines() {
        assert_eq!(Str::TabFile.show_for(Lang::Hebrew), "קובץ");
        assert_eq!(Str::ViewReport.show_for(Lang::Hebrew), "דוח");
        assert_eq!(Str::TabColumnTools.show_for(Lang::Hebrew), "עמודה כלי");
        assert_eq!(
            Str::PageNumber.fill_for(Lang::Hebrew, &["2", "5"]),
            "5 מתוך 2 עמוד"
        );
        assert_eq!(
            Str::RowCount.fill_for(Lang::Hebrew, &["1,234"]),
            "שורות 1,234"
        );
        assert_eq!(
            Str::RecoveryFound.fill_for(Lang::Hebrew, &["/tmp/demo.mcraft"]),
            "/tmp/demo.mcraft :קודמת מהפעלה נשמרה שלא עבודה לשחזר ניתן"
        );
        assert_eq!(
            Str::RecoveryFound.fill_for(Lang::Hebrew, &["/tmp/report(2026).mcraft"]),
            "/tmp/report(2026).mcraft :קודמת מהפעלה נשמרה שלא עבודה לשחזר ניתן"
        );
        assert_eq!(
            Str::RecoveryFound.fill_for(Lang::Hebrew, &["/tmp/Budget (2026).mcraft"]),
            "/tmp/Budget (2026).mcraft :קודמת מהפעלה נשמרה שלא עבודה לשחזר ניתן"
        );
        assert_eq!(
            Str::SaveFirst.fill_for(Lang::Hebrew, &["Budget (2026).mcraft"]),
            "?הסגירה לפני Budget (2026).mcraft-ב שינויים לשמור"
        );
        assert_eq!(
            Str::SaveFirst.fill_logical_for(Lang::Hebrew, &["Budget (2026).mcraft"]),
            "לשמור שינויים ב-Budget (2026).mcraft לפני הסגירה?"
        );
        assert_eq!(
            Str::RecoveryFound.fill_for(Lang::Hebrew, &["\u{2066}foo - אב - bar\u{2069}"]),
            "foo - אב - bar :קודמת מהפעלה נשמרה שלא עבודה לשחזר ניתן"
        );
        assert_eq!(
            visual_rtl("ייצוא (CSV)\n  cargo test\nעוד שורה"),
            "(CSV) ייצוא\n  cargo test\nשורה עוד"
        );
        assert_eq!(
            Str::SaveFirst.fill_for(Lang::Hebrew, &["report.mcraft"]),
            "?הסגירה לפני report.mcraft-ב שינויים לשמור"
        );
        let new_page = COMMANDS
            .iter()
            .find(|c| c.id == "insert.newPage")
            .expect("insert.newPage registered");
        assert_eq!(command_label_for(Lang::Hebrew, new_page), "עמוד חדש");
        assert_eq!(command_display_for(Lang::Hebrew, new_page), "חדש עמוד");
        assert_eq!(visual_rtl("ב-2"), "2-ב");
        assert_eq!(visual_rtl("ייצוא (CSV ו-Parquet)"), "(Parquet-ו CSV) ייצוא");
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let wrapped = wrapped_visual_lines(
            &ctx,
            true,
            "בחר תצוגה על הבד כדי לראות את השדות והמסננים שלה",
            egui::FontId::proportional(13.0),
            120.0,
        );
        let first_row = wrapped.lines().next().unwrap_or_default();
        assert!(first_row.contains("בחר"), "got {wrapped:?}");
        assert!(!first_row.contains("רחב"), "got {wrapped:?}");

        let mut label_rect = egui::Rect::NOTHING;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.allocate_ui(egui::vec2(140.0, 200.0), |ui| {
                    label_rect = wrapped_label_for(
                        ui,
                        true,
                        Str::SelectVisual.text_for(Lang::Hebrew),
                        |r| r,
                    )
                    .rect;
                });
            });
        })
        .textures_delta
        .clear();
        assert!(
            label_rect.height() >= 24.0,
            "expected multi-line wrapped label: {label_rect:?}"
        );

        assert_eq!(
            Str::DeleteStepNamed.fill_for(Lang::Hebrew, &["מכירות 2026"]),
            "2026 מכירות שלב מחק"
        );
        assert_eq!(
            Str::DeleteStepNamed.fill_logical_for(Lang::Hebrew, &["מכירות 2026"]),
            "מחק שלב מכירות 2026"
        );

        let mut selectable_rect = egui::Rect::NOTHING;
        ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.allocate_ui(egui::vec2(120.0, 200.0), |ui| {
                    selectable_rect = wrapped_selectable_label_for(
                        ui,
                        true,
                        false,
                        "שאילתת מכירות שנתית מאוד ארוכה",
                        false,
                    )
                    .rect;
                });
            });
        })
        .textures_delta
        .clear();
        assert!(
            selectable_rect.height() >= 24.0,
            "expected multi-line wrapped selectable label: {selectable_rect:?}"
        );
    }
}
