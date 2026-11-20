//! The string catalogue: every word the UI shows that isn't a command's own label
//! (those are keyed by command id in [`crate::i18n`]). Supports English and Hebrew;
//! `cargo xtask ci` rejects string literals passed to widgets anywhere else in this crate.

use crate::i18n::{self, Lang};

/// A UI string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Str {
    AppName,
    Untitled,
    // Ribbon tabs (the names commands' `ribbon` paths start with).
    TabFile,
    TabHome,
    TabInsert,
    TabModeling,
    TabView,
    TabHelp,
    TabColumnTools,
    TabTransform,
    // Views and Panes (glossary).
    ViewReport,
    ViewTable,
    ViewModel,
    PaneFilters,
    PaneVisualizations,
    PaneData,
    // View tab.
    AppTheme,
    ThemeLight,
    ThemeDark,
    Language,
    LangEnglish,
    LangHebrew,
    // Status bar and toasts.
    Ready,
    Working,
    Unsaved,
    Dismiss,
    // Command palette and forms.
    PaletteHint,
    PaletteNone,
    Run,
    Cancel,
    Optional,
    Browse,
    MetricCraftFiles,
    DataFiles,
    ParamIntError,
    ParamNumError,
    ParamJsonError,
    // Recovery and closing.
    RecoveryFound,
    RecoveryUnsaved,
    Recover,
    Discard,
    SaveFirst,
    Save,
    DontSave,
    // Views not built yet show what they will hold.
    NoPages,
    NoTables,
    TableViewHint,
    SelectVisual,
    PageNumber,
    Shortcut,
    // The canvas and the Visualizations pane.
    VisualOf,
    VisTable,
    VisMatrix,
    VisCard,
    VisBar,
    VisColumn,
    VisLine,
    VisSlicer,
    WellValues,
    WellRows,
    WellColumns,
    WellAxis,
    WellLegend,
    WellField,
    WellEmpty,
    Remove,
    RemoveField,
    ClearSelection,
    DeleteStepNamed,
    Percent,
    NoFilters,
    FitPage,
    FitWidth,
    RowCount,
    RowsRead,
    UndoLabel,
    CellError,
    TransformData,
    PowerQueryGroup,
    RegionRibbon,
    RegionPanes,
    Close,
    QueryEditor,
    Queries,
    AppliedSteps,
    SourceStep,
    DeleteStep,
    QueryM,
    NoQueries,
    PreviewOf,
    LoadingPreview,
    TableOf,
    FormulaShape,
    NewMeasureName,
    NewColumnName,
    Commit,
    FormulaMeasure,
    FormulaColumn,
    ColumnOf,
    RowScroll,
    ColumnScroll,
    Zoom,
}

