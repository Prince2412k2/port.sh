//! termap as a library.
//!
//! The geometry/projection/MVT core is intentionally available without the native
//! terminal stack. The portfolio WASM client consumes these exact modules; the
//! standalone binary enables the default `native` feature and keeps the full UI.

pub mod data;
pub mod geo;
pub mod mvt;
pub mod pmtiles;

#[cfg(feature = "native")]
pub mod app;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod canvas;
#[cfg(feature = "native")]
pub mod find;
#[cfg(feature = "native")]
pub mod gazetteer;
#[cfg(feature = "native")]
pub mod home;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod labels;
#[cfg(feature = "native")]
pub mod paths;
#[cfg(feature = "native")]
pub mod place;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod raster;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod relief;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod scene;
#[cfg(feature = "native")]
pub mod snapshot;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod style;
#[cfg(any(feature = "native", feature = "browser-core"))]
pub mod terrain;
#[cfg(feature = "native")]
pub mod tiles;
#[cfg(feature = "native")]
pub mod tour;
#[cfg(feature = "native")]
pub mod ui;
pub mod view;
