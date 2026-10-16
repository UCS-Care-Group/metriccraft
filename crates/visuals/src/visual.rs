//! Visual definitions (glossary: Visual, Field well, Field): what a report file stores for a
//! Visual and what its query and layout read. No data, no state of the Page.

use std::collections::BTreeMap;

use crate::query::{MAX_AXIS_FIELDS, MAX_VALUE_FIELDS};

/// The v0.1 visual types. Ids are what report files and commands use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VisualType {
    Table,
    Matrix,
    Card,
    ClusteredBar,
    ClusteredColumn,
    Line,
    Slicer,
}

/// A named slot of a Visual that takes Fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Well {
    /// Table columns; matrix, chart and card values.
    Values,
    Rows,
    Columns,
    Axis,
    Legend,
    /// A slicer's one Column.
    Field,
}

impl VisualType {
    pub const ALL: [VisualType; 7] = [
        VisualType::Table,
        VisualType::Matrix,
        VisualType::Card,
        VisualType::ClusteredBar,
        VisualType::ClusteredColumn,
        VisualType::Line,
        VisualType::Slicer,
    ];

    pub fn id(self) -> &'static str {
        match self {
            VisualType::Table => "table",
            VisualType::Matrix => "matrix",
            VisualType::Card => "card",
            VisualType::ClusteredBar => "clusteredBar",
            VisualType::ClusteredColumn => "clusteredColumn",
            VisualType::Line => "line",
            VisualType::Slicer => "slicer",
        }
    }

    /// The type with id `id` (case-insensitive).
    pub fn from_id(id: &str) -> Option<VisualType> {
        Self::ALL
            .into_iter()
            .find(|t| t.id().eq_ignore_ascii_case(id))
    }

    /// The wells this type has, in display order.
    pub fn wells(self) -> &'static [Well] {
        match self {
            VisualType::Table => &[Well::Values],
            VisualType::Matrix => &[Well::Rows, Well::Columns, Well::Values],
            VisualType::Card => &[Well::Values],
            VisualType::ClusteredBar | VisualType::ClusteredColumn | VisualType::Line => {
                &[Well::Axis, Well::Legend, Well::Values]
            }
            VisualType::Slicer => &[Well::Field],
        }
    }

    /// Most Fields `well` takes: one for a one-Field well, else [`MAX_AXIS_FIELDS`] for a
    /// grouping well and [`MAX_VALUE_FIELDS`] for Values (bounds what a file or command makes a
    /// query do).
    pub fn max_fields(self, well: Well) -> usize {
        match (self, well) {
            (VisualType::Card, Well::Values) | (VisualType::Slicer, Well::Field) => 1,
            (_, Well::Legend) => 1,
            (_, Well::Values) => MAX_VALUE_FIELDS,
            _ => MAX_AXIS_FIELDS,
        }
    }
}

impl Well {
    pub const ALL: [Well; 6] = [
        Well::Values,
        Well::Rows,
        Well::Columns,
        Well::Axis,
        Well::Legend,
        Well::Field,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Well::Values => "values",
            Well::Rows => "rows",
            Well::Columns => "columns",
            Well::Axis => "axis",
            Well::Legend => "legend",
            Well::Field => "field",
        }
    }

    pub fn from_id(id: &str) -> Option<Well> {
        Self::ALL
            .into_iter()
            .find(|w| w.id().eq_ignore_ascii_case(id))
    }
}

/// A Field: a Column or a Measure of the Model, by name (renames rewrite it). A Column
/// in a Values well is an Implicit measure.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FieldRef {
    Column {
        table: String,
        column: String,
    },
    /// Measure names are unique across the Model.
    Measure {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortDirection {
    Ascending,
    Descending,
}

/// An explicit sort; without one the type's default sort applies (M4.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sort {
    pub field: FieldRef,
    pub direction: SortDirection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LegendPosition {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SlicerStyle {
    #[default]
    List,
    Dropdown,
}

/// The v0.1 format options (grill Q8).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Format {
    /// `None`: a title generated from the Fields ("Sum of Amount by Region").
    pub title: Option<String>,
    pub show_title: bool,
    pub data_labels: bool,
    pub legend: bool,
    pub legend_position: LegendPosition,
    pub row_subtotals: bool,
    pub column_subtotals: bool,
    pub slicer_style: SlicerStyle,
}

impl Default for Format {
    fn default() -> Self {
        Format {
            title: None,
            show_title: true,
            data_labels: false,
            legend: true,
            legend_position: LegendPosition::Top,
            row_subtotals: true,
            column_subtotals: true,
            slicer_style: SlicerStyle::List,
        }
    }
}

/// One Visual on a Page: position in logical pixels (the Page is 1280×720 by default), its
/// Fields per well, sort and format.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Visual {
    /// Short stable id, unique in its Page.
    pub id: String,
    pub kind: VisualType,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub z: i32,
    pub wells: BTreeMap<Well, Vec<FieldRef>>,
    pub sort: Option<Sort>,
    pub format: Format,
}

impl Visual {
    /// An empty Visual of `kind`.
    pub fn new(id: impl Into<String>, kind: VisualType) -> Visual {
        Visual {
            id: id.into(),
            kind,
            x: 0,
            y: 0,
            w: 400,
            h: 300,
            z: 0,
            wells: BTreeMap::new(),
            sort: None,
            format: Format::default(),
        }
    }

    /// The Fields in `well` (empty if none).
    pub fn fields(&self, well: Well) -> &[FieldRef] {
        self.wells.get(&well).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for t in VisualType::ALL {
            assert_eq!(VisualType::from_id(t.id()), Some(t));
            assert!(!t.wells().is_empty());
        }
        for w in Well::ALL {
            assert_eq!(Well::from_id(&w.id().to_uppercase()), Some(w));
        }
        assert_eq!(VisualType::from_id("pie"), None);
        assert_eq!(VisualType::Card.max_fields(Well::Values), 1);
        assert_eq!(
            VisualType::Matrix.max_fields(Well::Values),
            MAX_VALUE_FIELDS
        );
        assert_eq!(VisualType::Matrix.max_fields(Well::Rows), MAX_AXIS_FIELDS);
    }
}
