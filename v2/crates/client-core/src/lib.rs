use portfolio_v2_assets::home_portrait;
use portfolio_v2_protocol::{Bootstrap, Contact, Profile};
use portfolio_v2_scene::{
    compose, CellSurface, GlyphRun, HitRegion, LogicalViewport, PaletteRole, Primitive,
    RenderFrame, RenderVariant, VisualScene,
};
pub use portfolio_v2_scene::{ColorMode, RenderPackage, Theme};

mod map;
pub mod map_store;
#[path = "../../../../portfolio/src/museum.rs"]
#[allow(dead_code)]
mod museum;
#[path = "../../../../portfolio/src/paint.rs"]
#[allow(dead_code)]
mod paint;
mod sections;
#[path = "../../../../portfolio/src/walls.rs"]
#[allow(dead_code)]
mod walls;
mod portraits {
    pub use portfolio_v2_assets::baked::*;
}
mod taste {
    pub use portfolio_v2_protocol::taste::*;
}

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
    Projects,
    Skills,
    Taste,
    Ask,
}
impl Section {
    pub fn id(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Experience => "experience",
            Self::Projects => "projects",
            Self::Skills => "skills",
            Self::Taste => "taste",
            Self::Ask => "ask",
        }
    }
    pub fn parse(id: &str) -> Option<Self> {
        Some(match id {
            "home" => Self::Home,
            "experience" => Self::Experience,
            "projects" => Self::Projects,
            "skills" => Self::Skills,
            "taste" => Self::Taste,
            "ask" => Self::Ask,
            _ => return None,
        })
    }
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
    pages: sections::Pages,
    help: bool,
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
            pages: sections::Pages::default(),
            help: false,
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
    MapBuildings(termap::data::Tile),
    MapGeneration(Vec<(u8, u32, u32, std::sync::Arc<termap::data::Tile>)>),
    MapCompleteGeneration {
        tiles: map_store::Generation,
        terrain: termap::terrain::Terrain,
    },
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
            Action::BootstrapLoaded(bootstrap) => {
                self.pages.load(&bootstrap);
                self.map.set_places(&bootstrap.places);
                self.bootstrap = Some(bootstrap);
            }
            Action::Resize(viewport) => {
                self.viewport = viewport;
                self.map.resize(viewport);
            }
            Action::SetReducedMotion(reduced) => {
                self.reduced_motion = reduced;
                if reduced {
                    self.pages.velocity = (0.0, 0.0);
                    self.pages.drifting = false;
                }
                if reduced {
                    self.map.finish_animation();
                }
            }
            Action::ToggleTheme => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                };
                self.render_package = match self.theme {
                    Theme::Dark => RenderPackage::Crt,
                    Theme::Light => RenderPackage::Ink,
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
                if section != self.section {
                    self.pages.scroll = 0;
                    self.pages.dragging = false;
                    self.pages.velocity = (0.0, 0.0);
                }
                self.section = section;
                if section == Section::Taste {
                    if let Some(museum) = &mut self.pages.museum {
                        museum.jump(museum.sel);
                    }
                }
                if section == Section::Experience {
                    self.map.open(self.viewport);
                    if self.reduced_motion {
                        self.map.finish_animation();
                    }
                }
            }
            Action::Tick(seconds) => {
                if !self.reduced_motion {
                    self.pages.tick(seconds, self.section);
                }
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
            Action::MapBuildings(tile) => self.map.set_buildings(tile),
            Action::MapGeneration(tiles) => self.map.replace_tiles(tiles),
            Action::MapCompleteGeneration { tiles, terrain } => {
                self.map.set_terrain(terrain);
                self.map.replace_tiles(tiles);
            }
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

    /// Platform adapters normalize keys; navigation and camera behavior are shared.
    pub fn key(&mut self, key: &str) -> bool {
        if self.help {
            if matches!(key, "Escape" | "/") {
                self.help = false;
            }
            return true;
        }
        if key == "Escape" && self.section == Section::Ask && self.pages.diagram.is_some() {
            self.pages.diagram = None;
            return true;
        }
        if self.section == Section::Experience && self.map.search_key(key) {
            return true;
        }
        if self.section == Section::Ask && key != "Escape" {
            return self.pages.ask_key(key);
        }
        if key == "/" {
            self.help = true;
            return true;
        }
        if self.pages.key(self.section, key, self.bootstrap.as_ref()) {
            return true;
        }
        let action = match key {
            "0" | "1" | "Escape" => Action::Navigate(Section::Home),
            "2" => Action::Navigate(Section::Experience),
            "3" => Action::Navigate(Section::Projects),
            "4" => Action::Navigate(Section::Skills),
            "5" => Action::Navigate(Section::Taste),
            "6" => Action::Navigate(Section::Ask),
            "Tab" if self.section == Section::Projects => Action::Navigate(Section::Skills),
            "Tab" if self.section == Section::Skills => Action::Navigate(Section::Projects),
            "Enter" if self.section == Section::Home => Action::Navigate(Section::Experience),
            "i" => Action::ToggleTheme,
            "p" => Action::CycleRenderPackage,
            "c" => Action::ToggleRenderColor,
            _ if self.section == Section::Experience => {
                let command = match key {
                    "n" => MapCommand::Next,
                    "b" => MapCommand::Previous,
                    "Enter" => MapCommand::Replay,
                    "h" | "ArrowLeft" => MapCommand::Pan(-0.18, 0.0),
                    "l" | "ArrowRight" => MapCommand::Pan(0.18, 0.0),
                    "k" | "ArrowUp" => MapCommand::Pan(0.0, -0.18),
                    "j" | "ArrowDown" => MapCommand::Pan(0.0, 0.18),
                    "+" | "=" => MapCommand::Zoom(0.35),
                    "-" | "_" => MapCommand::Zoom(-0.35),
                    "u" => MapCommand::Tilt(0.08),
                    "o" => MapCommand::Tilt(-0.08),
                    "," => MapCommand::Bearing(-0.10),
                    "." => MapCommand::Bearing(0.10),
                    "m" => MapCommand::ToggleCamera,
                    "v" => MapCommand::ToggleTerrain,
                    "t" => MapCommand::ToggleLabels,
                    "f" => MapCommand::CycleFocus,
                    "r" => MapCommand::CycleRoads,
                    "[" => MapCommand::RoadWeight(-0.15),
                    "]" => MapCommand::RoadWeight(0.15),
                    _ => return false,
                };
                Action::MapCommand(command)
            }
            _ => return false,
        };
        self.update(action);
        true
    }

    pub fn map_point(&self, col: f64, row: f64) -> Option<[f64; 2]> {
        let gutter = if self.viewport.cols >= 90 { 7.0 } else { 0.0 };
        (self.section == Section::Experience
            && col >= gutter
            && col < self.viewport.cols as f64
            && row >= 1.0
            && row < self.viewport.rows.saturating_sub(2) as f64)
            .then_some([(col - gutter) * 2.0 + 1.0, (row - 1.0) * 4.0 + 2.0])
    }

    pub fn pixel_mesh(&self) -> Vec<f32> {
        if self.section == Section::Experience {
            self.map.pixel_mesh(self.viewport)
        } else {
            Vec::new()
        }
    }
    pub fn pixel_masks(&self) -> Vec<[u16; 4]> {
        self.map.pixel_masks(self.viewport)
    }
    pub fn pixel_heightfield(&self) -> Vec<f32> {
        if self.section == Section::Experience {
            self.map.pixel_heightfield()
        } else {
            Vec::new()
        }
    }
    pub fn map_description(&self) -> String {
        self.map.description()
    }

    pub fn scene(&self) -> VisualScene {
        self.build_scene(false)
    }
    pub fn browser_scene(&self) -> VisualScene {
        self.build_scene(self.render_package == RenderPackage::Pixel)
    }
    fn build_scene(&self, scene_native_map: bool) -> VisualScene {
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
                if scene_native_map {
                    map::render_annotations(&mut scene, &self.map);
                } else {
                    map::render(&mut scene, &self.map);
                }
                footer_experience(&mut scene, &self.map);
            }
            _ => sections::render(
                &mut scene,
                self.section,
                &self.pages,
                self.bootstrap.as_ref().unwrap(),
            ),
        }
        if self.help {
            scene.primitives.clear();
            scene.hits.clear();
            put(&mut scene, 3, 2, "KEYS", PaletteRole::Amber, true);
            for (row, text) in [
                "1 home · 2 experience · 3 projects · 4 skills · 5 taste · 6 ask",
                "Map: arrows/hjkl pan · +/- zoom · n/b tour · ? search",
                "Map: u/o tilt · ,/. bearing · m camera · v terrain · t labels",
                "Projects: left/right select · j/k scroll · click a pip",
                "Skills: arrows pan · pointer lifts marks · drag the sheet",
                "Taste: left/right exhibits · j/k read essay · click a pip",
                "Ask: type locally · Enter send · Ctrl+X cancel · Escape home",
                "Appearance: i theme · p render package · c monochrome",
                "Escape or / closes this help · q quits the terminal",
            ]
            .iter()
            .enumerate()
            {
                if row + 4 >= scene.viewport.rows as usize {
                    break;
                }
                put(&mut scene, 3, row as u16 + 4, text, PaletteRole::Ink, false);
            }
        }
        scene
    }
    pub fn help_open(&self) -> bool {
        self.help
    }
    pub fn activate(&mut self, id: &str) -> bool {
        if let Some(rest) = id.strip_prefix("panel:") {
            if let Some((request, index)) = rest.split_once(':') {
                if let Ok(index) = index.parse::<usize>() {
                    if let Some(panel) = self
                        .pages
                        .session
                        .as_ref()
                        .and_then(|s| s.exchanges.iter().find(|e| e.request_id == request))
                        .and_then(|e| e.presentations.get(index))
                        .cloned()
                    {
                        return self.show_presentation(&panel);
                    }
                }
            }
        }
        if let Some(section) = Section::parse(id) {
            self.update(Action::Navigate(section));
            return true;
        }
        if let Some(index) = id
            .strip_prefix("project:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if self
                .bootstrap
                .as_ref()
                .is_some_and(|b| index < b.projects.len())
            {
                self.pages.at = index;
                self.pages.scroll = 0;
                self.section = Section::Projects;
                return true;
            }
        }
        if let Some(index) = id
            .strip_prefix("taste:")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if let Some(museum) = &mut self.pages.museum {
                if index < museum.len() {
                    museum.go(index);
                    self.pages.scroll = 0;
                    return true;
                }
            }
        }
        false
    }
    fn show_presentation(&mut self, panel: &portfolio_v2_protocol::session::Presentation) -> bool {
        match panel {
            portfolio_v2_protocol::session::Presentation::Map {
                title,
                lon,
                lat,
                zoom,
            } => {
                self.map.present(title, *lon, *lat, *zoom);
                self.map.resize(self.viewport);
                self.section = Section::Experience;
            }
            portfolio_v2_protocol::session::Presentation::Project { id } => {
                let Some(at) = self
                    .bootstrap
                    .as_ref()
                    .and_then(|b| b.projects.iter().position(|p| p.id == *id))
                else {
                    return false;
                };
                self.pages.at = at;
                self.pages.scroll = 0;
                self.section = Section::Projects;
            }
            portfolio_v2_protocol::session::Presentation::Diagram { .. } => {
                self.pages.diagram = Some(panel.clone());
                self.pages.scroll = 0;
                self.section = Section::Ask;
            }
        }
        true
    }
    pub fn click(&mut self, x: u16, y: u16) -> bool {
        let id = self
            .scene()
            .hits
            .iter()
            .rev()
            .find(|hit| {
                x >= hit.x
                    && x < hit.x.saturating_add(hit.width)
                    && y >= hit.y
                    && y < hit.y.saturating_add(hit.height)
            })
            .map(|hit| hit.id.clone());
        id.is_some_and(|id| self.activate(&id))
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
        if self.help {
            return false;
        }
        (self.section == Section::Experience && self.map.animating())
            || (!self.reduced_motion && self.section == Section::Projects)
            || (!self.reduced_motion
                && self.section == Section::Skills
                && !self.pages.dragging
                && (self.pages.drifting || self.pages.velocity != (0.0, 0.0)))
            || (!self.reduced_motion
                && self.section == Section::Taste
                && self.pages.scroll == 0
                && self.pages.museum.as_ref().is_some_and(|m| {
                    m.moving(ratatui::layout::Rect::new(
                        0,
                        0,
                        self.viewport
                            .cols
                            .saturating_sub(if self.viewport.cols >= 90 { 7 } else { 0 }),
                        self.viewport.rows.saturating_sub(3),
                    ))
                }))
    }
    pub fn restore_appearance(&mut self, theme: &str, package: &str, color: &str) {
        self.theme = match theme {
            "light" | "Light" => Theme::Light,
            _ => Theme::Dark,
        };
        self.render_package = match package {
            "crt" => RenderPackage::Crt,
            "vhs" => RenderPackage::Vhs,
            "ink" => RenderPackage::Ink,
            "pixel" => RenderPackage::Pixel,
            _ => RenderPackage::Canonical,
        };
        self.color_mode = if color == "monochrome" {
            ColorMode::Monochrome
        } else {
            ColorMode::Color
        };
        self.map.set_mono(self.color_mode == ColorMode::Monochrome);
    }
    pub fn content_semantics(&self) -> serde_json::Value {
        self.pages.semantics(self.section, self.bootstrap.as_ref())
    }
    pub fn local_snapshot(&self) -> serde_json::Value {
        serde_json::json!({"section":self.section.id(),"camera":self.map.camera_snapshot(),"mapNavigation":self.map.navigation_snapshot(),"project":self.pages.at,"taste":self.pages.museum.as_ref().map_or(0,|m|m.sel),"scroll":self.pages.scroll,"drift":self.pages.drift,"clock":self.pages.clock,"question":self.pages.question})
    }
    pub fn restore_local(&mut self, value: &serde_json::Value) {
        if let Some(section) = value["section"].as_str().and_then(Section::parse) {
            self.update(Action::Navigate(section));
        }
        if let Some(index) = value["project"].as_u64() {
            if self
                .bootstrap
                .as_ref()
                .is_some_and(|b| index < b.projects.len() as u64)
            {
                self.pages.at = index as usize;
            }
        }
        if let Some(index) = value["taste"].as_u64() {
            if let Some(m) = &mut self.pages.museum {
                if index < (m.len() as u64) {
                    m.jump(index as usize);
                }
            }
        }
        self.pages.scroll = value["scroll"].as_u64().unwrap_or(0).min(u16::MAX as u64) as u16;
        if let Some(clock) = value["clock"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v < 1e9)
        {
            self.pages.clock = clock;
        }
        if let Ok(drift) = serde_json::from_value::<(f64, f64)>(value["drift"].clone()) {
            if drift.0.is_finite() && drift.1.is_finite() {
                self.pages.drift = drift;
            }
        }
        if let Ok(camera) = serde_json::from_value::<[f64; 6]>(value["camera"].clone()) {
            self.map.restore_camera(camera);
        }
        if let Ok((index, title)) =
            serde_json::from_value::<(usize, Option<String>)>(value["mapNavigation"].clone())
        {
            self.map.restore_navigation(index, title);
        }
        if let Some(question) = value["question"].as_str() {
            self.edit_question(question);
        }
    }
    pub fn scroll(&mut self, delta: i32) {
        self.pages.scroll = if delta < 0 {
            self.pages
                .scroll
                .saturating_sub(delta.unsigned_abs() as u16)
        } else {
            self.pages.scroll.saturating_add(delta as u16)
        };
    }
    pub fn pointer(&mut self, x: f64, y: f64, dx: f64, dy: f64, drag: bool) -> bool {
        if self.section == Section::Experience && !drag {
            return self
                .map_point(x, y)
                .is_some_and(|point| self.map.hover(point));
        }
        if self.section == Section::Skills {
            let gutter = if self.viewport.cols >= 90 { 7.0 } else { 0.0 };
            if x < gutter || y < 1.0 {
                return false;
            }
            self.pages.cursor = Some((x - gutter, y - 1.0));
            if drag {
                self.pages.dragging = true;
                self.pages.drift.0 -= dx;
                self.pages.drift.1 -= dy;
                self.pages.velocity.0 = self.pages.velocity.0 * 0.4 - dx * 18.0;
                self.pages.velocity.1 = self.pages.velocity.1 * 0.4 - dy * 18.0;
            }
            return true;
        }
        false
    }
    pub fn release_pointer(&mut self) {
        self.pages.dragging = false;
    }
    pub fn map_search_query(&self) -> Option<String> {
        self.map.search_query()
    }
    pub fn local_map_search(&self, query: &str) -> Vec<portfolio_v2_protocol::map::SearchResult> {
        self.map.local_search(query)
    }
    pub fn map_search_results(
        &mut self,
        query: &str,
        results: Vec<portfolio_v2_protocol::map::SearchResult>,
    ) {
        self.map.search_results(query, results);
    }
    pub fn ask_question(&self) -> String {
        self.pages.question.clone()
    }
    pub fn reset_session(&mut self) {
        self.pages.session = None;
        self.pages.pending_submission = None;
        self.pages.diagram = None;
        self.pages.question.clear();
        self.pages.status.clear();
    }
    pub fn session_snapshot(&self) -> Option<portfolio_v2_protocol::session::Snapshot> {
        self.pages.session.clone()
    }
    pub fn edit_question(&mut self, text: &str) {
        if text.len() <= 4096
            && !text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            self.pages.question = text.into();
            self.pages.edit_revision += 1;
        }
    }
    pub fn mark_submitted(&mut self, id: &str) {
        if self
            .pages
            .pending_submission
            .as_ref()
            .is_none_or(|(previous, _, _)| previous != id)
        {
            self.pages.pending_submission = Some((
                id.into(),
                self.pages.question.clone(),
                self.pages.edit_revision,
            ));
        }
    }
    pub fn ask_status(&mut self, message: &str) {
        self.pages.status = message.chars().take(160).collect();
    }
    pub fn ask_snapshot(&mut self, snapshot: portfolio_v2_protocol::session::Snapshot) -> bool {
        if snapshot.validate().is_err() {
            return false;
        }
        if let Some(previous) = &self.pages.session {
            if previous.session_id == snapshot.session_id && previous.sequence >= snapshot.sequence
            {
                return false;
            }
        }
        let running = snapshot
            .exchanges
            .iter()
            .any(|e| e.status == portfolio_v2_protocol::session::RequestStatus::Running);
        if self.section == Section::Ask && self.pages.session.is_some() {
            let previous = self
                .pages
                .session
                .as_ref()
                .and_then(|s| s.exchanges.last())
                .filter(|e| {
                    snapshot
                        .exchanges
                        .last()
                        .is_some_and(|next| next.request_id == e.request_id)
                })
                .map_or(0, |e| e.presentations.len());
            if let Some(exchange) = snapshot.exchanges.last() {
                if exchange.presentations.len() > previous {
                    if let Some(panel) = exchange.presentations.last() {
                        match panel {
                            portfolio_v2_protocol::session::Presentation::Map {
                                title,
                                lon,
                                lat,
                                zoom,
                            } => {
                                self.map.present(title, *lon, *lat, *zoom);
                                self.map.resize(self.viewport);
                                self.section = Section::Experience;
                            }
                            portfolio_v2_protocol::session::Presentation::Project { id } => {
                                if let Some(at) = self
                                    .bootstrap
                                    .as_ref()
                                    .and_then(|b| b.projects.iter().position(|p| p.id == *id))
                                {
                                    self.pages.at = at;
                                    self.pages.scroll = 0;
                                    self.section = Section::Projects;
                                }
                            }
                            portfolio_v2_protocol::session::Presentation::Diagram { .. } => {
                                self.pages.diagram = Some(panel.clone());
                                self.pages.scroll = 0;
                            }
                        }
                    }
                }
            }
        }
        if let Some((id, question, revision)) = &self.pages.pending_submission {
            if snapshot.exchanges.iter().any(|e| e.request_id == *id) {
                if *revision == self.pages.edit_revision && *question == self.pages.question {
                    self.pages.question.clear();
                }
                self.pages.pending_submission = None;
            }
        }
        self.pages.status = if running {
            "answering…".into()
        } else {
            String::new()
        };
        self.pages.session = Some(snapshot);
        true
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
                Section::Projects => 2,
                Section::Skills => 3,
                Section::Taste => 4,
                Section::Ask => 5,
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
    put(scene, gutter + 2, y, "2-6", PaletteRole::Amber, true);
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
        ("2", "experience", "places on a map you can drive"),
        ("3", "projects", "what they are, and how they work"),
        ("4", "skills", "the tools"),
        ("5", "taste", "a room you can walk"),
        ("6", "ask", "put a question to the resident assistant"),
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
        .map(|glyph| if glyph.is_control() { ' ' } else { glyph })
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
    fn acknowledgement_preserves_a_new_draft_even_when_the_text_is_identical() {
        let mut state = ClientState::default();
        state.edit_question("hello");
        let id = "1".repeat(32);
        state.mark_submitted(&id);
        state.edit_question("hello");
        let snapshot = portfolio_v2_protocol::session::Snapshot {
            protocol: 2,
            session_id: "2".repeat(32),
            sequence: 1,
            exchanges: vec![portfolio_v2_protocol::session::Exchange {
                created_at: 0,
                request_id: id,
                question: "hello".into(),
                answer: String::new(),
                status: portfolio_v2_protocol::session::RequestStatus::Running,
                error: None,
                presentations: Vec::new(),
            }],
        };
        assert!(state.ask_snapshot(snapshot.clone()));
        assert_eq!(state.ask_question(), "hello");
        assert!(!state.ask_snapshot(snapshot));
        assert_eq!(state.ask_question(), "hello");
    }
    #[test]
    fn a_released_skills_throw_settles_and_stops_scheduling() {
        let mut state = ClientState::default();
        state.update(Action::Navigate(Section::Skills));
        state.pointer(30.0, 10.0, 4.0, 2.0, true);
        state.release_pointer();
        assert!(state.animating());
        for _ in 0..200 {
            state.update(Action::Tick(0.1));
        }
        assert!(!state.animating());
        assert!(state.pages.drift.0.abs() > 4.0);
    }

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
