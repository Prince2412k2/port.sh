//! Terminal chrome: header, side panel, scalebar, status bar, help.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use ratatui::Frame;

use crate::app::{App, LAYER_KEYS, TOGGLES};
use crate::canvas::{Canvas, Theme};
use crate::geo::PIXEL_ASPECT;
use crate::scene::{self, SceneOpts};

// The chrome's three greys. Functions of the theme rather than constants,
// because the map is drawn on two different grounds now and a fixed
// near-black background under paper-coloured ink is just a black rectangle.
//
// Threaded rather than kept in a global: one process serves many sessions over
// SSH, and a global theme would be whichever visitor toggled it last.

const PANEL_W: u16 = 26;
/// Below this the panel is more trouble than it is worth.
const PANEL_MIN_TOTAL_W: u16 = 96;

pub fn render(f: &mut Frame, app: &mut App) {
    render_in(f, f.area(), app)
}

/// Draw only the map — no header, no status line, no side panel.
///
/// For an embedder that has its own chrome. The alternative, letting this
/// crate draw its header inside someone else's layout, produces two title bars
/// and two footers, which is precisely how a combined app announces that it is
/// three apps in a trench coat.
pub fn render_map_only(f: &mut Frame, area: Rect, app: &mut App) {
    let th = app.theme;
    Block::default()
        .style(Style::default().bg(th.page()))
        .render(area, f.buffer_mut());
    map_view(f, area, app);
    overlays(f, area, app);
    if app.show_help {
        help(f, area, th);
    }
}

/// Draw a still map of one point, and leave the camera where it was.
///
/// A thumbnail, for somewhere that is not the map section: the portfolio's chat
/// puts one at the side when a visitor asks where something is. It borrows the
/// caller's `App` rather than taking one of its own, because a second `App` is a
/// second copy of the terrain and the overlays -- so the whole of the borrowing
/// is `park_camera` and `unpark_camera` around one draw.
///
/// `tilt` is radians and `persp` a strength. `pin` drops a marker on the centre:
/// 0 has it high above the point and 1 has it landed. Neither is a decision this function
/// makes -- both are read off a clock by the caller, so a frame is a pure
/// function of its time like everything else here.
///
/// The pin exists because a view of a city with nothing on it does not read as
/// pointing at anywhere. Tour stops draw their own from the places sheet; a
/// point handed to us by an agent has nothing but this.
pub struct Camera {
    /// Longitude then latitude, in that order.
    pub lonlat: (f64, f64),
    pub zoom: f64,
    /// Radians. Zero looks straight down.
    pub tilt: f64,
    /// Convergence strength. Zero is a parallel projection.
    pub persp: f64,
    /// Radians clockwise from north-up.
    pub bearing: f64,
}

pub fn render_locator(f: &mut Frame, area: Rect, app: &mut App, cam: Camera, pin: Option<f32>) {
    let th = app.theme;
    let Camera {
        lonlat,
        zoom,
        tilt,
        persp,
        bearing,
    } = cam;
    if area.width < 8 || area.height < 4 {
        return;
    }
    Block::default()
        .style(Style::default().bg(th.page()))
        .render(area, f.buffer_mut());
    let mut vp = crate::geo::Viewport::new(crate::geo::lonlat_to_world(lonlat.0, lonlat.1), zoom);
    vp.tilt = tilt;
    vp.persp = persp;
    vp.bearing = bearing;
    let was = app.park_viewport(vp);
    map_view(f, area, app);
    app.unpark_camera(was);

    if let Some(drop) = pin {
        drop_pin(f, area, drop, th);
    }
}

