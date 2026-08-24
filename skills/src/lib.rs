//! skysheet as a library.
//!
//! Same reasoning as termap's `lib.rs`: the binary here and the portfolio app
//! are two front ends over one set of modules, and the alternative to this file
//! is a second copy of the logo pipeline's output.

pub mod app;
pub mod canvas;
pub mod cards;
pub mod data;
pub mod diagram;
pub mod grid;
pub mod logos;
pub mod marks;
pub mod scene;
pub mod snapshot;
pub mod tile;
pub mod ui;
