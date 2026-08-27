//! termap as a library.
//!
//! The binary in `main.rs` is one consumer of this; the portfolio app is
//! another. Everything is public because the split is not an encapsulation
//! boundary — it is so two front ends can drive the same renderer without a
//! second copy of it existing.

pub mod app;
pub mod canvas;
pub mod data;
pub mod find;
pub mod gazetteer;
pub mod geo;
pub mod home;
pub mod labels;
pub mod mvt;
pub mod paths;
pub mod place;
pub mod pmtiles;
pub mod raster;
pub mod relief;
pub mod scene;
pub mod snapshot;
pub mod style;
pub mod terrain;
pub mod tiles;
pub mod tour;
pub mod ui;
pub mod view;