impl Str {
    /// Complete list of `Str` variants for test-time translation coverage checks.
    /// Keep in sync with `Str` (exhaustiveness is enforced by `Str::text_for` and `tests::str_all_has_no_duplicates`).
    #[cfg(test)]
    pub const ALL: [Str; 101] = [
        Str::AppName,
        Str::Untitled,
        Str::TabFile,
        Str::TabHome,
        Str::TabInsert,
        Str::TabModeling,
        Str::TabView,
        Str::TabHelp,
        Str::TabColumnTools,
        Str::TabTransform,
        Str::ViewReport,
        Str::ViewTable,
        Str::ViewModel,
        Str::PaneFilters,
        Str::PaneVisualizations,
        Str::PaneData,
        Str::AppTheme,
        Str::ThemeLight,
        Str::ThemeDark,
        Str::Language,
        Str::LangEnglish,
        Str::LangHebrew,
        Str::Ready,
        Str::Working,
        Str::Unsaved,
        Str::Dismiss,
        Str::PaletteHint,
        Str::PaletteNone,
        Str::Run,
        Str::Cancel,
        Str::Optional,
        Str::Browse,
        Str::MetricCraftFiles,
        Str::DataFiles,
        Str::ParamIntError,
        Str::ParamNumError,
        Str::ParamJsonError,
        Str::RecoveryFound,
        Str::RecoveryUnsaved,
        Str::Recover,
        Str::Discard,
        Str::SaveFirst,
        Str::Save,
        Str::DontSave,
        Str::NoPages,
        Str::NoTables,
        Str::TableViewHint,
        Str::SelectVisual,
        Str::PageNumber,
        Str::Shortcut,
        Str::VisualOf,
        Str::VisTable,
        Str::VisMatrix,
        Str::VisCard,
        Str::VisBar,
        Str::VisColumn,
        Str::VisLine,
        Str::VisSlicer,
        Str::WellValues,
        Str::WellRows,
        Str::WellColumns,
        Str::WellAxis,
        Str::WellLegend,
        Str::WellField,
        Str::WellEmpty,
        Str::Remove,
        Str::RemoveField,
        Str::ClearSelection,
        Str::DeleteStepNamed,
        Str::Percent,
        Str::NoFilters,
        Str::FitPage,
        Str::FitWidth,
        Str::RowCount,
        Str::RowsRead,
        Str::UndoLabel,
        Str::CellError,
        Str::TransformData,
        Str::PowerQueryGroup,
        Str::RegionRibbon,
        Str::RegionPanes,
        Str::Close,
        Str::QueryEditor,
        Str::Queries,
        Str::AppliedSteps,
        Str::SourceStep,
        Str::DeleteStep,
        Str::QueryM,
        Str::NoQueries,
        Str::PreviewOf,
        Str::LoadingPreview,
        Str::TableOf,
        Str::FormulaShape,
        Str::NewMeasureName,
        Str::NewColumnName,
        Str::Commit,
        Str::FormulaMeasure,
        Str::FormulaColumn,
        Str::ColumnOf,
        Str::RowScroll,
        Str::ColumnScroll,
    ];

