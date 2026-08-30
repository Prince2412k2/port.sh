use portfolio_v2_assets::home_portrait;
use portfolio_v2_protocol::{Bootstrap, Contact, Profile};
use portfolio_v2_scene::{
    compose, CellSurface, GlyphRun, HitRegion, LogicalViewport, PaletteRole, Primitive,
    RenderFrame, RenderVariant, VisualScene,
};
pub use portfolio_v2_scene::{ColorMode, RenderPackage, Theme};

mod map;

const MEASURE: u16 = 62;
const ART_GAP: u16 = 5;
const RAIL_GAP: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub cols: u16,
    pub rows: u16,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280.0,
            height: 780.0,
            scale: 1.0,
            cols: 160,
            rows: 45,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Home,
    Experience,
}

#[derive(Clone, Debug)]
pub struct ClientState {
    bootstrap: Option<Bootstrap>,
    pub viewport: Viewport,
    pub theme: Theme,
    pub section: Section,
    pub render_package: RenderPackage,
    pub color_mode: ColorMode,
    reduced_motion: bool,
    map: map::MapState,
}

impl Default for ClientState {
    fn default() -> Self {
        Self {
            bootstrap: None,
            viewport: Viewport::default(),
            theme: Theme::default(),
            section: Section::Home,
            render_package: RenderPackage::Canonical,
            color_mode: ColorMode::Color,
            reduced_motion: false,
            map: map::MapState::default(),
        }
    }
}

