# skysheet

Projects and skills as one night sky. Every project is a constellation, every
skill is a star, and a skill two projects share is drawn once and belongs to
both — so the geometry *is* the provenance. Click a project and the camera flies
into it: the description sets in the corner, its skills stay in the sky around
it, and clicking one of those replaces the description with the story of where
it came from.

```
cargo run --release
```

It reads one hand-edited text file, [`data/skills.sky`](data/skills.sky), which
is compiled into the binary. Editing that file is the whole content workflow.

## There is no panel

The text is not in a sidebar, a modal or a box. It is set in the top-left of the
frame, and the constellation is *moved out of its way* — the camera does not
point at the middle of the screen, it points wherever the typography left room.
Opening a project is one picture, not two regions of an interface.

Three things follow from that, and each of them is a decision the frame would
be worse without.

**The layout springs have a rest length,** so a constellation is a ring rather
than a blob. Skills sit *around* their project instead of piled on it, which is
both what a constellation is and what makes the figure recognisable at a glance.

**The ring is an ellipse, not a circle,** roughly 42 × 25 sky units. A terminal
is much wider than it is tall, and so is a paragraph; a circular figure sized to
fill a frame horizontally is then twice as tall as the frame.

**Only the skills a project has to itself are dealt places on the ring.** A
shared skill is going to be pulled out of the circle toward whatever else claims
it, so allotting it a slot leaves a gap there and the ring comes out as an arc
with bites missing. Those seed at their own centre of gravity instead, and
arrive where they were always going to end up — outside the figure, with a line
still reaching back to it.

The clearing under the text is a four-step ramp rather than a hole. A hard edge
reads as a box drawn on the sky, which is the one thing this composition is
trying not to be: there is no panel, so there must be no panel *shape*.

## Brightness is a count, not a rating

The one number here that could have been a self-assessed proficiency score
deliberately is not.

```rust
0.16 + 0.19 * (projects - 1) + 0.15 * load_bearing
```

`projects` is how many list the skill; `load_bearing` is how many marked it with
a `*`. Both are facts about the sheet, and the sheet is checkable against the
repositories. Nothing in the render encodes an opinion about how good anyone is
at anything — a bar chart at 88% is unfalsifiable, and a skills section full of
them is the one thing a portfolio cannot afford to be.

Reuse is weighted above load-bearing on purpose. It is the harder of the two to
overstate: a second project either used the thing or it did not.

## The layout is an argument

Star positions are not typed in. Every star is sprung toward the ring of each
project that claims it and repelled by every other star, and the simulation runs
to rest. A skill used once settles on that project's ring. A skill used by four
is pulled four ways and comes to rest between them, with four dashed lines still
reaching back.

That is the entire provenance claim, made structurally rather than in a caption:
**a shared skill is visibly shared, because it could not have ended up anywhere
else.**

Two more decisions inside it did real work.

**The spring leans toward where the skill was learned,** two and a half to one.
Weighting the projects equally puts a two-project skill exactly halfway between
them — which is inside neither figure, so you open a project and its most
important skill is off the edge, sitting in the gap. The sheet already names
which project the story happened in; the layout may as well believe it.

**Every star is sprung with the same total stiffness,** by dividing the pull by
the number of claims. Summing the springs instead makes a four-project skill
four times as rigid as a one-project skill — it stops being *placed* by the sky
and starts anchoring it, shoving its neighbours out of shape on the way.

**Figures are a minimum spanning tree.** Constellation lines are a drawn
convention, not a fact about stars, and an MST is the honest version of that
convention: join the sky's own nearest neighbours and stop. Anything denser
starts asserting relationships between skills that the sheet never claimed.

Nothing in here reads a clock or an RNG — jitter comes from hashing the star's
own id — so the sky is identical on every machine and for every visitor, and a
screenshot stays true. There is a test that asserts it.

## What makes it look like that

It draws on termap's canvas: a 2×4 braille grid per cell, so a 160×46 terminal
is really a 320×184 framebuffer, with a float *coverage* and a *depth* per
subpixel rather than an on/off bit.

