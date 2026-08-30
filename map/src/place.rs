//! The experience tour sheet: where each stop is, and what it meant.
//!
//! Same hand-rolled indented format as the rest of the project's data. Two
//! spaces starts a key, four or more continues the previous value, which lets a
//! paragraph sit in the file as a paragraph instead of one long line.

/// One stop on the tour.
#[derive(Debug, Clone, Default)]
pub struct Place {
    pub id: String,
    pub name: String,
    /// school / university / internship / work — shown as a category, and it is
    /// deliberately free text so a stop can be honest about being a wrong turn.
    pub kind: String,
    pub where_: String,
    pub years: String,
    pub role: String,
    pub lonlat: (f64, f64),
    /// World (Mercator) coordinates, converted once at load.
    pub world: [f64; 2],
    pub zoom: f64,
    /// Radians, both of them: the file says degrees, the camera wants radians,
    /// and converting at the boundary means nothing downstream has to remember
    /// which unit it is holding.
    pub tilt: f64,
    pub bearing: f64,
    pub note: String,
}

/// Loaded from `data/places.txt` if it is there, otherwise from the copy built
/// into the binary. The disk path wins so the sheet can be edited and reloaded
/// without a rebuild; the embedded copy means a bare binary still has a tour.
pub fn load() -> Vec<Place> {
    let disk = crate::paths::data_file("places.txt").and_then(|p| std::fs::read_to_string(p).ok());
    let src = disk
        .as_deref()
        .unwrap_or(include_str!("../data/places.txt"));
    match parse(src) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("termap: places.txt: {e}");
            Vec::new()
        }
    }
}

pub fn parse(src: &str) -> Result<Vec<Place>, String> {
    let mut out: Vec<Place> = Vec::new();
    let mut last_key = String::new();

    for (n, line) in src.lines().enumerate() {
        let line = line.trim_end();
        let no = n + 1;
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();

        if indent == 0 {
            let (word, id) = bare
                .split_once(char::is_whitespace)
                .ok_or_else(|| format!("line {no}: `{bare}` needs an id"))?;
            if word != "place" {
                return Err(format!("line {no}: unknown record `{word}`"));
            }
            out.push(Place {
                id: id.trim().to_string(),
                ..Default::default()
            });
            last_key.clear();
            continue;
        }

        let p = out
            .last_mut()
            .ok_or_else(|| format!("line {no}: value before any `place`"))?;

        // Four or more spaces continues the previous key's value. Joined with a
        // space, so a paragraph wrapped in the file reflows in the terminal
        // rather than keeping the file's line breaks.
        if indent >= 4 && !last_key.is_empty() {
            let field = field_mut(p, &last_key)
                .ok_or_else(|| format!("line {no}: cannot continue `{last_key}`"))?;
            if !field.is_empty() {
                field.push(' ');
            }
            field.push_str(bare);
            continue;
        }

        let (key, val) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        let val = val.trim();
        last_key = key.to_string();

        match key {
            "at" => {
                let (a, b) = val
                    .split_once(',')
                    .ok_or_else(|| format!("line {no}: `at` wants `lat, lon`"))?;
                let lat: f64 = a
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {no}: bad latitude `{}`", a.trim()))?;
                let lon: f64 = b
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {no}: bad longitude `{}`", b.trim()))?;
                // Catches the lat/lon swap, which is otherwise silent and puts
                // the whole tour in the Indian Ocean.
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                    return Err(format!("line {no}: ({lat}, {lon}) is not a lat/lon pair"));
                }
                p.lonlat = (lon, lat);
                p.world = crate::geo::lonlat_to_world(lon, lat);
            }
            "zoom" => {
                p.zoom = val
                    .parse::<f64>()
                    .map_err(|_| format!("line {no}: bad zoom `{val}`"))?
                    .clamp(crate::geo::MIN_ZOOM, crate::geo::MAX_ZOOM)
            }
            "tilt" => {
                p.tilt = val
                    .parse::<f64>()
                    .map_err(|_| format!("line {no}: bad tilt `{val}`"))?
                    .clamp(0.0, 68.0)
                    .to_radians()
            }
            "bearing" => {
                p.bearing = val
                    .parse::<f64>()
                    .map_err(|_| format!("line {no}: bad bearing `{val}`"))?
                    .to_radians()
            }
            _ => {
                let field =
                    field_mut(p, key).ok_or_else(|| format!("line {no}: unknown key `{key}`"))?;
                field.push_str(val);
            }
        }
    }

    for p in &out {
        if p.name.is_empty() {
            return Err(format!("`{}` has no name", p.id));
        }
        if p.world == [0.0, 0.0] {
            return Err(format!("`{}` has no `at`", p.id));
        }
    }
    Ok(out)
}

/// The string fields, by key. Kept in one place so `parse` and the continuation
/// rule cannot disagree about which keys exist.
fn field_mut<'a>(p: &'a mut Place, key: &str) -> Option<&'a mut String> {
    Some(match key {
        "name" => &mut p.name,
        "kind" => &mut p.kind,
        "where" => &mut p.where_,
        "years" => &mut p.years,
        "role" => &mut p.role,
        "note" => &mut p.note,
        _ => return None,
    })
}

/// Greedy word wrap. Returns lines no longer than `width` where the text
/// allows; a single word longer than `width` is left over-long rather than
/// broken, because breaking it mid-word reads worse than one ragged line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_sheet_parses() {
        let places = parse(include_str!("../data/places.txt")).expect("sheet parses");
        assert!(places.len() >= 5, "got {} places", places.len());
        for p in &places {
            assert!(!p.note.is_empty(), "{} has no note", p.id);
            assert!(!p.years.is_empty(), "{} has no years", p.id);
            assert!(p.zoom > 0.0, "{} has no zoom", p.id);
        }
    }

    /// Every stop should be somewhere in Gujarat. A lat/lon swap still parses
    /// as a valid pair (23,72 and 72,23 are both in range), so range checking
    /// alone would not catch it — but it would move the tour to Kazakhstan.
    #[test]
    fn every_stop_is_where_it_claims_to_be() {
        for p in parse(include_str!("../data/places.txt")).unwrap() {
            let (lon, lat) = p.lonlat;
            assert!(
                (20.0..25.0).contains(&lat) && (68.0..75.0).contains(&lon),
                "{} at ({lat}, {lon}) is not in Gujarat",
                p.id
            );
        }
    }

    #[test]
    fn a_wrapped_paragraph_keeps_its_words_and_respects_the_width() {
        let text = "one two three four five six seven eight nine ten";
        let lines = wrap(text, 12);
        assert!(lines.iter().all(|l| l.chars().count() <= 12), "{lines:?}");
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn a_continued_value_reflows_instead_of_keeping_file_line_breaks() {
        let p = &parse("place x\n  name X\n  at 23.0, 72.0\n  note one\n    two\n").unwrap()[0];
        assert_eq!(p.note, "one two");
    }

    #[test]
    fn a_swapped_coordinate_pair_is_refused() {
        // 200 is not a longitude, and this is the shape the mistake takes.
        let err = parse("place x\n  name X\n  at 23.0, 200.0\n").unwrap_err();
        assert!(err.contains("not a lat/lon pair"), "{err}");
    }
}