    /// Logical string in `lang` (for native OS controls, AccessKit labels, and placeholder substitution).
    pub fn text_for(self, lang: Lang) -> &'static str {
        match (lang, self) {
            (Lang::English, Str::AppName) => "MetricCraft",
            (Lang::English, Str::Untitled) => "Untitled",
            (Lang::English, Str::TabFile) => "File",
            (Lang::English, Str::TabHome) => "Home",
            (Lang::English, Str::TabInsert) => "Insert",
            (Lang::English, Str::TabModeling) => "Modeling",
            (Lang::English, Str::TabView) => "View",
            (Lang::English, Str::TabHelp) => "Help",
            (Lang::English, Str::TabColumnTools) => "Column tools",
            (Lang::English, Str::TabTransform) => "Transform",
            (Lang::English, Str::ViewReport) => "Report",
            (Lang::English, Str::ViewTable) => "Table",
            (Lang::English, Str::ViewModel) => "Model",
            (Lang::English, Str::PaneFilters) => "Filters",
            (Lang::English, Str::PaneVisualizations) => "Visualizations",
            (Lang::English, Str::PaneData) => "Data",
            (Lang::English, Str::AppTheme) => "App theme",
            (Lang::English, Str::ThemeLight) => "Light",
            (Lang::English, Str::ThemeDark) => "Dark",
            (Lang::English, Str::Language) => "Language",
            (Lang::English, Str::LangEnglish) => "English",
            (Lang::English, Str::LangHebrew) => "עברית",
            (Lang::English, Str::Ready) => "Ready",
            (Lang::English, Str::Working) => "Working: {}",
            (Lang::English, Str::Unsaved) => "Unsaved changes",
            (Lang::English, Str::Dismiss) => "Dismiss",
            (Lang::English, Str::PaletteHint) => "Type a command",
            (Lang::English, Str::PaletteNone) => "No command matches",
            (Lang::English, Str::Run) => "Run",
            (Lang::English, Str::Cancel) => "Cancel",
            (Lang::English, Str::Optional) => "optional",
            (Lang::English, Str::Browse) => "Browse…",
            (Lang::English, Str::MetricCraftFiles) => "MetricCraft projects and packages",
            (Lang::English, Str::DataFiles) => "CSV, Excel and Parquet files",
            (Lang::English, Str::ParamIntError) => "{}: a whole number",
            (Lang::English, Str::ParamNumError) => "{}: a number",
            (Lang::English, Str::ParamJsonError) => "{}: not valid JSON ({})",
            (Lang::English, Str::RecoveryFound) => {
                "Unsaved work from an earlier session can be recovered: {}"
            }
            (Lang::English, Str::RecoveryUnsaved) => "a document never saved",
            (Lang::English, Str::Recover) => "Recover",
            (Lang::English, Str::Discard) => "Discard",
            (Lang::English, Str::SaveFirst) => "Save changes to {} before closing?",
            (Lang::English, Str::Save) => "Save",
            (Lang::English, Str::DontSave) => "Don't save",
            (Lang::English, Str::NoPages) => "No pages yet. Insert a page to start a report.",
            (Lang::English, Str::NoTables) => "No tables yet. Get data to add one.",
            (Lang::English, Str::TableViewHint) => "Pick a table in the Data pane to see its rows.",
            (Lang::English, Str::SelectVisual) => {
                "Select a visual on the canvas to see its fields and filters."
            }
            (Lang::English, Str::PageNumber) => "Page {} of {}",
            (Lang::English, Str::Shortcut) => "Shortcut: {}",
            (Lang::English, Str::VisualOf) => "{} visual",
            (Lang::English, Str::VisTable) => "Table",
            (Lang::English, Str::VisMatrix) => "Matrix",
            (Lang::English, Str::VisCard) => "Card",
            (Lang::English, Str::VisBar) => "Clustered bar chart",
            (Lang::English, Str::VisColumn) => "Clustered column chart",
            (Lang::English, Str::VisLine) => "Line chart",
            (Lang::English, Str::VisSlicer) => "Slicer",
            (Lang::English, Str::WellValues) => "Values",
            (Lang::English, Str::WellRows) => "Rows",
            (Lang::English, Str::WellColumns) => "Columns",
            (Lang::English, Str::WellAxis) => "X-axis",
            (Lang::English, Str::WellLegend) => "Legend",
            (Lang::English, Str::WellField) => "Field",
            (Lang::English, Str::WellEmpty) => "Add data fields here",
            (Lang::English, Str::Remove) => "Remove",
            (Lang::English, Str::RemoveField) => "Remove {}",
            (Lang::English, Str::ClearSelection) => "Clear the {} selection",
            (Lang::English, Str::DeleteStepNamed) => "Delete step {}",
            (Lang::English, Str::Percent) => "{} %",
            (Lang::English, Str::NoFilters) => "No slicer selections on this page.",
            (Lang::English, Str::RowCount) => "{} rows",
            (Lang::English, Str::RowsRead) => "{}: {} rows read",
            (Lang::English, Str::UndoLabel) => "Undo: {}",
            (Lang::English, Str::CellError) => "Error",
            (Lang::English, Str::TransformData) => "Transform data",
            (Lang::English, Str::PowerQueryGroup) => "Power Query",
            (Lang::English, Str::RegionRibbon) => "Ribbon",
            (Lang::English, Str::RegionPanes) => "Panes",
            (Lang::English, Str::Close) => "Close editor",
            (Lang::English, Str::QueryEditor) => "Power Query Editor",
            (Lang::English, Str::Queries) => "Queries",
            (Lang::English, Str::AppliedSteps) => "Applied Steps",
            (Lang::English, Str::SourceStep) => "Source",
            (Lang::English, Str::DeleteStep) => "Delete step",
            (Lang::English, Str::QueryM) => "M (read-only)",
            (Lang::English, Str::NoQueries) => "No queries yet. Get data to add one.",
            (Lang::English, Str::PreviewOf) => "{} preview",
            (Lang::English, Str::LoadingPreview) => "Loading the preview…",
            (Lang::English, Str::TableOf) => "{} table",
            (Lang::English, Str::FormulaShape) => "Write it as Name = expression",
            (Lang::English, Str::NewMeasureName) => "Measure",
            (Lang::English, Str::NewColumnName) => "Column",
            (Lang::English, Str::Commit) => "Commit",
            (Lang::English, Str::FormulaMeasure) => "Measure formula",
            (Lang::English, Str::FormulaColumn) => "Column formula",
            (Lang::English, Str::ColumnOf) => "{} column of {}",
            (Lang::English, Str::RowScroll) => "Scroll rows",
            (Lang::English, Str::ColumnScroll) => "Scroll columns",
            (Lang::English, Str::FitPage) => "Fit to page",
            (Lang::English, Str::FitWidth) => "Fit to width",
            (Lang::English, Str::Zoom) => "Zoom",

            (Lang::Hebrew, Str::AppName) => "MetricCraft",
            (Lang::Hebrew, Str::Untitled) => "ללא שם",
            (Lang::Hebrew, Str::TabFile) => "קובץ",
            (Lang::Hebrew, Str::TabHome) => "בית",
            (Lang::Hebrew, Str::TabInsert) => "הוספה",
            (Lang::Hebrew, Str::TabModeling) => "מידול",
            (Lang::Hebrew, Str::TabView) => "תצוגה",
            (Lang::Hebrew, Str::TabHelp) => "עזרה",
            (Lang::Hebrew, Str::TabColumnTools) => "כלי עמודה",
            (Lang::Hebrew, Str::TabTransform) => "המרה",
            (Lang::Hebrew, Str::ViewReport) => "דוח",
            (Lang::Hebrew, Str::ViewTable) => "טבלה",
            (Lang::Hebrew, Str::ViewModel) => "מודל",
            (Lang::Hebrew, Str::PaneFilters) => "מסננים",
            (Lang::Hebrew, Str::PaneVisualizations) => "תצוגות",
            (Lang::Hebrew, Str::PaneData) => "נתונים",
            (Lang::Hebrew, Str::AppTheme) => "ערכת נושא",
            (Lang::Hebrew, Str::ThemeLight) => "בהיר",
            (Lang::Hebrew, Str::ThemeDark) => "כהה",
            (Lang::Hebrew, Str::Language) => "שפה",
            (Lang::Hebrew, Str::LangEnglish) => "English",
            (Lang::Hebrew, Str::LangHebrew) => "עברית",
            (Lang::Hebrew, Str::Ready) => "מוכן",
            (Lang::Hebrew, Str::Working) => "מעבד: {}",
            (Lang::Hebrew, Str::Unsaved) => "שינויים שלא נשמרו",
            (Lang::Hebrew, Str::Dismiss) => "סגירה",
            (Lang::Hebrew, Str::PaletteHint) => "הקלד פקודה",
            (Lang::Hebrew, Str::PaletteNone) => "אין פקודה תואמת",
            (Lang::Hebrew, Str::Run) => "הרצה",
            (Lang::Hebrew, Str::Cancel) => "ביטול",
            (Lang::Hebrew, Str::Optional) => "אופציונלי",
            (Lang::Hebrew, Str::Browse) => "עיון…",
            (Lang::Hebrew, Str::MetricCraftFiles) => "פרויקטים וחבילות MetricCraft",
            (Lang::Hebrew, Str::DataFiles) => "קבצי CSV, Excel ו-Parquet",
            (Lang::Hebrew, Str::ParamIntError) => "{}: מספר שלם",
            (Lang::Hebrew, Str::ParamNumError) => "{}: מספר",
            (Lang::Hebrew, Str::ParamJsonError) => "{}: אינו JSON תקין ({})",
            (Lang::Hebrew, Str::RecoveryFound) => "ניתן לשחזר עבודה שלא נשמרה מהפעלה קודמת: {}",
            (Lang::Hebrew, Str::RecoveryUnsaved) => "מסמך שמעולם לא נשמר",
            (Lang::Hebrew, Str::Recover) => "שחזור",
            (Lang::Hebrew, Str::Discard) => "השלך",
            (Lang::Hebrew, Str::SaveFirst) => "לשמור שינויים ב-{} לפני הסגירה?",
            (Lang::Hebrew, Str::Save) => "שמירה",
            (Lang::Hebrew, Str::DontSave) => "אל תשמור",
            (Lang::Hebrew, Str::NoPages) => "אין עמודים עדיין. הוסף עמוד כדי להתחיל דוח.",
            (Lang::Hebrew, Str::NoTables) => "אין טבלאות עדיין. יבא נתונים כדי להוסיף טבלה.",
            (Lang::Hebrew, Str::TableViewHint) => "בחר טבלה בחלונית הנתונים כדי לראות את שורותיה.",
            (Lang::Hebrew, Str::SelectVisual) => {
                "בחר תצוגה על הבד כדי לראות את השדות והמסננים שלה."
            }
            (Lang::Hebrew, Str::PageNumber) => "עמוד {} מתוך {}",
            (Lang::Hebrew, Str::Shortcut) => "קיצור דרך: {}",
            (Lang::Hebrew, Str::VisualOf) => "תצוגת {}",
            (Lang::Hebrew, Str::VisTable) => "טבלה",
            (Lang::Hebrew, Str::VisMatrix) => "מטריצה",
            (Lang::Hebrew, Str::VisCard) => "כרטיס",
            (Lang::Hebrew, Str::VisBar) => "תרשים עמודות אופקי מקובץ",
            (Lang::Hebrew, Str::VisColumn) => "תרשים עמודות אנכי מקובץ",
            (Lang::Hebrew, Str::VisLine) => "תרשים קו",
            (Lang::Hebrew, Str::VisSlicer) => "מסנן חיתוך",
            (Lang::Hebrew, Str::WellValues) => "ערכים",
            (Lang::Hebrew, Str::WellRows) => "שורות",
            (Lang::Hebrew, Str::WellColumns) => "עמודות",
            (Lang::Hebrew, Str::WellAxis) => "ציר X",
            (Lang::Hebrew, Str::WellLegend) => "מקרא",
            (Lang::Hebrew, Str::WellField) => "שדה",
            (Lang::Hebrew, Str::WellEmpty) => "הוסף שדות נתונים לכאן",
            (Lang::Hebrew, Str::Remove) => "הסרה",
            (Lang::Hebrew, Str::RemoveField) => "הסרת {}",
            (Lang::Hebrew, Str::ClearSelection) => "נקה את בחירת {}",
            (Lang::Hebrew, Str::DeleteStepNamed) => "מחק שלב {}",
            (Lang::Hebrew, Str::Percent) => "{} %",
            (Lang::Hebrew, Str::NoFilters) => "אין בחירות חיתוך בעמוד זה.",
            (Lang::Hebrew, Str::RowCount) => "{} שורות",
            (Lang::Hebrew, Str::RowsRead) => "{}: {} שורות נקראו",
            (Lang::Hebrew, Str::UndoLabel) => "ביטול: {}",
            (Lang::Hebrew, Str::CellError) => "שגיאה",
            (Lang::Hebrew, Str::TransformData) => "המרת נתונים",
            (Lang::Hebrew, Str::PowerQueryGroup) => "Power Query",
            (Lang::Hebrew, Str::RegionRibbon) => "רצועת כלים",
            (Lang::Hebrew, Str::RegionPanes) => "חלוניות",
            (Lang::Hebrew, Str::Close) => "סגור עורך",
            (Lang::Hebrew, Str::QueryEditor) => "עורך Power Query",
            (Lang::Hebrew, Str::Queries) => "שאילתות",
            (Lang::Hebrew, Str::AppliedSteps) => "שלבים שהוחלו",
            (Lang::Hebrew, Str::SourceStep) => "מקור",
            (Lang::Hebrew, Str::DeleteStep) => "מחק שלב",
            (Lang::Hebrew, Str::QueryM) => "M (קריאה בלבד)",
            (Lang::Hebrew, Str::NoQueries) => "אין שאילתות עדיין. יבא נתונים כדי להוסיף שאילתה.",
            (Lang::Hebrew, Str::PreviewOf) => "תצוגה מקדימה של {}",
            (Lang::Hebrew, Str::LoadingPreview) => "טוען תצוגה מקדימה…",
            (Lang::Hebrew, Str::TableOf) => "טבלת {}",
            (Lang::Hebrew, Str::FormulaShape) => "כתוב בצורה שם = ביטוי",
            (Lang::Hebrew, Str::NewMeasureName) => "מדד",
            (Lang::Hebrew, Str::NewColumnName) => "עמודה",
            (Lang::Hebrew, Str::Commit) => "אישור",
            (Lang::Hebrew, Str::FormulaMeasure) => "נוסחת מדד",
            (Lang::Hebrew, Str::FormulaColumn) => "נוסחת עמודה",
            (Lang::Hebrew, Str::ColumnOf) => "עמודת {} מתוך {}",
            (Lang::Hebrew, Str::RowScroll) => "גלול שורות",
            (Lang::Hebrew, Str::ColumnScroll) => "גלול עמודות",
            (Lang::Hebrew, Str::FitPage) => "התאם לעמוד",
            (Lang::Hebrew, Str::FitWidth) => "התאם לרוחב",
            (Lang::Hebrew, Str::Zoom) => "זום",
        }
    }

    pub fn text(self) -> &'static str {
        self.text_for(i18n::language())
    }

    /// Text ready for egui 0.36 rendering (visually reordered when active language is RTL).
    pub fn show_for(self, lang: Lang) -> String {
        let s = self.text_for(lang);
        if lang.is_rtl() {
            i18n::visual_rtl(s)
        } else {
            s.to_string()
        }
    }

    pub fn show(self) -> String {
        self.show_for(i18n::language())
    }

    /// Substitutes `{}` in logical order without applying visual RTL reordering (for AccessKit).
    pub fn fill_logical_for(self, lang: Lang, args: &[&str]) -> String {
        let mut out = String::new();
        let mut args = args.iter();
        let mut parts = self.text_for(lang).split("{}").peekable();
        while let Some(part) = parts.next() {
            out.push_str(part);
            if parts.peek().is_some() {
                out.push_str(args.next().copied().unwrap_or_default());
            }
        }
        out
    }

    pub fn fill_logical(self, args: &[&str]) -> String {
        self.fill_logical_for(i18n::language(), args)
    }

    /// Substitutes `{}` in logical order, then reorders visually for RTL display in `lang`.
    pub fn fill_for(self, lang: Lang, args: &[&str]) -> String {
        let logical = self.fill_logical_for(lang, args);
        if lang.is_rtl() {
            i18n::visual_rtl(&logical)
        } else {
            logical
        }
    }

    /// The text with each `{}` replaced by the next of `args`, formatted for visual display.
    pub fn fill(self, args: &[&str]) -> String {
        self.fill_for(i18n::language(), args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_placeholders_in_order() {
        assert_eq!(
            Str::PageNumber.fill_for(Lang::English, &["2", "5"]),
            "Page 2 of 5"
        );
        assert_eq!(
            Str::PageNumber.fill_for(Lang::English, &["2"]),
            "Page 2 of "
        );
        assert_eq!(Str::Ready.fill_for(Lang::English, &["x"]), "Ready");
    }

    #[test]
    fn str_all_has_no_duplicates() {
        let mut seen = Vec::with_capacity(Str::ALL.len());
        for s in Str::ALL {
            assert!(!seen.contains(&s), "duplicate Str::ALL entry: {s:?}");
            seen.push(s);
        }
    }
}