**A star has a profile, not a position.** One dot is a dot. A gaussian core two
or three subpixels across, with diffraction spikes on the brightest, is what
every photograph of the sky has trained the eye to read as a star. The floor is
five subpixels — a centre and its four neighbours — because the background field
is one dot each, and a skill that also resolves to one dot is invisible among
them.

**Stars have a size in the sky as well as a floor in pixels,** and take
whichever is larger. Pulled back, everything is a point and magnitude reads
through the spikes; pushed in, the bright ones open up and the figure gains
structure instead of just getting further apart.

**Short figure edges draw solid, long ones dash.** A dash pattern needs room to
be a pattern: pulled back to the whole sky an edge is six or eight subpixels,
and two dashes of a dotted line are indistinguishable from two more stars. That
threshold is most of what makes a constellation read as a shape rather than a
cluster.

**The dust is dithered, not shaded.** A band drawn at uniform low coverage
lights one dot in every cell and comes out as flat grey wash. Run through an 8×8
ordered matrix it becomes a stipple, and a stipple reads as *behind* before
brightness has any say. The intensity field is in sky coordinates so the band
pans with the stars, but the threshold is per subpixel, so the stipple stays put
while you drag — both fixed to the sky would shimmer, both fixed to the screen
would leave the band painted onto the glass.

**Dimming is depth, not grey.** Opening a project pushes everything else *back*
rather than desaturating it, so the sky keeps its structure while one figure
comes forward. Depth and focus compose, so a dimmed bright star still outranks a
dimmed faint one.

**Mode follows zoom, and follows attention.** Pulled back to the whole sheet the
dust is what gives the frame depth; pushed in to read one project it is nine
hundred dots of noise between the reader and six words of prose. Both the band
and the field thin out as you zoom, and take another cut when a project is open.

## Controls

| | |
|---|---|
| click | open a project, or one of its skills |
| `1`–`9` | open a project by number; again to release |
| `n` `p` | walk the skills of the open project |
| `esc` | back out one layer |
| drag | pan |
| wheel | zoom, anchored under the cursor |
| `h` `j` `k` `l` | pan |
| `+` `-` | zoom |
| `0` `g` | back to the whole sky |
| `/` | find a skill, a story, or a project |
| `pgup` `pgdn` | scroll a story too long for the terminal |
| `s` `f` | dust / constellation figures |
| `m` | monochrome |
| `?` `q` | help, quit |

Names are what you click, not stars. At wide zoom a star is two subpixels across
and its label is fifteen cells; asking the reader to hit the star is asking them
to hit the wrong target. Project names are placed dead centre of their own ring
— the same hole the description drops into — so the sky doubles as the menu.

`esc` backs out one layer at a time: query, then skill, then project.

Search is ranked, not just filtered. Stories are searched too, which is most of
the value (`drop` finds nftables, `median` finds Theil–Sen), but it also means a
two-letter query hits every *al**go**rithm* in the sheet. An exact name wins,
then a prefix, then project names, then the prose.

## It fits the terminal it is given

The text block takes 42% of the width, clamped to 34–58 columns, and the camera
is placed from whatever is left. Which arm of the resulting L the constellation
goes in depends on which is bigger: beside the text on a wide terminal, under it
when the block is short, and — on a terminal too small for either — overlapping,
pushed down and right so the collision lands where the text is thinnest.

Below about thirty rows a project drops its long description and keeps the
one-liner and the numbers, because a constellation squeezed into what a full
paragraph leaves over is not a constellation. A *skill's* story is never
shortened: it is the entire content of that screen, so it overflows and scrolls,
and the bottom edge says so.

## The sheet

