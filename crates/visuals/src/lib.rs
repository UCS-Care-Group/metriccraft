//! Visual definitions, visual queries and layout to draw primitives.
//!
//! Layer L3 — see `docs/plan/0001-mvp.md` §1. ADR 0007: a Visual lays out into a [`Scene`] of
//! draw commands in logical pixels; renderers (egui in M5, [`png`] headless) consume it.

pub mod draw;
#[cfg(feature = "png")]
pub mod png;
pub mod snapshot;
pub mod text;
pub mod theme;
pub mod visual;

pub use draw::{Align, Cmd, Color, Scene, Tag, Weight};
pub use theme::Theme;
pub use visual::{FieldRef, Format, Sort, SortDirection, Visual, VisualType, Well};