/// A marker falling onto the middle of the map.
///
/// Two cells while it is in the air -- the head and a streak under it -- and one
/// once it has landed, with a ring that opens and fades. Bounded, like
/// everything else that moves in this project: it plays once on arrival and then
/// it is a marker sitting on a point.
fn drop_pin(f: &mut Frame, area: Rect, drop: f32, th: Theme) {
    let (cx, cy) = (area.x + area.width / 2, area.y + area.height / 2);
    let k = drop.clamp(0.0, 1.0);

    if k < 1.0 {
        // Accelerating, so it falls rather than drifts: the eye reads constant
        // speed as a hover.
        let fall = 1.0 - k * k;
        let up = (fall * (area.height as f32 / 2.0).min(7.0)).round() as u16;
        let y = cy.saturating_sub(up);
        let cell = |f: &mut Frame, y: u16, ch: &str, c: Color, bold: bool| {
            if y >= area.y && y < area.y + area.height {
                let mut st = Style::default().fg(c);
                if bold {
                    st = st.add_modifier(Modifier::BOLD);
                }
                f.render_widget(
                    Paragraph::new(Span::styled(ch.to_string(), st)),
                    Rect {
                        x: cx,
                        y,
                        width: 1,
                        height: 1,
                    },
                );
            }
        };
        cell(f, y, "\u{25be}", th.amber(), true);
        if up > 0 {
            cell(f, y.saturating_sub(1), "\u{2502}", th.faint(), false);
        }
        return;
    }

    // Landed. The same glyph the tour uses for the stop you are on, so the two
    // maps in this app agree about what "here" looks like.
    f.render_widget(
        Paragraph::new(Span::styled(
            "\u{25c8}".to_string(),
            Style::default().fg(th.amber()).add_modifier(Modifier::BOLD),
        )),
        Rect {
            x: cx,
            y: cy,
            width: 1,
            height: 1,
        },
    );
}

/// Draw the whole map UI into `area` rather than the whole frame.
///
/// The portfolio embeds this below its own section rail. Header and status stay
/// with it: they are the map's instruments — zoom, mode, what is under the
/// pointer — and belong to the view, not to the shell around it.
pub fn render_in(f: &mut Frame, area: Rect, app: &mut App) {
    let th = app.theme;
    Block::default()
        .style(Style::default().bg(th.page()))
        .render(area, f.buffer_mut());

    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(area);

    let show_panel = app.show_panel && area.width >= PANEL_MIN_TOTAL_W;
    let (panel, map) = if show_panel {
        let [p, m] =
            Layout::horizontal([Constraint::Length(PANEL_W), Constraint::Min(1)]).areas(body);
        (Some(p), m)
    } else {
        (None, body)
    };

    header(f, head, app);
    if let Some(p) = panel {
        side_panel(f, p, app);
    }
    map_view(f, map, app);
    overlays(f, map, app);
    status(f, foot, app);

    if app.show_help {
        help(f, area, th);
    }
}

fn header(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let (lon, lat) = app.vp.center_lonlat();
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    let ew = if lon >= 0.0 { 'E' } else { 'W' };

    let left = Line::from(vec![
        Span::styled(
            " termap ",
            Style::default()
                .fg(th.page())
                .bg(th.ink())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" 0.1.0", Style::default().fg(th.faint())),
        Span::styled("  │  ", Style::default().fg(th.ghost())),
        Span::styled(
            app.source.label().to_string(),
            Style::default().fg(th.faint()),
        ),
    ]);

    let right = Line::from(vec![
        Span::styled(
            format!("{:.4} {ns}  {:.4} {ew}", lat.abs(), lon.abs()),
            Style::default().fg(th.ink()),
        ),
        Span::styled("  │  ", Style::default().fg(th.ghost())),
        Span::styled(
            format!("z{:.1}", app.vp.zoom),
            Style::default().fg(th.ink()),
        ),
        Span::styled("  │  ", Style::default().fg(th.ghost())),
        Span::styled(
            if app.vp.is_flat() {
                String::new()
            } else {
                format!(
                    "tilt {:.0}°  bear {:.0}°  │  ",
                    app.vp.tilt.to_degrees(),
                    app.vp.bearing.to_degrees().rem_euclid(360.0)
                )
            },
            Style::default().fg(th.faint()),
        ),
        Span::styled(
            format!("depth:{} ", app.focus.label()),
            Style::default().fg(th.amber()),
        ),
    ]);

    f.render_widget(Paragraph::new(left), area);
    f.render_widget(Paragraph::new(right).right_aligned(), area);
}

