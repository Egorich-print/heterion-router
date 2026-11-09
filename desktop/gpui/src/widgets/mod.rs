//! The building blocks of the dashboard: controls, inputs, tables and charts.
//!
//! gpui ships layout primitives, not widgets — there is no `Button`, no
//! `Input`, no `Table` in the crate. These are the ones the dashboard needs,
//! styled like the Svelte bundle so both frontends look the same.

pub mod chart;
pub mod controls;
pub mod input;
pub mod table;

pub use chart::*;
pub use controls::*;
pub use input::*;
pub use table::*;
