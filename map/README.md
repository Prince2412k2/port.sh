# termap

A map you pan around inside a terminal. Every cell is a 2×4 braille grid, so an
ordinary 160×46 terminal is really a 320×184 framebuffer — enough to draw a
coastline that actually looks like a coastline.

It reads a PMTiles basemap, so "the map" can be a whole country at street level
without that country having to fit in memory — a 1.6 GB archive of India renders
in 19 MB resident.

```
cargo run --release
```

Pointing it at another region, feeding it your own data, or lifting pieces of it
into something else: **[INTEGRATING.md](INTEGRATING.md)**.

## What makes it look like that

**Three ways to draw a road, on `r`.** No single glyph family is right, so the
map carries all three and you pick:

| | resolution | stroke |
|---|---|---|
| `braille` | 2×4 | finest angles, but dotted by construction |
| `blocks` | 2×2 | continuous; half a cell is the thinnest possible |
| `lines` | 1×1 | true hairlines with real junctions; diagonals staircase |

Unicode 16 *octants* would be the ideal — solid strokes at braille's 2×4 — but
no font on a normal system ships them yet. `lines` is the default: box-drawing
records which cell edges a road crosses, so crossings resolve to `┼ ├ ┴` rather
than two strokes overlapping, and motorways take the heavy variants (`━ ╋`).

Ground layers always stay braille. That split is the point: roads read as
strokes, terrain reads as texture, and the eye separates them before it has
parsed anything.

**Two axes out of one cell.** The canvas stores a float *coverage* and a *depth*
per subpixel rather than a bare on/off bit. At resolve time, coverage decides
which of the eight dots light up, and depth scales how bright they are. So a
feature can be faint because it is thin, or faint because it is far — and those
read differently.

**Depth without a camera.** A flat map has no z, so depth is assigned by
importance: a primary road sits at 0.20, a park boundary at 0.95. On top of that
sits an interactive falloff — press `f` to cycle it — that pulls whatever is near
the cursor forward and pushes the rest back. Long features get per-segment depth,
so a road can be near at one end and far at the other.

**Fills are dithered, not solid.** The sea and parks go through an 8×8 ordered
dither, which thins them to a stipple. That reads as *behind* before the
brightness has any say. 8×8 rather than 4×4 because a 4×4 matrix repeats every
two cells and the eye picks it up as corduroy.

**Colour carries kind, brightness carries depth.** Motorways are warm cream,
primary and secondary tan, minor roads plain grey, railways violet, water blue,
parks green, landmarks amber. Every hue is heavily desaturated and they sit in a
narrow luminance band, so the map reads as a tinted technical drawing rather
than a chart. `c` drops the whole thing back to monochrome.

**Zoom thresholds are doing real work.** A 20 km view that draws all 5,982
secondary roads is mush no amount of shading rescues. Secondary waits for z11.5,
minor for z13.0.

## 2.5D

`u` tilts the camera; `m` puts it back. The projection is **oblique, not
perspective** — rotate by bearing, compress y by `cos(tilt)`, lift elevation by
`sin(tilt)`. There is no vanishing point, no near-plane clipping and no divide,
which at braille resolution costs nothing visually and removes a whole class of
degenerate cases. At `tilt = 0` it reduces exactly to the 2D projection, so
there is one code path rather than two.

Tilt alone does not read as depth, and three separate things had to be true
before it did:

**A real z-buffer.** `Canvas` stored a depth per subpixel but never *tested* it,
so nothing was ever hidden. It now rejects writes that fall behind a nearer
surface — but only when tilted. In 2D, depth is a styling device ("importance as
distance") and the paint order is not monotonic in it, so testing there would
drop minor roads wherever they cross water.

**Terrain occludes from its gaps.** Relief is drawn as a stipple but is
geometrically solid, so it claims a separate `zbuf` across the whole ribbon,
painted or not. Without that, roads behind a ridge show through between the dots.

**Geometry is draped.** Features used to project at sea level while the ground
rose above them, so nothing lined up. Every vertex is now sampled against the
heightmap first.

**The ground is a bounded slab that dissolves at its edge.** An unbounded ground
plane covers the whole frame and gives the eye nothing to read, but a hard clip
is no better: four straight edges look like a frame drawn around the map rather
than ground running out. Content fades over the last stretch instead, which
bounds the plane just as firmly without ever drawing a box.

**Perspective, above z14.** Convergence is the one depth cue a parallel
projection cannot produce — it is what makes a street recede — but it is not
free: anything that has passed behind the eye must be *rejected*, not clamped.
Clamping gives it an enormous scale factor and flings it across the frame in a
radial fan that reads like a clipping bug. It stays off at wide zooms, where it
buys little and risks much.

**Terrain** comes from Copernicus DSM 30 m tiles, baked down by
`scripts/dem2hgt.py` into one flat `i16` grid (all of India at 30 arcsec is
28 MB, so it loads once and stays resident — terrain is smooth and a pyramid
would buy nothing).

Relief is drawn as a screen-aligned grid of vertical ribbons, marched far to
near, so a nearer ridge paints over the valley behind it and occlusion falls out
of the draw order with no visibility test. Two decisions matter more than they
look:

- **Shading is driven by slope, not elevation.** Drawn by height, terrain fills
  a whole city with texture and buries the map. Drawn by slope, flat ground
  stays empty and only real relief shows.
- **Vertical exaggeration is 14×, measured from local ground.** India's tallest
  ground is under 0.1% of the country's width, so honest relief is invisible at
  map scale — every tilted map exaggerates. But it has to be exaggerated
  *relative to the ground under the view*, not above sea level: a town sitting
  on a 300 m plateau otherwise lifts by more than half the frame and slides the
  whole map off the top of the screen.

```bash
python3 scripts/dem2hgt.py <copernicus_dir> data/india.tmhg --arcsec 30
```

## Mode follows zoom

One renderer used at every scale is wrong at most of them: what makes a street
corner read as three-dimensional turns a view of a whole state into noise. So
the mode is a function of zoom, and the camera ramps rather than switching —
the world rising into three dimensions as you zoom in is the point.

| zoom | mode | |
|---|---|---|
| < 10 | `FLAT` | reference map: sparse roads, no terrain, no fills |
| 10–14 | `RELIEF` | ground relief, slight lean |
| 14–15.5 | `2.5D` | building masses appear |
| 15.5+ | `3D` | full extrusion and perspective |

`m` hands the camera back to you; `u`/`o` and `,`/`.` take it over.

Far views thin by **rank**, not by stipple. A road that is far away should stay
a line and get darker — dissolving it into dots is what made a view of Gujarat
look like static.

## Buildings

The basemap has no buildings layer, so footprints come from Overpass with
`height` / `building:levels`, loaded through the same overlay slot as the state
borders:

```bash
python3 scripts/fetch_buildings.py 18.96,72.80,19.06,72.88 data/buildings.tmap
```

Walls are stippled and the roof outline is the brightest edge — a terminal has
no fill shades to spare, and a half-toned face still reads as a surface. Back
faces are not culled: the z-buffer already rejects whatever a nearer wall has
claimed, so a building hides its own far side for free.

## Your location

`@` finds you and flies there.

Resolved in order of how much it can be trusted:

```bash
TERMAP_HOME=19.0176,72.8562 termap     # explicit; the only one worth relying on
echo "19.0176,72.8562" > ~/.config/termap/home
# otherwise: IP geolocation, city-level, wrong entirely behind a VPN
```

The IP lookup runs on its own thread — blocking startup on a network round trip
to draw one marker is a poor trade — over plain HTTP on a raw socket, because
the alternative is a TLS stack for something the server already knows.

Two details that are about honesty rather than polish. The marker carries an
**accuracy ring**, because an IP fix is a city centroid and a bare dot claims a
precision the source does not have. And `@` zooms to the *uncertainty*: flying
to street level on a fix good to ten kilometres would put a confident dot on
the wrong street.

## Controls

| | |
|---|---|
| drag | pan |
| wheel | zoom, anchored under the cursor |
| click | pin a feature (`esc` unpins) |
| `h` `j` `k` `l` | pan |
| `+` `-` | zoom |
| `u` `o` | tilt the camera (2.5D) |
| `,` `.` | rotate bearing |
| `m` | back to flat 2D |
| `(` | terrain relief |
| `x` | fly to your location |
| `f` | depth focus: off / subtle / strong |
| `r` | road glyphs: lines / braille / blocks |
| `c` | monochrome / colour by kind |
| `!` `@` `#` `$` `%` `^` `&` `*` | toggle a layer, `)` for all |
| `t` `p` | labels, side panel |
| `g` | fit to data |
| `?` `q` | help, quit |
| `e` | fly the experience tour |
| `n` `b` | next / previous stop |
| `enter` | replay the arrival |

The layer toggles are Shift and a digit — `!` for the first layer through `*` for
the eighth, `(` and `)` for terrain and all-on — rather than the digits
themselves. They were the digits until the portfolio embedded this map and took
`1`–`5` for moving between sections, at which point `3` meant "minor road" here
and "skills" one section later, and `6`–`9` reached nothing at all. The side
panel prints the key beside each layer, and both come from one table
(`app::LAYER_KEYS`), so the label cannot drift from the binding. What is matched
is the character rather than the chord, so a keyboard that puts `!` somewhere
other than Shift+1 still works.

Hovering reports what is under the pointer in the status bar. Hit-testing reads a
per-subpixel feature-id buffer written during rasterisation, so it is exact
rather than a nearest-centroid guess.

## The experience tour

`termap --tour` turns the map into a CV. It reads `data/places.txt` — five
stops, each with a coordinate, a camera, and a sentence about what happened
there — and flies between them. The caption sits at the top, in air the map has
been dissolved out of.

**The flight is an arc, and that is the whole idea.** Interpolating centre and
zoom independently is the obvious implementation and it is unwatchable: at
street zoom the ground crosses the screen hundreds of times a second in the
middle of the journey, so the middle is a grey blur and the ends crawl. You
arrive with no idea where you came from.

So the camera climbs as it travels and descends as it arrives, along the path
Van Wijk and Nuij derive in [*Smooth and Efficient Zooming and
Panning*](https://www.win.tue.nl/~vanwijk/zoompan.pdf) (InfoVis 2003) — the one
that holds *perceived* speed constant, measured in screen-widths per second
rather than metres. Their closed form is `Flight` in `src/tour.rs`, thirty
lines, no iteration.

The altitude at the top of the arc is not a chosen number: it falls out of the
derivation as the height at which both endpoints are comfortably in frame. Which
means the flight from Ahmedabad to Kapadwanj *shows you the sixty kilometres
between them on the way past*, with a marker on every stop. That frame is the
argument for doing it this way:

```
FLAT   z9.6   tilt 0°
   ◇ ◇ ◇ Ahmedabad ─────────── highway ─────────── Kapadvanj ◈
```

Two tests hold it down. One differentiates the solution and checks the invariant
`ρ²(du/w)² + (dw/ρw)² = 1` along the path, which is what "constant perceived
speed" actually means — it holds to 1.02×. The other measures peak pan rate
against the naive interpolation over the same endpoints and requires the arc to
beat it by 6×; it wins by considerably more.

**It opens from orbit.** The first thing the tour does is not the first stop —
it is the whole country, flat, with a marker on every place the tour will visit,
clustered in one corner of Gujarat. Then it descends ten zoom levels into
Kapadwanj and the ground tilts up. Nothing before it establishes where in the
world any of this is, so the opening is stretched against a normal hop
(`OPEN_STRETCH`) and allowed to run past the usual ceiling.

**Travel is flat, arrival is 2.5D.** The camera drops to straight-down in the
first third of the flight and crosses the region as a plain map, because that is
the legible way to see a path across a region. Only once it has stopped moving
does the ground tilt up. Doing both at once muddies both — the tilt reads as
wobble and the motion hides the tilt.

**The caption gets out of the way.** It fades out on the place you are leaving,
is absent for the middle of the flight, and fades back in on the place you are
arriving at. Holding a name up while the world rushes past underneath asks the
reader to do two things at once, and the name means nothing until you can see
what it is attached to.

**No box.** Under the caption the map dissolves to nothing and comes back over
four rows, the same smoothstep the ground slab uses at its own edge. A bordered
panel would say "here is some chrome"; ground running out says "there is nothing
up here but sky", which is also what the far distance of a tilted map looks like.

The tour draws in whatever road mode the map is in — braille by default, so it
reads as a fine stipple engraving rather than a diagram. `r` still cycles it
mid-tour; `blocks` is worth a look at the arrivals, where continuous strokes
make the street grid read harder.

Stops live in `data/places.txt` — see [INTEGRATING.md](INTEGRATING.md).

### What it costs

Measured over a pty at 180×48, which is the number that matters because this is
served over SSH:

| | |
|---|---|
| idle, camera at rest | **0.5 KB/s** |
| during a flight | **~1.0 MB/s** |

A flight is a two-to-three second burst, so about 2.5 MB per keypress. It is
expensive because a full-screen dithered map is a full-screen texture, and
animating it means retransmitting nearly every cell: at ~11 bytes per cell
(three for the UTF-8 glyph, eight for the colour) a frame is ~100 KB and there
is not much fat left to cut. Two things already cut it 43% from where it started:

- **The brightness ramp is quantised** to 24 steps (`LEVELS` in `canvas.rs`).
  Continuous luminance gives almost every cell its own RGB value, so no two
  neighbours ever share a style and every single cell needs its own escape.
  The ramp is finer than the eye resolves on a dark ground; the frames are not.
- **Neutral cells are emitted as palette indices, not truecolor.** On the
  monochrome map every cell is a grey, and xterm's indices 232–255 *are* a
  24-step grey ramp — the same 24 steps. `ESC[38;5;Nm` against
  `ESC[38;2;R;G;Bm` is eight bytes a cell for an identical picture. Anything
  with real hue keeps truecolor, because the 6×6×6 cube would shift it.

If the link cannot keep up the flight plays late rather than skipping: `dt` is
clamped to 100 ms per frame, so a stalled connection slows the animation instead
of teleporting the camera to the end of it.

## Data

Two backends behind one interface (`src/tiles.rs`), so the renderer never learns
which it is talking to.

### PMTiles basemap — country scale

Point it at any PMTiles v3 archive of MVT tiles:

```bash
termap path/to/basemap.pmtiles
TERMAP_BASEMAP=/path/to/basemap.pmtiles termap
```

Tiles are read on demand and cached, so archive size stops mattering:

| | |
|---|---|
| archive | 1.6 GB, all of India, z4–z14 |
| resident memory | **19 MB** |
| frame at z13 (street level) | 1.8 ms |
| frame at z5 (whole country) | 11.6 ms |

That is the whole reason for this backend. Loading features eagerly costs
~220 bytes each, which caps out around 2–3M features — not enough for a country
at street level. Streaming tiles removes the ceiling entirely.

Both the PMTiles directory format and the slice of MVT that matters are parsed
by hand (`src/pmtiles.rs`, `src/mvt.rs`). The published crates are async-first
and would have dragged a runtime into an otherwise synchronous program; the two
formats together are about 400 lines. `scripts/probe_pmtiles.py` dumps an
archive's schema, which is how the layer and rank tables were derived — from
what the tiles actually contain rather than from documentation.

### Overlays — state borders and names

The basemap has neither: planetiler's default profile drops admin boundaries,
and its `places` layer stops at city level, so a national view came out with no
state divisions and no state names.

Natural Earth admin-1 fills the gap. It is public domain, named, and small
enough that streaming it would be pure overhead, so it loads once and composites
over whatever the backend returns:

```bash
curl -LO https://naciscdn.org/naturalearth/10m/cultural/ne_10m_admin_1_states_provinces.zip
unzip -d data/ne ne_10m_admin_1_states_provinces.zip
python3 scripts/ne2tmap.py data/ne/ne_10m_admin_1_states_provinces IN data/states.tmap
```

`data/states.tmap` is picked up automatically. Swap `IN` for another ISO code to
overlay a different country. The shapefile is parsed by hand — `.shp` is a
documented binary format and `.dbf` is dBase III, which is far less trouble than
a GDAL dependency. (Watch out: dBase pads with NUL, not spaces, so a naive
`.strip()` leaves every name unusable.)

### .tmap — a single region

The app also ships a hand-traced sketch of Mumbai (`assets/mumbai-sample.tmap`)
so it runs with no data at all. It is recognisable, not accurate, and thins out
badly above z12 — that is the sample, not the renderer.

To build a real one:

```bash
python3 scripts/fetch_osm.py       # Overpass -> data/mumbai.tmap
```

Overpass will 504 on a single query that asks for every road in a city, so the
work is split into 16 chunks, each cached under `data/raw/`. Re-running skips
whatever already succeeded, so a timeout costs one chunk rather than the whole
fetch. Four mirrors are tried in turn. A Mumbai extract is ~56,000 features.

Set `BBOX="S,W,N,E"` to fetch somewhere else. `data/*.tmap` is picked up
automatically on next launch.

Two things about OSM worth knowing, both handled in `scripts/osm2tmap.py`:

- **There is no ocean.** Sea is implied by coastline direction (land on the
  left), not stored as an area. The converter stitches coastline ways into
  closed land rings, closing open chains against the bbox; the renderer then
  dithers the whole viewport and erases the land.
- **Road classes are not comparable between cities.** `primary` is 16% of
  Mumbai's ways and `tertiary` 36%, so the tiers are cut from the actual tag
  histogram rather than the wiki's ordering. Same for landmarks: `railway=station`
  is reliable, `historic=monument` is local statuary, and `amenity=hospital` is
  a thousand clinics.

`.tmap` is a flat text format — one header line, one coordinate line — chosen so
the Rust side needs no JSON or vector-tile dependency:

```
F <layer> <rank> <closed> <npts> <name>
<lon> <lat> <lon> <lat> ...
```

## Development

```bash
cargo test                                    # projection + zoom invariants
./target/debug/termap --snapshot 160x46       # one frame of ANSI to stdout
./target/debug/termap --snapshot 160x46 --plain --zoom 12
./target/debug/termap --snapshot 160x46 | python3 scripts/ansi2png.py out.png
```

`--snapshot` renders a single frame and exits, which makes style changes
reviewable without an interactive terminal. `--cursor X,Y` and `--focus MODE`
place the depth focus so the effect can be diffed across edits.

## Fonts

The default `lines` mode needs only box-drawing (U+2500) and quadrants (U+2580),
which every monospace font ships — so it works anywhere.

Ground textures use Braille Patterns (U+2800–U+28FF), and that block is missing
from a surprising number of coding fonts, JetBrains Mono and Noto Sans Mono among
them; they fall back to another font at the terminal's discretion. If terrain
renders as boxes, point your terminal at a font that has braille (DejaVu Sans
does) or add a fallback entry.

## Layout

```
src/
  geo.rs       Web Mercator, viewport, pan/zoom/fit  (+ tests)
  tiles.rs     the two backends: resident .tmap, or streamed PMTiles
  pmtiles.rs   PMTiles v3 archive reader (header, directories, tile fetch)
  mvt.rs       vector-tile decode and the basemap schema mapping
  terrain.rs   heightmap reader
  relief.rs    the 2.5D terrain pass: sample, hillshade, ribbon, occlude
  data.rs      Layer/Feature model and the .tmap reader
  canvas.rs    subpixel coverage+depth buffer, braille resolve, hit-test
  raster.rs    clipped lines, dashes, dithered fills
  style.rs     per-layer style table and the depth field
  labels.rs    collision-avoiding placement with leader lines
  scene.rs     culls, orders, and draws one frame
  ui.rs        header, layer panel, scalebar, status, help
  app.rs       state and input
  snapshot.rs  single-frame render for development
```