fn side_panel(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(th.ghost()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    let hdr = |s: &str| {
        Line::from(Span::styled(
            s.to_string(),
            Style::default().fg(th.faint()).add_modifier(Modifier::BOLD),
        ))
    };

    lines.push(hdr(" LAYERS"));
    for (i, layer) in TOGGLES.iter().enumerate() {
        let on = app.layers[layer.index()];
        let n: usize = app
            .tiles
            .iter()
            .map(|t| t.by_layer[layer.index()].len())
            .sum();
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", if on { "▣" } else { "▢" }),
                Style::default().fg(if on { th.ink() } else { th.ghost() }),
            ),
            // The key that actually toggles it, straight from the table the key
            // handler reads. This printed `i + 1` and so advertised a digit --
            // which, embedded in the portfolio, moved you to another section.
            Span::styled(
                format!("{} ", LAYER_KEYS[i]),
                Style::default().fg(th.ghost()),
            ),
            Span::styled(
                format!("{:<13}", layer.label()),
                Style::default().fg(if on { th.ink() } else { th.ghost() }),
            ),
            Span::styled(format!("{n:>5}"), Style::default().fg(th.ghost())),
        ]));
    }

    lines.push(Line::default());
    lines.push(hdr(" DEPTH"));
    // The ramp is generated by the same fog curve the map uses, so it is a real
    // key rather than a drawing of one.
    for (d, tag) in [
        (0.05, "near"),
        (0.28, ""),
        (0.50, ""),
        (0.72, ""),
        (0.95, "far"),
    ] {
        let l = app.fog.factor(d).clamp(0.0, 1.0);
        // Through the theme, not composited here: this is a key to the map, so
        // it has to be wrong in exactly the same direction the map is. Written
        // out longhand it drew the paper ramp upside down -- near faint, far
        // solid -- and a legend that disagrees with the thing it explains is
        // worse than no legend.
        let c = th.paint(crate::canvas::TINT_MONO as usize, l, true, false);
        lines.push(Line::from(vec![
            Span::styled(" ⣿⣿⣿⣿⣿⣿⣿⣿ ", Style::default().fg(c)),
            Span::styled(tag.to_string(), Style::default().fg(th.ghost())),
        ]));
    }

    lines.push(Line::default());
    lines.push(hdr(" CONTROLS"));
    for (k, v) in [
        ("drag", "pan"),
        ("wheel", "zoom at cursor"),
        ("click", "pin feature"),
        ("hjkl", "pan"),
        ("+ -", "zoom"),
        ("f", "depth focus"),
        ("r", "road glyphs"),
        ("[ ]", "road weight"),
        ("u o", "tilt"),
        (", .", "rotate"),
        ("m", "auto / manual cam"),
        ("x", "my location"),
        ("c", "colour / mono"),
        ("i", "ink / light"),
        ("t", "labels"),
        ("p", "panel"),
        ("g", "recentre"),
        ("?", "help"),
        ("q", "quit"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!(" {k:<6}"), Style::default().fg(th.ink())),
            Span::styled(v.to_string(), Style::default().fg(th.faint())),
        ]));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn map_view(f: &mut Frame, area: Rect, app: &mut App) {
    let th = app.theme;
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Resolve hover against the previous frame before the canvas is cleared.
    app.map_area = area;
    app.update_hover();

    let (cw, ch) = (area.width as usize, area.height as usize);
    if app.canvas.cw != cw || app.canvas.ch != ch {
        app.canvas = Canvas::new(cw, ch);
    } else {
        app.canvas.clear();
    }
    app.vp.sw = app.canvas.sw as f64;
    app.vp.sh = app.canvas.sh as f64;
    app.fit_if_pending();
    app.open_tour_if_pending();
    app.sync_camera();
    // Tiles are resolved before drawing; a static source just hands back its one.
    app.tiles = app.source.tiles(&app.vp);
    if app.query.is_some() {
        // The gazetteer is whatever is loaded, and what is loaded changes as
        // the view moves — so the results have to be recomputed, not cached.
        app.refresh_hits();
    }

    // Ground level under the view: everything vertical is measured from here.
    // Worked out first so the label placer can treat it as occupied space.
    let sb = scalebar_geom(area, app);

    let t0 = std::time::Instant::now();
    let ground = if app.show_terrain {
        crate::view::ground_strength(app.vp.zoom)
    } else {
        0.0
    };
    // Terrain first: it is the ground everything else sits on, the depth
    // buffer sorts out what ends up hidden behind a ridge, and the exposure it
    // picks is what everything draped on it has to be lifted by.
    let mut relief_pts = 0;
    if ground > 0.0 {
        if let Some(t) = app.source.terrain {
            relief_pts = app.relief.draw(
                t,
                &mut app.canvas,
                &app.vp,
                crate::relief::Plot {
                    strength: ground,
                    theme: th,
                },
            );
        }
    }
    let lift = app.relief.lift;
    let home = app
        .home
        .lock()
        .unwrap()
        .clone()
        .map(|fix| crate::scene::HomeMarker {
            world: fix.world,
            accuracy_km: fix.accuracy_km,
        });
    let places: Vec<_> = if app.tour.active {
        app.tour
            .places
            .iter()
            .map(|place| crate::scene::PlaceMarker {
                world: place.world,
                detail: crate::data::stable_detail_id("authored-place", &place.id),
            })
            .collect()
    } else {
        Vec::new()
    };
    let terrain: Option<&dyn crate::scene::Elevation> = if ground > 0.0 {
        app.source
            .terrain
            .map(|terrain| terrain as &dyn crate::scene::Elevation)
    } else {
        None
    };
    let opts = SceneOpts {
        vp: &app.vp,
        layers: app.layers,
        depth: &app.depth_field(),
        highlight: app.highlight(),
        show_labels: app.show_labels,
        road_glyph: app.road_glyph,
        mode: app.mode(),
        terrain,
        exag: lift.exag,
        datum: lift.datum,
        home,
        road_weight: app.road_weight,
        reserved: sb.as_ref().map(|s| s.local),
        places: &places,
        place_at: app.tour.at,
    };
    let tile_refs: Vec<_> = app.tiles.iter().map(|tile| tile.as_ref()).collect();
    app.stats = scene::draw(&tile_refs, &mut app.canvas, &opts);
    app.stats.relief = relief_pts;
    app.canvas
        .resolve(f.buffer_mut(), area, &app.fog, app.mono, th);
    app.frame_us = t0.elapsed().as_micros();
}