pub enum Action {
    BootstrapLoaded(Bootstrap),
    Resize(Viewport),
    SetReducedMotion(bool),
    ToggleTheme,
    CycleRenderPackage,
    ToggleRenderColor,
    Navigate(Section),
    Tick(f64),
    MapCommand(MapCommand),
    MapTile {
        z: u8,
        x: u32,
        y: u32,
        tile: termap::data::Tile,
    },
    MapOverlay(termap::data::Tile),
    MapTerrain(termap::terrain::Terrain),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapCommand {
    Next,
    Previous,
    Replay,
    Pan(f64, f64),
    Drag([f64; 2], [f64; 2]),
    Zoom(f64),
    ZoomAt(f64, [f64; 2]),
    Tilt(f64),
    Bearing(f64),
    ToggleCamera,
    ToggleTerrain,
    ToggleLabels,
    ToggleColor,
    CycleFocus,
    CycleRoads,
    RoadWeight(f64),
}

impl ClientState {
    pub fn update(&mut self, action: Action) {
        match action {
            Action::BootstrapLoaded(bootstrap) => self.bootstrap = Some(bootstrap),
            Action::Resize(viewport) => {
                self.viewport = viewport;
                self.map.resize(viewport);
            }
            Action::SetReducedMotion(reduced) => {
                self.reduced_motion = reduced;
                if reduced {
                    self.map.finish_animation();
                }
            }
            Action::ToggleTheme => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                };
            }
            Action::CycleRenderPackage => self.render_package = self.render_package.next(),
            Action::ToggleRenderColor => {
                self.color_mode = match self.color_mode {
                    ColorMode::Color => ColorMode::Monochrome,
                    ColorMode::Monochrome => ColorMode::Color,
                };
                self.map.set_mono(self.color_mode == ColorMode::Monochrome);
            }
            Action::Navigate(section) => {
                self.section = section;
                if section == Section::Experience {
                    self.map.open(self.viewport);
                    if self.reduced_motion {
                        self.map.finish_animation();
                    }
                }
            }
            Action::Tick(seconds) => {
                if self.section == Section::Experience {
                    if self.reduced_motion {
                        self.map.finish_animation();
                    } else {
                        self.map.tick(seconds);
                    }
                }
            }
            Action::MapCommand(command) => {
                if self.section == Section::Experience {
                    self.map.command(command);
                }
            }
            Action::MapTile { z, x, y, tile } => self.map.insert_tile(z, x, y, tile),
            Action::MapOverlay(tile) => self.map.overlay = Some(tile),
            Action::MapTerrain(terrain) => self.map.set_terrain(terrain),
        }
    }

    pub fn semantic_home(&self) -> Option<SemanticHome> {
        let bootstrap = self.bootstrap.as_ref()?;
        Some(SemanticHome {
            profile: bootstrap.profile.clone(),
            contacts: bootstrap.profile.contacts.clone(),
            revision: bootstrap.revision.clone(),
        })
    }

    pub fn scene(&self) -> VisualScene {
        let viewport = LogicalViewport {
            cols: self.viewport.cols,
            rows: self.viewport.rows,
        };
        let mut scene = VisualScene {
            viewport,
            theme: self.theme,
            primitives: Vec::new(),
            hits: Vec::new(),
            details: Vec::new(),
        };
        let Some(home) = self.semantic_home() else {
            put(&mut scene, 2, 2, "initialising", PaletteRole::Faint, false);
            return scene;
        };

        rail(&mut scene, self.section, &home.profile.name);
        match self.section {
            Section::Home => {
                footer_home(&mut scene);
                home_content(&mut scene, &home.profile);
            }
            Section::Experience => {
                map::render(&mut scene, &self.map);
                footer_experience(&mut scene, &self.map);
            }
        }
        scene
    }

    pub fn cells(&self) -> CellSurface {
        compose(&self.scene())
    }

    pub fn render_frame(&self) -> RenderFrame {
        let scene = self.scene();
        RenderFrame {
            fallback: compose(&scene),
            details: scene.details,
            variant: RenderVariant {
                package: self.render_package,
                color: self.color_mode,
                reduced_motion: self.reduced_motion,
            },
        }
    }

    pub fn map_demand(&self) -> Option<MapDemand> {
        (self.section == Section::Experience).then(|| self.map.demand(self.viewport))
    }

    pub fn map_prefetch_demand(&self) -> MapDemand {
        self.map.prefetch_demand(self.viewport)
    }

    pub fn map_needs_overlay(&self) -> bool {
        self.map.overlay.is_none()
    }

    pub fn map_needs_terrain(&self) -> bool {
        self.map.needs_terrain()
    }

    pub fn animating(&self) -> bool {
        self.section == Section::Experience && self.map.animating()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapDemand {
    pub tiles: Vec<(u8, u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticHome {
    pub profile: Profile,
    pub contacts: Vec<Contact>,
    pub revision: String,
}

fn rail(scene: &mut VisualScene, section: Section, name: &str) {
    let items = ["home", "experience", "projects", "skills", "taste", "ask"];
    let width = items
        .iter()
        .map(|item| 4 + item.chars().count() as u16)
        .sum::<u16>()
        + RAIL_GAP * (items.len() as u16 - 1);
    if width > scene.viewport.cols {
        return;
    }
    let gutter = if scene.viewport.cols >= 90 { 7 } else { 0 };
    let mut x = gutter + (scene.viewport.cols.saturating_sub(gutter + width)) / 2;
    for (index, item) in items.iter().enumerate() {
        let on = index
            == match section {
                Section::Home => 0,
                Section::Experience => 1,
            };
        let key = format!("[{}] ", index + 1);
        put(
            scene,
            x,
            0,
            &key,
            if on {
                PaletteRole::Amber
            } else {
                PaletteRole::Ghost
            },
            false,
        );
        x += key.chars().count() as u16;
        put(
            scene,
            x,
            0,
            item,
            if on {
                PaletteRole::Ink
            } else {
                PaletteRole::Faint
            },
            on,
        );
        let item_width = item.chars().count() as u16;
        scene.hits.push(HitRegion {
            id: (*item).into(),
            x: x.saturating_sub(key.chars().count() as u16),
            y: 0,
            width: key.chars().count() as u16 + item_width,
            height: 1,
        });
        x += item_width + RAIL_GAP;
    }
    if section != Section::Home {
        put(
            scene,
            if scene.viewport.cols >= 90 { 9 } else { 2 },
            0,
            &name.to_uppercase(),
            PaletteRole::Ink,
            true,
        );
    }
}

fn footer_home(scene: &mut VisualScene) {
    if scene.viewport.rows < 3 {
        return;
    }
    let y = scene.viewport.rows - 2;
    let gutter = if scene.viewport.cols >= 90 { 7 } else { 0 };
    put(scene, gutter + 2, y, "1-5", PaletteRole::Amber, true);
    put(
        scene,
        gutter + 7,
        y,
        "open a section",
        PaletteRole::Faint,
        false,
    );
    if scene.viewport.cols >= 70 {
        put(scene, 32, y, "/", PaletteRole::Amber, true);
        put(scene, 35, y, "all keys", PaletteRole::Faint, false);
    }
    if scene.viewport.cols >= 10 {
        put(
            scene,
            scene.viewport.cols - 9,
            y,
            "q",
            PaletteRole::Amber,
            true,
        );
        put(
            scene,
            scene.viewport.cols - 6,
            y,
            "quit",
            PaletteRole::Faint,
            false,
        );
    }
}

fn footer_experience(scene: &mut VisualScene, map: &map::MapState) {
    if scene.viewport.rows < 3 {
        return;
    }
    let y = scene.viewport.rows - 2;
    let x = if scene.viewport.cols >= 90 { 9 } else { 2 };
    if scene.viewport.cols < 70 {
        put(scene, x, y, "n b", PaletteRole::Amber, true);
        put(scene, x + 5, y, "places", PaletteRole::Faint, false);
        let home_x = scene.viewport.cols.saturating_sub(6);
        put(scene, home_x, y, "home", PaletteRole::Amber, true);
        scene.hits.push(HitRegion {
            id: "home".into(),
            x: home_x,
            y,
            width: 4,
            height: 1,
        });
        return;
    }
    for (key, label, offset) in [
        ("n b", "places", 0),
        ("?", "find", 15),
        ("drag", "pan", 26),
        ("wheel", "zoom", 39),
        ("esc", "home", 54),
        ("/", "all keys", 67),
    ] {
        put(scene, x + offset, y, key, PaletteRole::Amber, true);
        put(
            scene,
            x + offset + key.chars().count() as u16 + 2,
            y,
            label,
            PaletteRole::Faint,
            false,
        );
    }
    scene.hits.push(HitRegion {
        id: "home".into(),
        x: x + 54,
        y,
        width: 9,
        height: 1,
    });
    let status = format!(
        "{}   z{:.1}   tilt {:.0}°",
        map.mode_label(),
        map.zoom(),
        map.tilt_degrees()
    );
    let status_x = scene
        .viewport
        .cols
        .saturating_sub(status.chars().count() as u16 + 14);
    put(scene, status_x, y, &status, PaletteRole::Ghost, false);
    if scene.viewport.cols >= 10 {
        put(
            scene,
            scene.viewport.cols - 9,
            y,
            "q",
            PaletteRole::Amber,
            true,
        );
        put(
            scene,
            scene.viewport.cols - 6,
            y,
            "quit",
            PaletteRole::Faint,
            false,
        );
    }
}

fn home_content(scene: &mut VisualScene, profile: &Profile) {
    if scene.viewport.cols < 24 || scene.viewport.rows < 8 {
        return;
    }
    let body_x = if scene.viewport.cols >= 90 { 7 } else { 0 };
    let body_width = scene.viewport.cols.saturating_sub(body_x);
    let body_height = scene.viewport.rows.saturating_sub(3);
    let body_y = 1;
    let contact_width = contact_width(profile);

    let portrait = [72, 52, 40].into_iter().find_map(|cols| {
        let rows = match cols {
            72 => 27,
            52 => 19,
            _ => 15,
        };
        let room = body_width.saturating_sub(contact_width.max(MEASURE) + ART_GAP + 8);
        (cols <= room && rows <= body_height.saturating_sub(2)).then_some((cols, rows))
    });
    let art_width = portrait.map_or(0, |value| value.0);
    let gap = if art_width > 0 { ART_GAP } else { 0 };
    let room = body_width.saturating_sub(8 + art_width + gap);
    let measure = MEASURE.min(room);
    let stacked_contacts = contact_width > room;
    let block = art_width + gap + measure.max(contact_width.min(room));
    let art_x = body_x + body_width.saturating_sub(block) / 2;
    let text_x = art_x + art_width + gap;

    let pitch = wrap(&profile.pitch, measure as usize);
    let now = wrap(&profile.now, measure as usize);
    let text_rows = 3
        + pitch.len()
        + if now.is_empty() { 0 } else { 2 + now.len() }
        + 2
        + 2
        + 6
        + usize::from(stacked_contacts) * 2;
    let tall = text_rows.max(portrait.map_or(0, |value| value.1 as usize) + 1);
    let mut y = body_y + ((body_height as usize).saturating_sub(tall) / 2).max(1) as u16;

    if let Some((cols, rows)) = portrait {
        if let Some(art) = home_portrait(cols, rows, art_x, y) {
            scene.primitives.push(Primitive::CellArt(art));
        }
    }

    put(
        scene,
        text_x,
        y,
        &profile.name.to_uppercase(),
        PaletteRole::Ink,
        true,
    );
    y += 1;
    put(scene, text_x, y, &profile.role, PaletteRole::Amber, false);
    put(
        scene,
        text_x + profile.role.chars().count() as u16 + 3,
        y,
        &profile.location,
        PaletteRole::Faint,
        false,
    );
    y += 2;
    for line in pitch {
        put(scene, text_x, y, &line, PaletteRole::Ink, false);
        y += 1;
    }
    if !now.is_empty() {
        y += 1;
        put(scene, text_x, y, "NOW", PaletteRole::Ghost, false);
        y += 1;
        for line in now {
            put(scene, text_x, y, &line, PaletteRole::Faint, false);
            y += 1;
        }
    }
    y += 1;
    put(scene, text_x, y, "◆", PaletteRole::Certificate, false);
    put(
        scene,
        text_x + 3,
        y,
        "Claude Certified Architect",
        PaletteRole::Ink,
        false,
    );
    put(
        scene,
        text_x + 31,
        y,
        "· Foundations",
        PaletteRole::Ghost,
        false,
    );
    y += 2;

    for (key, label, blurb) in [
        ("1", "experience", "five places on a map you can drive"),
        ("2", "projects", "ten of them, and how they work"),
        ("3", "skills", "the tools"),
        ("4", "taste", "a room you can walk"),
        ("5", "ask", "put a question to the agent on this box"),
    ] {
        put(scene, text_x, y, key, PaletteRole::Amber, false);
        put(
            scene,
            text_x + 3,
            y,
            &format!("{label:<12}"),
            PaletteRole::Ink,
            false,
        );
        put(scene, text_x + 15, y, blurb, PaletteRole::Ghost, false);
        scene.hits.push(HitRegion {
            id: label.into(),
            x: text_x,
            y,
            width: measure,
            height: 1,
        });
        y += 1;
    }
    y += 1;
    let email = contact(profile, "email");
    let github = contact(profile, "github");
    let ssh = contact(profile, "ssh");
    let mosh = contact(profile, "mosh");
    if stacked_contacts {
        for (offset, value) in [github, email, ssh, mosh].into_iter().enumerate() {
            contact_row(scene, text_x, y + offset as u16, &[value]);
        }
    } else {
        contact_row(scene, text_x, y, &[github, email]);
        contact_row(scene, text_x, y + 1, &[ssh, mosh]);
    }
}

fn contact_row(scene: &mut VisualScene, mut x: u16, y: u16, values: &[&str]) {
    let mut first = true;
    for value in values.iter().filter(|value| !value.is_empty()) {
        if !first {
            put(scene, x, y, "   ·   ", PaletteRole::Ghost, false);
            x += 7;
        }
        put(scene, x, y, value, PaletteRole::Cyan, false);
        x += value.chars().count() as u16;
        first = false;
    }
}

fn contact<'a>(profile: &'a Profile, id: &str) -> &'a str {
    profile
        .contacts
        .iter()
        .find(|contact| contact.id == id)
        .map_or("", |contact| contact.value.as_str())
}

fn contact_width(profile: &Profile) -> u16 {
    let row = |a: &str, b: &str| -> u16 {
        let values = [contact(profile, a), contact(profile, b)];
        values
            .iter()
            .map(|value| value.chars().count() as u16)
            .sum::<u16>()
            + if values.iter().all(|value| !value.is_empty()) {
                7
            } else {
                0
            }
    };
    row("github", "email").max(row("ssh", "mosh"))
}

fn put(scene: &mut VisualScene, x: u16, y: u16, text: &str, foreground: PaletteRole, bold: bool) {
    if y >= scene.viewport.rows || x >= scene.viewport.cols || text.is_empty() {
        return;
    }
    let text = text
        .chars()
        .take(scene.viewport.cols.saturating_sub(x) as usize)
        .collect();
    scene.primitives.push(Primitive::GlyphRun(GlyphRun {
        x,
        y,
        text,
        foreground,
        bold,
        detail: 0,
    }));
}

fn wrap(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in value.split_whitespace() {
        let next = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
        if next > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_power_toggles_dark_and_light() {
        let mut state = ClientState::default();
        assert_eq!(state.theme, Theme::Dark);
        state.update(Action::ToggleTheme);
        assert_eq!(state.theme, Theme::Light);
        state.update(Action::ToggleTheme);
        assert_eq!(state.theme, Theme::Dark);
    }

    #[test]
    fn reduced_motion_finishes_the_opening_flight_immediately() {
        let mut state = ClientState::default();
        state.update(Action::SetReducedMotion(true));
        state.update(Action::Navigate(Section::Experience));
        assert!(!state.animating());
        assert!((state.map.zoom() - 11.6).abs() < 1e-9);
        assert_eq!(state.map.tilt_degrees(), 46.0);
    }

    #[test]
    fn render_package_and_color_variants_are_global_state() {
        let mut state = ClientState::default();
        state.update(Action::CycleRenderPackage);
        state.update(Action::ToggleRenderColor);
        let frame = state.render_frame();
        assert_eq!(frame.variant.package, RenderPackage::Crt);
        assert_eq!(frame.variant.color, ColorMode::Monochrome);
        assert_eq!(frame.fallback.cols, state.viewport.cols);
        assert_eq!(frame.fallback.rows, state.viewport.rows);
    }
}
