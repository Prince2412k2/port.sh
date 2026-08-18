# Using termap for another project

How to point this renderer at a different place, feed it your own data, or take
pieces of it into something else.

`README.md` covers what termap is and how it draws. This covers how to make it
draw *your* thing.

---

## 1. Point it somewhere else

The fastest path. termap reads any **PMTiles v3 archive of MVT tiles**, so if
you can get a basemap for your region you are done.

```bash
termap path/to/your.pmtiles
# or
TERMAP_BASEMAP=/path/to/your.pmtiles termap
# or drop it at data/basemap.pmtiles and just run `termap`
```

**Where to get one**

| source | what you get |
|---|---|
| [Protomaps](https://protomaps.com) build service | cut a bbox from the daily planet build |
| [planetiler](https://github.com/onthegomap/planetiler) | build it yourself from a Geofabrik `.osm.pbf` |
| any MVT PMTiles | works if the layer names match, or edit `src/mvt.rs` |

**Check the schema before you trust it.** Archives disagree about layer names
and class values, and the docs are not reliable:

```bash
python3 scripts/probe_pmtiles.py your.pmtiles 72.8777 19.0760 14
```

That dumps every layer, feature count, and the distribution of `class` values in
one tile. The layer and rank tables in `src/mvt.rs` were derived from this
output, not from documentation.

**Then map its vocabulary onto termap's layers** in `src/mvt.rs::classify`. That
function is the entire contract between an archive and the renderer.

> **Do not copy the road tiers as-is.** They are cut from a Mumbai extract, where
> `primary` is 16% of ways and `tertiary` is 36%. Putting `primary` in the top
> tier there collapses the whole network into one bright mesh. Run the probe on
> *your* region and cut the tiers from what it actually contains.

---

## 2. Feed it your own data

For anything the basemap does not have — administrative borders, buildings, your
own dataset — there is a flat text format that needs no dependencies on either
side.

### `.tmap`

```
# termap 1
F <layer> <rank> <closed> <npts> <name>
<lon> <lat> <lon> <lat> ...
```

One header line, one coordinate line, repeated. Coordinates are **lon lat**, in
that order — the most common mistake when writing one by hand.

| field | meaning |
|---|---|
| `layer` | id from the table below |
| `rank` | draw order within the layer, and the label priority. For `Building` it is the **height in metres**. |
| `closed` | `1` for a ring, `0` for a line or point |
| `npts` | number of coordinate pairs |
| `name` | may be empty; may contain spaces; runs to end of line |

**Layer ids**

| id | layer | | id | layer |
|---|---|---|---|---|
| 0 | `Landuse` | | 6 | `RoadMajor` |
| 1 | `Water` | | 7 | `Place` |
| 2 | `Coast` | | 8 | `Landmark` |
| 3 | `Rail` | | 9 | `Land` (ocean mask, not drawn) |
| 4 | `RoadMinor` | | 10 | `Boundary` |
| 5 | `RoadMedium` | | 11 | `Building` (extruded) |

Files at `data/states.tmap` and `data/buildings.tmap` load automatically as
**overlays** — always resident, composited over whatever the tile backend
returns. Add more slots in `Source::open` (`src/tiles.rs`).

A `.tmap` passed as the sole argument is used as the *whole map* instead:

```bash
termap data/mumbai.tmap
```

### `data/places.txt` — the experience tour

What `--tour` flies between. Same indented format: two spaces starts a key, four
or more continues the previous value, so a paragraph sits in the file as a
paragraph.

```
place gateway
  name     Gateway Corp
  kind     work
  where    Ahmedabad, Gujarat
  years    2025 — present
  role     SDE 1
  at       23.039, 72.512
  zoom     14.4
  tilt     54
  bearing  -7
  note     What happened there. Wraps to the caption's width; four or more
           spaces continues it onto the next line.
```

| key | meaning |
|---|---|
| `at` | **latitude, longitude** — the opposite order to `.tmap` |
| `zoom` | where the flight stops |
| `tilt` | degrees the camera leans *after* it has stopped moving |
| `bearing` | degrees clockwise from north-up |
| `kind` | free text, shown as a category |
| `note` | what the stop meant |

`at` is lat/lon and `.tmap` is lon/lat, which is inconsistent and deliberate:
this file is written by hand from the sources that use lat/lon — map websites,
phones, your own notes — and matching them is worth more than matching the
binary format nobody types. The parser range-checks the pair and refuses one
that cannot be a lat/lon, and a test asserts every shipped stop lands in
Gujarat, because 23,72 and 72,23 are *both* valid pairs and only one of them is
in India.

Loaded from disk if present so the sheet can be edited without a rebuild, with a
copy built into the binary as the fallback.

**Pick `zoom` for the data you have, not the place you mean.** These stops sit
around 14.4, which is wider than a building deserves, because the basemap has no
building footprints outside Mumbai — street zoom over Ahmedabad is half a dozen
roads on an empty plate. A little further back the road network and the
neighbourhood labels arrive together, and *that* is what reads as a place.

### `.tmhg` — heightmaps

```
magic   "TMHG"          4 bytes
version u8 = 1          1
_pad                    3
bounds  4 x f64         west, south, east, north (degrees)
size    2 x u32         width, height (samples)
data    w*h x i16       metres, row 0 = north, -32768 = nodata
```

Built from Copernicus DSM GeoTIFFs:

```bash
python3 scripts/dem2hgt.py <dem_dir> data/your.tmhg --arcsec 30 --bbox W,S,E,N
```

Any DEM works if you can get it into that layout. Loaded from
`data/india.tmhg` or `data/terrain.tmhg` — rename or edit `Source::open`.

---

## 3. Ready-made pipelines

Each script is standalone and writes one of the formats above.

```bash
# OSM extract for one city, chunked and resumable (Overpass 504s on whole cities)
python3 scripts/fetch_osm.py            # BBOX="S,W,N,E" to move it

# state / province borders and names, any country
curl -LO https://naciscdn.org/naturalearth/10m/cultural/ne_10m_admin_1_states_provinces.zip
unzip -d data/ne ne_10m_admin_1_states_provinces.zip
python3 scripts/ne2tmap.py data/ne/ne_10m_admin_1_states_provinces IN data/states.tmap

# building footprints with heights, small bbox only
python3 scripts/fetch_buildings.py 18.96,72.80,19.06,72.88 data/buildings.tmap

# terrain
python3 scripts/dem2hgt.py <copernicus_dir> data/india.tmhg --arcsec 30
```

Swap `IN` for any ISO code in the borders step. Shapefiles are parsed by hand —
no GDAL.

---

## 4. Taking pieces into another program

The modules are separable, and the dependency list is `ratatui`, `crossterm`,
`flate2` — nothing else.

| module | what it is | depends on |
|---|---|---|
| `pmtiles.rs` | PMTiles v3 reader | `flate2` only |
| `mvt.rs` | vector-tile decoder | `data.rs` |
| `terrain.rs` | heightmap sampling | nothing |
| `canvas.rs` | subpixel framebuffer, z-buffer, braille/quadrant/box resolve | `ratatui` for output only |
| `raster.rs` | clipped lines, dashes, dithered fills | `canvas.rs` |
| `geo.rs` | Web Mercator, viewport, oblique + perspective camera | nothing |
| `labels.rs` | collision-avoiding placement | nothing |
| `tour.rs` | Van Wijk–Nuij zoom/pan flight, and the tour state machine | `geo.rs`, `place.rs` |

`pmtiles.rs` and `mvt.rs` together are about 400 lines and have no relationship
to the rest — lift them if you want to read vector tiles anywhere. Same for
`terrain.rs`.

**`canvas.rs` is the interesting one to reuse.** It is a general subpixel
framebuffer for terminals: float coverage, depth, tint and a pick id per
subpixel, resolving to braille (2×4), quadrants (2×2) or box-drawing with
proper junctions. Nothing in it is map-specific.

### Rendering without a terminal

```bash
termap --snapshot 168x48                      # one frame of ANSI to stdout
termap --snapshot 168x48 --plain              # glyphs only
termap --snapshot 168x48 | python3 scripts/ansi2png.py out.png
```

Flags: `--center LON,LAT --zoom Z --tilt DEG --bearing DEG --roads MODE
--weight W --focus MODE --cursor X,Y`.

The tour can be snapshotted too, including mid-flight:

```bash
termap --snapshot 180x48 --place gateway                  # the arrival
termap --snapshot 180x48 --place knowledge-high \
       --from gateway --at 1.06                           # the top of the arc
```

`--at` steps the animation at a fixed 1/60 rather than off a wall clock, so the
same command produces the same pixels every time and a flight can be reviewed
frame by frame.

This is how every rendering decision in this project was checked. If you change
the renderer, diff snapshots rather than trusting a description of the change.

---

## 5. Changing how it looks

| want to | edit |
|---|---|
| restyle a layer (colour, width, depth, dither, zoom floor) | `src/style.rs` |
| add a layer | `src/data.rs` (`Layer`, `LAYER_COUNT`, `DRAW_ORDER`) then `style.rs` |
| change what appears at which zoom | `src/view.rs` |
| change the tour's stops, cameras or captions | `data/places.txt` |
| change how the flight feels (pace, arc, lean) | `RHO` `SPEED` `SETTLE` `LEVEL_BY` in `src/tour.rs` |
| change how far out the tour opens | `Viewport::fit` call in `App::open_tour_if_pending` |
| change the palette | `TINT_RGB` in `src/canvas.rs` |
| change glyph families | `BRAILLE` / `QUADRANT` / `LINE_LIGHT` / `LINE_HEAVY` in `src/canvas.rs` |
| change terrain treatment | `src/relief.rs` |

Constants worth knowing:

| constant | where | what |
|---|---|---|
| `MIN_ZOOM` `MAX_ZOOM` | `geo.rs` | 2.5 – 18.0 |
| `EXAG` | `relief.rs` | terrain exaggeration, 14× |
| `STEP` | `relief.rs` | terrain sample spacing, 3 subpixels |
| `FADE` | `scene.rs` | how far the ground slab dissolves at its edge |
| `CACHE_TILES` | `tiles.rs` | 96 tiles held |
| `MAX_TILES_PER_VIEW` | `tiles.rs` | 40, then it steps down a zoom |

---

## 6. What it costs

Measured on the India basemap (1.6 GB archive, 27 MB heightmap) at 168×48:

| view | features drawn | frame |
|---|---|---|
| `FLAT` z4, whole country | 14,636 | 5.0 ms |
| `RELIEF` z11, city | 2,139 | 2.9 ms |
| `3D` z16, street + 33 buildings | 164 | 2.2 ms |

Resident memory ~60 MB, most of it the heightmap. **Archive size does not
affect memory** — tiles stream and 96 are held.

Eager loading, for comparison, costs ~220 bytes/feature and caps out around
2–3M features. That is not enough for a country at street level, which is why
the tiled backend exists.

---

## 7. Things that will bite you

Every one of these cost real debugging time here.

**Coordinates are lon/lat in `.tmap`,** and lat/lon almost everywhere else you
will copy from. Silent, and produces a map of the wrong hemisphere.

**dBase pads with NUL, not spaces.** Shapefile attribute names come back as
`Ladakh\x00\x00\x00…` and a plain `.strip()` leaves them unusable, so every
label silently vanishes.

**MVT exterior rings are clockwise in *tile* space,** and tile space has y
increasing downward — so clockwise is *positive* shoelace area, the opposite of
the usual convention. Get it backwards and you keep the holes and drop every
polygon.

**Douglas–Peucker anchors on first and last point,** which on a closed ring are
neighbours. Every vertex then measures as "near the baseline" and the ring
collapses to a handful of points. Do not simplify rings with it.

**OSM has no ocean.** Sea is implied by coastline direction (land on the left),
not stored as an area. Either derive land rings and use them as a mask (see
`scripts/osm2tmap.py`), or use a basemap whose `water` layer already includes
ocean polygons, which planetiler's does.

**Road classes are not comparable between cities.** See §1.

**Archives lie about their zoom range.** The India basemap advertises
`min_zoom: 4` and has no z4 tiles. `Tiled::cover` steps in until something comes
back.

**Density is per subpixel, legibility is per cell.** At 8% of subpixels, ~64% of
*cells* catch a dot and a dither reads as static, not texture.

**Exaggerate elevation relative to local ground, not sea level.** A town on a
300 m plateau at 14× lifts by more than half the frame and slides the whole map
off the top of the screen.

**Under perspective, reject geometry behind the eye — do not clamp it.** Clamping
gives it an enormous scale factor and flings it across the frame in a radial
fan that looks like a clipping bug.

**Clip segments, do not drop them.** At street zoom a basemap's vertices are
hundreds of metres apart, so both ends of a road sit outside a small viewport
while the middle crosses it. Dropping such segments makes roads vanish entirely.

**Area fills need every vertex inside the clip; lines need only some.** A ring
is scan-filled as a whole, so one stray vertex drags the fill across everything
between them.

---

## 8. Fonts

`lines` road mode needs only box-drawing (U+2500) and quadrants (U+2580), which
every monospace font ships.

Ground textures use Braille Patterns (U+2800–U+28FF), and that block is missing
from a surprising number of coding fonts — JetBrains Mono and Noto Sans Mono
among them. They fall back at the terminal's discretion. If terrain renders as
boxes, that is why.

Unicode 16 **octants** (U+1CD00) would be strictly better than both — solid
strokes at braille's 2×4 resolution — but no font ships them yet. When one does,
adding a fourth `RoadGlyph` variant is a small change in `canvas.rs`.