/// The things that sit on top of the map rather than in it: the tour's card,
/// the search box, the one-time nudge.
///
/// Split out of `map_view` for the thumbnail. A locator is a picture of a
/// place, and a picture of a place with somebody else's tour caption across it
/// -- which is what it drew at first -- is a picture of nothing in particular.
fn overlays(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    // The scalebar is an instrument, and instruments are chrome. It stayed in
    // `map_view` on the grounds that scale belongs to the map -- true of a map
    // you are driving, and wrong for a thumbnail that is trying to read as part
    // of the page rather than as a window onto one. A ruled bar in the corner is
    // the most frame-like thing on it.
    if let Some(sb) = scalebar_geom(area, app) {
        draw_scalebar(f, area, &sb, th);
    }
    place_card(f, area, app);
    search_box(f, area, app);
    hint(f, area, app);
}

/// The one-time nudge that says the map is not a picture.
fn hint(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let a = app.hint_alpha();
    if a <= 0.01 || app.query.is_some() || area.width < 50 || area.height < 10 {
        return;
    }
    let parts: [(&str, bool); 6] = [
        ("drag", true),
        (" to pan    ", false),
        ("wheel", true),
        (" to zoom    ", false),
        ("?", true),
        (" to find a place", false),
    ];
    let w: u16 = parts.iter().map(|(s, _)| s.chars().count() as u16).sum();
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + area.height.saturating_sub(3);

    let spans: Vec<Span> = parts
        .iter()
        .map(|(s, key)| {
            Span::styled(
                (*s).to_string(),
                Style::default().fg(fade(if *key { th.cyan() } else { th.faint() }, a, th)),
            )
        })
        .collect();
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x,
            y,
            width: w,
            height: 1,
        },
    );
}

/// Blend toward the page ground, for anything that fades in and out.
fn fade(c: Color, a: f32, th: Theme) -> Color {
    // `ground`, not `page`. The page comes back as a grey-ramp index -- so
    // matching on `Color::Rgb` silently returned the colour unfaded once --
    // and under system it is `Reset`, which has no components at all.
    let Some((r, g, b)) = crate::canvas::rgb_of(c) else {
        return c;
    };
    let (br, bg, bb) = th.ground();
    let mix = |v: u8, t: u8| (t as f32 + (v as f32 - t as f32) * a) as u8;
    crate::canvas::ink(mix(r, br), mix(g, bg), mix(b, bb))
}