```
constellation netjail
  name   netjail
  year   Jul–Aug 2026
  repo   github.com/Prince2412k2/netjail
  at     -105 -95
  blurb  Run any command with only its network sandboxed.
  about
    A Linux sandbox that isolates what a process can reach on the network
    while leaving everything else untouched — same filesystem, same $HOME …
  stats  Go · 16,700 LOC · 28 packages · 6,100 lines of tests (37%)

star nftables
  name   nftables
  in     netjail*
  story
    Started with per-host allow rules, which is the wrong shape: anything
    nobody thought of yet is permitted. Default-drop and punch holes is the
    only ordering that fails closed …
```

Two spaces starts a key, four or more continues the previous value. That is the
entire grammar — a flat indented format rather than TOML or JSON for the same
reason termap has `.tmap`: this file is meant to be edited by hand and it is the
only real content in the program, so a serialisation crate would cost a
dependency and buy nothing.

| | |
|---|---|
| `blurb` | the one-line version |
| `about` | the paragraph, shown when the project is opened |
| `stats` | languages, size, and whatever else is countable |
| `in` | the constellations that claim a skill. **The first is where the story happened.** A `*` marks a project that leans on it. |
| `at` | the constellation's anchor. The one thing here meant to be tuned by hand; keep neighbours at least a ring apart or two projects overlap. |
| `story` | the point of the app. Not what the skill is — what that project taught about it. |

Everything is validated at load: an unknown key, a star in no constellation, an
`in` naming a project that does not exist, a project with no description. A
malformed sheet fails with a line number rather than rendering a wrong sky.

```bash
skysheet path/to/other.sky      # read a different sheet instead of the built-in one
```

## Development

```bash
cargo test                                     # parser, layout, camera, cards, input
./target/release/skysheet --snapshot 140x40    # one frame of ANSI to stdout
./target/release/skysheet --snapshot 140x40 --plain --focus netjail
./target/release/skysheet --snapshot 140x40 --select nftables | python3 ../map/scripts/ansi2png.py out.png
```

`--snapshot` renders a single frame and exits. Every rendering decision here was
made this way: a star field is almost entirely a matter of thresholds — how
faint the dust is, how far the spikes reach, when a label is worth drawing — and
none of those can be judged from a description of the change. Diff the frames.

Flags: `--focus ID --select ID --find QUERY --zoom Z --cursor X,Y --plain
--keys --no-dust --no-figures`.

```bash
skysheet --layout            # what the simulation settled on, per constellation
skysheet --layout netjail    # every star in one, and how far it drifted
```

Tuning an `at` anchor blind is guesswork — a constellation can look wrong
because its anchor is badly placed, or because half its stars are shared and
being pulled elsewhere, and those want opposite fixes. `--layout` is how you
tell which.

## Cost

The sky does not animate, so the event loop blocks and redraws only when
something changed. An idle session is zero CPU and zero bytes on the wire, which
is the point when the audience arrives over SSH.

Layout is solved once at startup: 700 steps of an O(n²) simulation over 59
stars, about a millisecond. Nothing is recomputed while panning.

## Layout

```
src/
  data.rs      the .sky sheet: parser, model, and the magnitude rule  (+ tests)
  layout.rs    the ring simulation and the per-constellation MST      (+ tests)
  sky.rs       the camera: pan, anchored zoom, fit, place             (+ tests)
  card.rs      the text block, and the clearing it opens in the sky   (+ tests)
  canvas.rs    subpixel coverage+depth buffer, braille resolve, hit-test
  labels.rs    collision-avoiding placement — verbatim from termap
  draw.rs      dust, field, figures, stars, names                     (+ tests)
  ui.rs        header, status, help, and the order the frame is built in
  app.rs       state, input, and where the camera goes                (+ tests)
  snapshot.rs  single-frame render for development
```

The frame is built in a strict order: the sky is painted, then the card fades a
clearing in it and writes into the gap, then project names claim their space,
and only then are the star names placed around whatever is left. Do it in any
other order and a label lands under the description, or the description lands on
a star, and there is no third layer to arbitrate.

`canvas.rs` is termap's with the road machinery removed — a sky has no junctions
to resolve — and `labels.rs` is that file untouched. Both are kept deliberately
close so that when the apps are combined they dedupe to one copy rather than a
diff to reconcile.
