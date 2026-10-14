//! The string catalogue (spec A-2): every word the UI shows that isn't a command's own label
//! (those are keyed by command id in the engine's command table). English only in v0.1;
//! `cargo xtask ci` rejects string literals passed to widgets anywhere else in this crate.

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
    // Views and Panes (CONTEXT.md).
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
    OpenDashFiles,
    DataFiles,
    // Recovery and closing (spec U-7).
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
    NoFilters,
    FitPage,
    FitWidth,
    RowCount,
    RowsRead,
    CellError,
    TransformData,
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
    pub fn text(self) -> &'static str {
        match self {
            Str::AppName => "OpenDash BI",
            Str::Untitled => "Untitled",
            Str::TabFile => "File",
            Str::TabHome => "Home",
            Str::TabInsert => "Insert",
            Str::TabModeling => "Modeling",
            Str::TabView => "View",
            Str::TabHelp => "Help",
            Str::TabColumnTools => "Column tools",
            Str::TabTransform => "Transform",
            Str::ViewReport => "Report",
            Str::ViewTable => "Table",
            Str::ViewModel => "Model",
            Str::PaneFilters => "Filters",
            Str::PaneVisualizations => "Visualizations",
            Str::PaneData => "Data",
            Str::AppTheme => "App theme",
            Str::ThemeLight => "Light",
            Str::ThemeDark => "Dark",
            Str::Ready => "Ready",
            Str::Working => "Working: {}",
            Str::Unsaved => "Unsaved changes",
            Str::Dismiss => "Dismiss",
            Str::PaletteHint => "Type a command",
            Str::PaletteNone => "No command matches",
            Str::Run => "Run",
            Str::Cancel => "Cancel",
            Str::Optional => "optional",
            Str::Browse => "Browse…",
            Str::OpenDashFiles => "OpenDash projects and packages",
            Str::DataFiles => "CSV, Excel and Parquet files",
            Str::RecoveryFound => "Unsaved work from an earlier session can be recovered: {}",
            Str::RecoveryUnsaved => "a document never saved",
            Str::Recover => "Recover",
            Str::Discard => "Discard",
            Str::SaveFirst => "Save changes to {} before closing?",
            Str::Save => "Save",
            Str::DontSave => "Don't save",
            Str::NoPages => "No pages yet. Insert a page to start a report.",
            Str::NoTables => "No tables yet. Get data to add one.",
            Str::TableViewHint => "Pick a table in the Data pane to see its rows.",
            Str::SelectVisual => "Select a visual on the canvas to see its fields and filters.",
            Str::PageNumber => "Page {} of {}",
            Str::Shortcut => "Shortcut: {}",
            Str::VisualOf => "{} visual",
            Str::VisTable => "Table",
            Str::VisMatrix => "Matrix",
            Str::VisCard => "Card",
            Str::VisBar => "Clustered bar chart",
            Str::VisColumn => "Clustered column chart",
            Str::VisLine => "Line chart",
            Str::VisSlicer => "Slicer",
            Str::WellValues => "Values",
            Str::WellRows => "Rows",
            Str::WellColumns => "Columns",
            Str::WellAxis => "X-axis",
            Str::WellLegend => "Legend",
            Str::WellField => "Field",
            Str::WellEmpty => "Add data fields here",
            Str::Remove => "Remove",
            Str::NoFilters => "No slicer selections on this page.",
            Str::RowCount => "{} rows",
            Str::RowsRead => "{}: {} rows read",
            Str::CellError => "Error",
            Str::TransformData => "Transform data",
            Str::Close => "Close editor",
            Str::QueryEditor => "Power Query Editor",
            Str::Queries => "Queries",
            Str::AppliedSteps => "Applied Steps",
            Str::SourceStep => "Source",
            Str::DeleteStep => "Delete step",
            Str::QueryM => "M (read-only)",
            Str::NoQueries => "No queries yet. Get data to add one.",
            Str::PreviewOf => "{} preview",
            Str::LoadingPreview => "Loading the preview…",
            Str::TableOf => "{} table",
            Str::FormulaShape => "Write it as Name = expression",
            Str::NewMeasureName => "Measure",
            Str::NewColumnName => "Column",
            Str::Commit => "Commit",
            Str::FormulaMeasure => "Measure formula",
            Str::FormulaColumn => "Column formula",
            Str::ColumnOf => "{} column of {}",
            Str::RowScroll => "Scroll rows",
            Str::ColumnScroll => "Scroll columns",
            Str::FitPage => "Fit to page",
            Str::FitWidth => "Fit to width",
            Str::Zoom => "Zoom",
        }
    }

    /// The text with each `{}` replaced by the next of `args`.
    pub fn fill(self, args: &[&str]) -> String {
        let mut out = String::new();
        let mut args = args.iter();
        let mut parts = self.text().split("{}").peekable();
        while let Some(part) = parts.next() {
            out.push_str(part);
            if parts.peek().is_some() {
                out.push_str(args.next().copied().unwrap_or_default());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_placeholders_in_order() {
        assert_eq!(Str::PageNumber.fill(&["2", "5"]), "Page 2 of 5");
        assert_eq!(Str::PageNumber.fill(&["2"]), "Page 2 of ");
        assert_eq!(Str::Ready.fill(&["x"]), "Ready");
    }
}