/// The location search: a query line and what it found.
///
/// Sits bottom-left over the map rather than in the middle. What you are
/// looking for is usually somewhere on screen already, and a dialogue in the
/// centre covers the thing you are trying to find.
fn search_box(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let Some(q) = &app.query else { return };
    if area.width < 30 || area.height < 8 {
        return;
    }
    let w = 42.min(area.width.saturating_sub(4));
    let rows = (app.hits.len() as u16).min(8);
    let h = rows + 2;
    let x = area.x + 2;
    let y = area.y + area.height.saturating_sub(h + 2);

    // Clear the ground under it, or the map's stipple reads through the text.
    for row in 0..h {
        for col in 0..w {
            if let Some(c) = f.buffer_mut().cell_mut((x + col, y + row)) {
                c.set_char(' ').set_style(Style::default().bg(th.page()));
            }
        }
    }

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("find ", Style::default().fg(th.ghost())),
            Span::styled(
                q.clone(),
                Style::default().fg(th.ink()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▌", Style::default().fg(th.cyan())),
        ])),
        Rect {
            x,
            y,
            width: w,
            height: 1,
        },
    );

    if app.hits.is_empty() {
        let msg = if q.is_empty() {
            "a state, a town, a landmark, a stop on the tour"
        } else {
            "nothing by that name in what is loaded"
        };
        f.render_widget(
            Paragraph::new(Span::styled(
                msg.to_string(),
                Style::default().fg(th.ghost()),
            )),
            Rect {
                x,
                y: y + 1,
                width: w,
                height: 1,
            },
        );
        return;
    }

    for (i, hit) in app.hits.iter().take(rows as usize).enumerate() {
        let on = i == app.hit;
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    if on { "› " } else { "  " },
                    Style::default().fg(if on { th.amber() } else { th.ghost() }),
                ),
                Span::styled(
                    hit.name.clone(),
                    Style::default().fg(if on { th.ink() } else { th.faint() }),
                ),
            ])),
            Rect {
                x,
                y: y + 1 + i as u16,
                width: w.saturating_sub(10),
                height: 1,
            },
        );
        f.render_widget(
            Paragraph::new(Span::styled(
                hit.what.to_string(),
                Style::default().fg(th.ghost()),
            ))
            .right_aligned(),
            Rect {
                x,
                y: y + 1 + i as u16,
                width: w,
                height: 1,
            },
        );
    }
}

/// The caption over the top of the map: what this place is, and what it meant.
///
/// It is not a panel and it has no border. The map is dissolved to nothing
/// underneath it instead — see `fade_band` — so the text sits in cleared air
/// rather than in a box drawn on top of the view. A box would say "here is some
/// chrome"; ground running out says "there is nothing up here but sky", which
/// is also what the far distance of a tilted map actually looks like.
fn place_card(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let Some((p, alpha)) = app.tour.card() else {
        return;
    };
    if alpha <= 0.004 || area.width < 34 || area.height < 12 {
        return;
    }

    const PAD: u16 = 3;
    let w = (area.width.saturating_sub(PAD * 2)).min(74);
    let note = crate::place::wrap(&p.note, w as usize);
    // The note is what the stop actually means, so it gets the room it needs —
    // but not more than a third of the map, or the tour becomes a slideshow of
    // paragraphs with a map behind them.
    let max_note = ((area.height / 3).saturating_sub(4)).clamp(1, 5) as usize;
    let note = &note[..note.len().min(max_note)];

    let rows = 4 + note.len() as u16;
    let band = rows + 2;
    fade_band(f, area, band, 4, alpha, th);

    let x = area.x + PAD;
    let mut y = area.y + 1;
    let line = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>| {
        f.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
        );
    };
    let right = |f: &mut Frame, y: u16, spans: Vec<Span<'static>>| {
        f.render_widget(
            Paragraph::new(Line::from(spans)).right_aligned(),
            Rect {
                x,
                y,
                width: w,
                height: 1,
            },
        );
    };

    // Where you are in the sequence, and which way it runs.
    let pips: Vec<Span> = app
        .tour
        .places
        .iter()
        .enumerate()
        .flat_map(|(i, _)| {
            let here = i == app.tour.at;
            [
                Span::styled(
                    if here { "\u{25cf}" } else { "\u{00b7}" },
                    Style::default().fg(dim(if here { th.amber() } else { th.ghost() }, alpha, th)),
                ),
                Span::styled(" ", Style::default()),
            ]
        })
        .collect();
    line(f, y, pips.into_iter().map(|s| s.to_owned()).collect());
    right(
        f,
        y,
        vec![Span::styled(
            p.kind.to_uppercase(),
            Style::default().fg(dim(th.ghost(), alpha, th)),
        )],
    );

    y += 1;
    line(
        f,
        y,
        vec![Span::styled(
            p.name.to_uppercase(),
            Style::default()
                .fg(dim(th.ink(), alpha, th))
                .add_modifier(Modifier::BOLD),
        )],
    );
    right(
        f,
        y,
        vec![Span::styled(
            p.years.clone(),
            Style::default().fg(dim(th.amber(), alpha, th)),
        )],
    );

    y += 1;
    let (lon, lat) = p.lonlat;
    line(
        f,
        y,
        vec![Span::styled(
            format!("{}  \u{00b7}  {}", p.role, p.where_),
            Style::default().fg(dim(th.faint(), alpha, th)),
        )],
    );
    right(
        f,
        y,
        vec![Span::styled(
            format!("{:.3}\u{00b0}N {:.3}\u{00b0}E", lat, lon),
            Style::default().fg(dim(th.ghost(), alpha, th)),
        )],
    );

    y += 2;
    for l in note {
        line(
            f,
            y,
            vec![Span::styled(
                l.clone(),
                Style::default().fg(dim(th.faint(), alpha, th)),
            )],
        );
        y += 1;
    }
}

