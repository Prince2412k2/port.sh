//! Worker-only adapter. No DOM, timers, sockets, or GPU resources in the engine.
use portfolio_v2_client_core::{map_store::MapStore, Action, ClientState, MapCommand, Viewport};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Engine {
    state: ClientState,
    cache: MapStore,
    metadata: String,
    active: Vec<(u8, u32, u32)>,
    tiled_terrain: bool,
}

#[wasm_bindgen]
impl Engine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        console_error_panic_hook::set_once();
        Self {
            state: ClientState::default(),
            cache: MapStore::default(),
            metadata: String::new(),
            active: Vec::new(),
            tiled_terrain: false,
        }
    }

    pub fn bootstrap(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let bootstrap = portfolio_v2_protocol::decode_bootstrap(bytes)
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        self.state.update(Action::BootstrapLoaded(bootstrap));
        Ok(())
    }

    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        self.state.update(Action::Resize(Viewport {
            width,
            height,
            scale,
            cols: ((width / 8.0).floor() as u16).clamp(20, 180),
            rows: ((height / 17.0).floor() as u16).clamp(6, 60),
        }));
    }

    pub fn key(&mut self, key: &str) -> bool {
        self.state.key(key)
    }
    pub fn section(&self) -> String {
        self.state.section.id().into()
    }
    pub fn restore_appearance(&mut self, theme: &str, package: &str, color: &str) {
        self.state.restore_appearance(theme, package, color);
    }
    pub fn appearance(&mut self, id: &str) {
        match id {
            "theme" => self.state.update(Action::ToggleTheme),
            "package" => self.state.update(Action::CycleRenderPackage),
            "color" => self.state.update(Action::ToggleRenderColor),
            _ => (),
        }
    }
    pub fn reduced_motion(&mut self, reduced: bool) {
        self.state.update(Action::SetReducedMotion(reduced));
    }
    pub fn tick(&mut self, seconds: f64) {
        self.state.update(Action::Tick(seconds.clamp(0.0, 10.0)));
    }
    pub fn animating(&self) -> bool {
        self.state.animating()
    }
    pub fn drag(&mut self, x: f64, y: f64, dx: f64, dy: f64) {
        if let (Some(from), Some(to)) = (
            self.state.map_point(x, y),
            self.state.map_point(x + dx, y + dy),
        ) {
            self.state
                .update(Action::MapCommand(MapCommand::Drag(from, to)));
        }
    }
    pub fn zoom(&mut self, x: f64, y: f64, delta: f64) {
        if let Some(point) = self.state.map_point(x, y) {
            self.state.update(Action::MapCommand(MapCommand::ZoomAt(
                delta.clamp(-1.0, 1.0),
                point,
            )));
        }
    }
    pub fn navigate(&mut self, id: &str) {
        self.state.activate(id);
    }
    pub fn pointer(&mut self, x: f64, y: f64, dx: f64, dy: f64, drag: bool) -> bool {
        self.state.pointer(x, y, dx, dy, drag)
    }
    pub fn scroll(&mut self, delta: i32) {
        self.state.scroll(delta);
    }
    pub fn release_pointer(&mut self) {
        self.state.release_pointer();
    }
    pub fn question(&self) -> String {
        self.state.ask_question()
    }
    pub fn mark_submitted(&mut self, id: &str) {
        self.state.mark_submitted(id);
    }
    pub fn reset_session(&mut self) {
        self.state.reset_session();
    }
    pub fn search_query(&self) -> Option<String> {
        self.state.map_search_query()
    }
    pub fn local_search(&self, query: &str) -> String {
        serde_json::to_string(&self.state.local_map_search(query)).unwrap()
    }
    pub fn search_results(&mut self, query: &str, text: &str) -> Result<(), JsValue> {
        let results =
            serde_json::from_str(text).map_err(|_| JsValue::from_str("invalid search result"))?;
        self.state.map_search_results(query, results);
        Ok(())
    }
    pub fn session_snapshot(&self) -> String {
        serde_json::to_string(&self.state.session_snapshot()).unwrap()
    }
    pub fn edit_question(&mut self, text: &str) {
        self.state.edit_question(text);
    }
    pub fn ask_status(&mut self, text: &str) {
        self.state.ask_status(text);
    }
    pub fn session(&mut self, text: &str) -> Result<bool, JsValue> {
        let snapshot: portfolio_v2_protocol::session::Snapshot =
            serde_json::from_str(text).map_err(|_| JsValue::from_str("invalid session"))?;
        snapshot.validate().map_err(JsValue::from_str)?;
        Ok(self.state.ask_snapshot(snapshot))
    }
    pub fn session_cbor(&mut self, bytes: &[u8]) -> Result<bool, JsValue> {
        if bytes.len() > 3 * 1024 * 1024 {
            return Err("session exceeds byte limit".into());
        }
        let snapshot: portfolio_v2_protocol::session::Snapshot =
            ciborium::from_reader(bytes).map_err(|_| JsValue::from_str("invalid CBOR session"))?;
        snapshot.validate().map_err(JsValue::from_str)?;
        Ok(self.state.ask_snapshot(snapshot))
    }
    pub fn demand(&self) -> String {
        serde_json::to_string(&self.state.map_demand().map(|d| d.tiles).unwrap_or_default())
            .unwrap()
    }
    pub fn prefetch(&self) -> String {
        serde_json::to_string(&self.state.map_prefetch_demand().tiles).unwrap()
    }
    pub fn has_tile(&self, z: u8, x: u32, y: u32) -> bool {
        self.cache.contains(&(z, x, y))
    }
    pub fn tiled_terrain(&mut self, enabled: bool) {
        self.tiled_terrain = enabled;
    }
    pub fn has_terrain_tile(&self, z: u8, x: u32, y: u32) -> bool {
        self.cache.has_terrain(&(z, x, y))
    }
    pub fn terrain_tile(&mut self, z: u8, x: u32, y: u32, bytes: Vec<u8>) -> bool {
        self.cache.insert_terrain((z, x, y), bytes)
    }
    pub fn tile(&mut self, z: u8, x: u32, y: u32, bytes: &[u8]) -> bool {
        self.cache.insert((z, x, y), bytes)
    }
    pub fn overlay(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.len() > 2 * 1024 * 1024 {
            return Err("overlay too large".into());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| JsValue::from_str("invalid overlay"))?;
        self.state
            .update(Action::MapOverlay(termap::data::Tile::new(
                termap::data::parse_features_checked(text).map_err(JsValue::from_str)?,
            )));
        Ok(())
    }
    pub fn terrain(&mut self, bytes: Vec<u8>) -> Result<(), JsValue> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("terrain too large".into());
        }
        let terrain = termap::terrain::Terrain::from_bytes(bytes)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        self.state.update(Action::MapTerrain(terrain));
        Ok(())
    }
    pub fn buildings(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("building data too large".into());
        }
        let text =
            std::str::from_utf8(bytes).map_err(|_| JsValue::from_str("invalid building data"))?;
        self.state
            .update(Action::MapBuildings(termap::data::Tile::new(
                termap::data::parse_features_checked(text).map_err(JsValue::from_str)?,
            )));
        Ok(())
    }

    /// Fixed-width transferable cells avoid serializing thousands of JSON objects.
    pub fn frame(&mut self) -> Vec<u8> {
        let wanted = self.state.map_demand().map(|d| d.tiles).unwrap_or_default();
        self.cache.protect(&wanted);
        if self.state.map_demand().is_some() && wanted != self.active {
            if let Some(tiles) = self.cache.generation(&wanted) {
                if self.tiled_terrain {
                    if let Some(terrain) = self.cache.terrain_generation(&wanted) {
                        self.state
                            .update(Action::MapCompleteGeneration { tiles, terrain });
                        self.active = wanted.clone();
                    }
                } else {
                    self.state.update(Action::MapGeneration(tiles));
                    self.active = wanted.clone();
                }
            }
        }
        let scene = self.state.browser_scene();
        let surface = portfolio_v2_scene::compose(&scene);
        self.metadata = serde_json::json!({
            "cols": surface.cols, "rows": surface.rows, "theme": self.state.theme,
            "variant": { "package": self.state.render_package, "color": self.state.color_mode },
            "details": scene.details, "hits": scene.hits,
            "profile": self.state.semantic_home().map(|home| home.profile),
            "section": self.state.section.id(),
            "content": self.state.content_semantics(),
            "loading": wanted != self.active, "decodedBytes": self.cache.bytes(),
            "pixelMasks": self.state.pixel_masks(),
            "mapDescription": self.state.map_description(),
            "search":self.state.map_search_query(),
            "help":self.state.help_open(),
            "local":self.state.local_snapshot(),
        })
        .to_string();
        surface.packed()
    }
    pub fn metadata(&self) -> String {
        self.metadata.clone()
    }
    pub fn restore_local(&mut self, text: &str) -> Result<(), JsValue> {
        if text.len() > 16384 {
            return Err("local state exceeds bounds".into());
        }
        let value =
            serde_json::from_str(text).map_err(|_| JsValue::from_str("invalid local state"))?;
        self.state.restore_local(&value);
        Ok(())
    }
    pub fn canonical_fallback(&mut self) {
        self.state.render_package = portfolio_v2_scene::RenderPackage::Canonical;
    }
    pub fn pixel_heightfield(&self) -> Vec<f32> {
        if self.state.render_package == portfolio_v2_scene::RenderPackage::Pixel {
            self.state.pixel_heightfield()
        } else {
            Vec::new()
        }
    }
    pub fn pixel_mesh(&self) -> Vec<f32> {
        if self.state.render_package == portfolio_v2_scene::RenderPackage::Pixel {
            self.state.pixel_mesh()
        } else {
            Vec::new()
        }
    }
}
