//! Finding the data directory.
//!
//! The renderer reads its basemap, heightmap and overlays from relative paths,
//! which was fine while it was one binary run from one directory. It is now
//! also a library inside a workspace, so the working directory might be `map/`,
//! the workspace root, or wherever the portfolio binary was launched from — and
//! guessing wrong does not fail loudly, it silently falls back to the embedded
//! Mumbai sample and renders the wrong city.

use std::path::{Path, PathBuf};

/// Directories to try, nearest first. `TERMAP_DATA` wins so the archive can
/// live outside the project entirely — it is 1.6 GB and does not belong in a
/// checkout.
fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(p) = std::env::var_os("TERMAP_DATA") {
        v.push(PathBuf::from(p));
    }
    v.extend(["data", "map/data", "../map/data", "../data"].map(PathBuf::from));
    v
}

/// The first candidate directory that exists.
pub fn data_dir() -> Option<PathBuf> {
    candidates().into_iter().find(|p| p.is_dir())
}

/// Resolve one file under the data directory, if it is there.
///
/// Returns `None` rather than a path that does not exist, so callers cannot
/// accidentally report "failed to open" for a file that was never going to be
/// at the path they built.
pub fn data_file(name: &str) -> Option<PathBuf> {
    candidates()
        .into_iter()
        .map(|d| d.join(name))
        .find(|p| Path::new(p).exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tour sheet is committed, so it is findable from anywhere in the
    /// workspace. If this fails, the portfolio is about to render Mumbai.
    #[test]
    fn the_data_directory_is_found_from_wherever_the_tests_run() {
        assert!(data_file("places.txt").is_some(), "candidates: {:?}", candidates());
    }
}