/// Dissolve the top of the map into the background.
///
/// `solid` rows go all the way to nothing, then `ramp` rows come back up to the
/// full map. Smoothstepped rather than linear for the same reason the ground
/// slab's own edge is: a linear ramp shows a visible seam at the point it
/// starts, and the seam reads as a horizontal rule nobody drew.
///
/// `alpha` scales the whole effect, so as the caption fades out the map closes
/// back over it instead of leaving a bald strip.
fn fade_band(f: &mut Frame, area: Rect, solid: u16, ramp: u16, alpha: f32, th: Theme) {
    let buf = f.buffer_mut();
    for dy in 0..(solid + ramp).min(area.height) {
        // How much of the map survives at this row, before alpha.
        let keep = if dy < solid {
            0.0
        } else {
            let t = (dy - solid + 1) as f32 / (ramp + 1) as f32;
            t * t * (3.0 - 2.0 * t)
        };
        let k = 1.0 - alpha * (1.0 - keep);
        if k >= 0.999 {
            continue;
        }
        for dx in 0..area.width {
            let Some(cell) = buf.cell_mut((area.x + dx, area.y + dy)) else {
                continue;
            };
            let (fg, bg) = (toward_bg(cell.fg, k, th), toward_bg(cell.bg, k, th));
            cell.set_style(Style::default().fg(fg).bg(bg));
        }
    }
}

/// Blend a colour toward the page background.
///
/// Goes through `canvas::rgb_of` rather than matching `Color::Rgb` directly:
/// the renderer emits palette indices for neutral cells, and a match that only
/// understood truecolor would leave the entire monochrome map unfaded while
/// appearing to work on everything else.
fn toward_bg(c: Color, k: f32, th: Theme) -> Color {
    let Some((r, g, b)) = crate::canvas::rgb_of(c) else {
        return c;
    };
    let (br, bg_, bb) = th.ground();
    let mix = |a: u8, b: u8| {
        (b as f32 + (a as f32 - b as f32) * k)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    crate::canvas::ink(mix(r, br), mix(g, bg_), mix(b, bb))
}

/// Fade a colour toward the background by an opacity.
fn dim(c: Color, alpha: f32, th: Theme) -> Color {
    toward_bg(c, alpha.clamp(0.0, 1.0), th)
}

struct ScaleBar {
    /// Map-local cell coords, so it can be handed to the label occupancy grid.
    local: Rect,
    nums: String,
    bar: String,
}

/// The round number of metres the bar spans, for a cell size and a frame width.
///
/// Split out of `scalebar_geom` because it is the one part of the chrome the
/// rest of the map has an opinion about: "terrain by the time the bar reads
/// 10 km" is a claim about this function and about `view::ground_strength`
/// together, and neither one alone can be tested for it.
fn bar_metres(m_per_cell: f64, width: u16) -> f64 {
    let target_cells = (width as f64 * 0.22).clamp(10.0, 30.0);
    let raw = m_per_cell * target_cells;
    let pow = 10f64.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0]
        .into_iter()
        .map(|m| m * pow)
        .find(|&v| v >= raw * 0.6)
        .unwrap_or(pow)
}

/// Worked out before the scene is drawn so labels can be kept off it.
fn scalebar_geom(area: Rect, app: &App) -> Option<ScaleBar> {
    if area.height < 4 || area.width < 24 {
        return None;
    }
    // A cell is SUB_X subpixels wide; work in cells so the bar lands on cell
    // boundaries and the ticks line up with the digits underneath.
    let m_per_cell = app.vp.meters_per_subpixel() * crate::canvas::SUB_X as f64;
    let nice = bar_metres(m_per_cell, area.width);

    let cells = (nice / m_per_cell).round() as u16;
    if cells < 6 || cells + 2 >= area.width {
        return None;
    }

    let (full, half) = if nice >= 1000.0 {
        (
            format!("{:.0} km", nice / 1000.0),
            format!("{:.1}", nice / 2000.0),
        )
    } else {
        (format!("{nice:.0} m"), format!("{:.0}", nice / 2.0))
    };

    let bar: String = (0..=cells)
        .map(|i| match i {
            0 => '├',
            i if i == cells => '┤',
            i if i == cells / 2 => '┼',
            _ => '─',
        })
        .collect();

    // Lay the numbers out against the tick positions rather than by padding, so
    // they stay put as the bar length changes with zoom.
    let width = cells as usize + full.chars().count() + 2;
    let mut nums: Vec<char> = vec![' '; width];
    let mut put = |at: usize, s: &str| {
        for (i, c) in s.chars().enumerate() {
            if at + i < width {
                nums[at + i] = c;
            }
        }
    };
    put(0, "0");
    put(
        (cells as usize / 2).saturating_sub(half.chars().count() / 2),
        &half,
    );
    put(cells as usize + 2, &full);
    let nums: String = nums.into_iter().collect();

    Some(ScaleBar {
        local: Rect {
            x: 2,
            y: area.height - 3,
            width: (width as u16).min(area.width.saturating_sub(2)),
            height: 2,
        },
        nums,
        bar,
    })
}

fn draw_scalebar(f: &mut Frame, area: Rect, sb: &ScaleBar, th: Theme) {
    // A background strip keeps the map's braille from showing through the text.
    let backing = Style::default().fg(th.faint()).bg(th.page());
    let strip = Rect {
        x: area.x + sb.local.x,
        y: area.y + sb.local.y,
        width: sb.local.width,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(sb.nums.clone(), backing))),
        strip,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(sb.bar.clone(), backing))),
        Rect {
            y: strip.y + 1,
            ..strip
        },
    );
}

fn status(f: &mut Frame, area: Rect, app: &App) {
    let th = app.theme;
    let mut left = vec![Span::styled(
        if app.pinned.is_some() {
            " PINNED "
        } else {
            " NORMAL "
        },
        Style::default()
            .fg(th.page())
            .bg(if app.pinned.is_some() {
                th.cyan()
            } else {
                th.ink()
            })
            .add_modifier(Modifier::BOLD),
    )];
    left.push(Span::styled("  ", Style::default()));

    if let Some(msg) = &app.toast {
        left.push(Span::styled(msg.clone(), Style::default().fg(th.amber())));
    } else if let Some(info) = app.highlight().and_then(|id| app.feature_info(id)) {
        left.push(Span::styled(
            info,
            Style::default().fg(if app.pinned.is_some() {
                th.cyan()
            } else {
                th.ink()
            }),
        ));
    } else {
        left.push(Span::styled(
            "drag to pan · wheel to zoom · ? for help",
            Style::default().fg(th.faint()),
        ));
    }

    let right = Line::from(vec![
        Span::styled(
            format!(
                "{} feat  {} bld  {} lbl  {}k held  ",
                app.stats.features,
                app.stats.buildings,
                app.stats.labels,
                app.source.resident() / 1000
            ),
            Style::default().fg(th.ghost()),
        ),
        Span::styled(
            format!("{:.1} ms ", app.frame_us as f64 / 1000.0),
            Style::default().fg(th.faint()),
        ),
    ]);

    f.render_widget(Paragraph::new(Line::from(left)), area);
    f.render_widget(Paragraph::new(right).right_aligned(), area);
}

fn help(f: &mut Frame, area: Rect, th: Theme) {
    let w = 60.min(area.width.saturating_sub(4));
    let h = 24.min(area.height.saturating_sub(4));
    let popup = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };

    f.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.faint()))
        .style(Style::default().bg(th.page()))
        .title(Span::styled(
            " termap ",
            Style::default().fg(th.amber()).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let row = |k: &str, v: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<12}"), Style::default().fg(th.ink())),
            Span::styled(v.to_string(), Style::default().fg(th.faint())),
        ])
    };

    let lines = vec![
        Line::from(Span::styled(
            "  A map drawn in braille subpixels.",
            Style::default()
                .fg(th.faint())
                .add_modifier(Modifier::ITALIC),
        )),
        Line::default(),
        row("drag", "pan the map"),
        row("wheel", "zoom, anchored at the cursor"),
        row("click", "pin a feature (esc unpins)"),
        row("h j k l", "pan left/down/up/right"),
        row("+ -", "zoom in / out"),
        row("f", "cycle depth focus off/subtle/strong"),
        // Shift and a digit. Not the digits themselves: the portfolio that
        // embeds this map owns those for moving between sections.
        row("! @ # $ % ^ & *", "toggle a layer (see the panel)"),
        row(")", "all layers on"),
        row("x", "my location"),
        row("i", "ink on paper / light on black"),
        row("t", "toggle labels"),
        row("p", "toggle side panel"),
        row("g", "recentre on the data"),
        row("?", "find a place"),
        row("q", "quit"),
        Line::default(),
        Line::from(Span::styled(
            "  The experience tour",
            Style::default().fg(th.amber()),
        )),
        row("e", "fly the tour / leave it"),
        row("n  tab", "next place"),
        row("b", "previous place"),
        row("enter", "replay the arrival"),
    ];

    f.render_widget(Paragraph::new(lines), inner);
    let _ = PIXEL_ASPECT;
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// The exact shape of a bug that was live: the renderer emits palette
    /// indices for neutral cells, `toward_bg` only understood `Color::Rgb`, and
    /// so the fade band silently stopped clearing the monochrome map while
    /// still appearing to work on the coloured parts. Fully faded means gone,
    /// for every colour the renderer can produce.
    #[test]
    fn every_colour_the_renderer_emits_can_be_faded_out() {
        let mut cases = vec![Color::Rgb(232, 232, 226), Color::Rgb(255, 176, 64)];
        cases.extend((232..=255).map(Color::Indexed));
        cases.push(Color::Indexed(16));

        for c in cases {
            assert_eq!(
                toward_bg(c, 0.0, Theme::Night),
                Theme::Night.page(),
                "{c:?} survived a full fade"
            );
        }
    }

    /// And the same thing on a real buffer. Painted rather than rendered from
    /// the map, so the test still means something on a machine with no basemap
    /// — an earlier version of this asserted against an empty view and passed
    /// while the bug above was live.
    #[test]
    fn the_band_dissolves_whatever_was_under_it() {
        let ground = crate::canvas::ink(200, 200, 196);
        let cleared = Theme::Night.page();
        let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();

        term.draw(|f| {
            let area = f.area();
            for y in 0..area.height {
                for x in 0..area.width {
                    if let Some(c) = f.buffer_mut().cell_mut((x, y)) {
                        c.set_char('\u{2580}').set_fg(ground);
                    }
                }
            }
            fade_band(f, area, 4, 3, 1.0, Theme::Night);
        })
        .unwrap();

        let buf = term.backend().buffer();
        for y in 0..4 {
            for x in 0..40 {
                assert_eq!(buf.cell((x, y)).unwrap().fg, cleared, "row {y} survived");
            }
        }
        // Partway up the ramp: dimmed, but neither gone nor untouched.
        let mid = buf.cell((0, 5)).unwrap().fg;
        assert_ne!(mid, cleared, "ramp starts too dark");
        assert_ne!(mid, ground, "ramp is not ramping");
        // And past the ramp the map is itself again.
        assert_eq!(buf.cell((0, 8)).unwrap().fg, ground, "map never came back");
    }
}
