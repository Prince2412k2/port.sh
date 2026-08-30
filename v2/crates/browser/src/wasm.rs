use std::{cell::RefCell, collections::BTreeSet};

use js_sys::Uint8Array;
use portfolio_v2_client_core::{Action, ClientState, MapCommand, Section, Theme, Viewport};
use portfolio_v2_protocol::Bootstrap;
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Event, KeyboardEvent, MouseEvent, PointerEvent, Response, WheelEvent, Window,
};

const CELL_WIDTH: f64 = 8.0;
const CELL_HEIGHT: f64 = 17.0;
const MAX_ANIMATION_STEP: f64 = 0.025;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

struct App {
    state: ClientState,
    document: Document,
    requested_tiles: BTreeSet<(u8, u32, u32)>,
    wanted_tiles: BTreeSet<(u8, u32, u32)>,
    map_generation: u32,
    overlay_loading: bool,
    terrain_loading: bool,
    last_frame: Option<f64>,
    frame_pending: bool,
    drag_from: Option<[f64; 2]>,
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = portfolioV2, js_name = render)]
    fn render_package(scene: &str);

    #[wasm_bindgen(js_namespace = portfolioV2Controls, js_name = paint)]
    fn paint_controls(theme: &str, package: u8);

}

#[wasm_bindgen(module = "/src/pmtiles.js")]
extern "C" {
    #[wasm_bindgen(js_name = beginMapGeneration)]
    fn begin_map_generation() -> u32;

    #[wasm_bindgen(catch, js_name = fetchMapTile)]
    async fn fetch_map_tile(
        url: &str,
        z: u8,
        x: u32,
        y: u32,
        generation: u32,
    ) -> Result<JsValue, JsValue>;

}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or("window unavailable")?;
    let document = window.document().ok_or("document unavailable")?;
    let mut state = ClientState::default();
    state.update(Action::Resize(viewport(&window)));
    let reduced_motion = window
        .match_media("(prefers-reduced-motion: reduce)")?
        .is_some_and(|query| query.matches());
    state.update(Action::SetReducedMotion(reduced_motion));
    APP.with(|slot| {
        *slot.borrow_mut() = Some(App {
            state,
            document,
            requested_tiles: BTreeSet::new(),
            wanted_tiles: BTreeSet::new(),
            map_generation: 0,
            overlay_loading: false,
            terrain_loading: false,
            last_frame: None,
            frame_pending: false,
            drag_from: None,
        })
    });
    render();
    install_events(&window)?;

    spawn_local(async {
        if let Err(error) = load_bootstrap().await {
            set_status(&format!("V2 bootstrap failed: {error:?}"));
        }
    });
    Ok(())
}

async fn load_bootstrap() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or("window unavailable")?;
    let response = JsFuture::from(window.fetch_with_str("/api/v2/bootstrap")).await?;
    let response: Response = response.dyn_into()?;
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()).into());
    }
    let json = JsFuture::from(response.text()?).await?;
    let bootstrap: Bootstrap = serde_json::from_str(&json.as_string().unwrap_or_default())
        .map_err(|error| JsValue::from_str(&error.to_string()))?;
    bootstrap.validate().map_err(JsValue::from_str)?;
    APP.with(|slot| {
        if let Some(app) = slot.borrow_mut().as_mut() {
            app.state.update(Action::BootstrapLoaded(bootstrap));
        }
    });
    render();
    Ok(())
}

fn install_events(window: &Window) -> Result<(), JsValue> {
    let resize = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(window) = web_sys::window() {
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    app.state.update(Action::Resize(viewport(&window)));
                }
            });
            schedule_frame();
        }
    });
    window.add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref())?;
    resize.forget();

    let theme = Closure::<dyn FnMut(Event)>::new(move |_| {
        APP.with(|slot| {
            if let Some(app) = slot.borrow_mut().as_mut() {
                app.state.update(Action::ToggleTheme);
            }
        });
        schedule_frame();
    });
    let document = window.document().ok_or("document unavailable")?;
    document
        .get_element_by_id("theme-toggle")
        .ok_or("theme toggle unavailable")?
        .add_event_listener_with_callback("click", theme.as_ref().unchecked_ref())?;
    theme.forget();

    let package = Closure::<dyn FnMut(Event)>::new(move |_| {
        APP.with(|slot| {
            if let Some(app) = slot.borrow_mut().as_mut() {
                app.state.update(Action::CycleRenderPackage);
            }
        });
        schedule_frame();
    });
    document
        .get_element_by_id("package-toggle")
        .ok_or("package toggle unavailable")?
        .add_event_listener_with_callback("click", package.as_ref().unchecked_ref())?;
    package.forget();

    let keydown = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        let key = event.key();
        let current = APP.with(|slot| slot.borrow().as_ref().map(|app| app.state.section));
        let section = match key.as_str() {
            "2" => Some(Section::Experience),
            "1" | "Escape" => Some(Section::Home),
            "Enter" if current == Some(Section::Home) => Some(Section::Experience),
            _ => None,
        };
        let command = (current == Some(Section::Experience))
            .then(|| match key.as_str() {
                "n" => Some(MapCommand::Next),
                "b" => Some(MapCommand::Previous),
                "Enter" => Some(MapCommand::Replay),
                "h" | "ArrowLeft" => Some(MapCommand::Pan(-0.18, 0.0)),
                "l" | "ArrowRight" => Some(MapCommand::Pan(0.18, 0.0)),
                "k" | "ArrowUp" => Some(MapCommand::Pan(0.0, -0.18)),
                "j" | "ArrowDown" => Some(MapCommand::Pan(0.0, 0.18)),
                "+" | "=" => Some(MapCommand::Zoom(0.35)),
                "-" | "_" => Some(MapCommand::Zoom(-0.35)),
                "u" => Some(MapCommand::Tilt(0.08)),
                "o" => Some(MapCommand::Tilt(-0.08)),
                "," => Some(MapCommand::Bearing(-0.10)),
                "." => Some(MapCommand::Bearing(0.10)),
                "m" => Some(MapCommand::ToggleCamera),
                "v" => Some(MapCommand::ToggleTerrain),
                "t" => Some(MapCommand::ToggleLabels),
                "f" => Some(MapCommand::CycleFocus),
                "r" => Some(MapCommand::CycleRoads),
                "[" => Some(MapCommand::RoadWeight(-0.15)),
                "]" => Some(MapCommand::RoadWeight(0.15)),
                _ => None,
            })
            .flatten();
        if section.is_some() || command.is_some() || matches!(key.as_str(), "i" | "p" | "c") {
            event.prevent_default();
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    if let Some(section) = section {
                        app.state.update(Action::Navigate(section));
                    }
                    if let Some(command) = command {
                        app.state.update(Action::MapCommand(command));
                    }
                    if key == "i" {
                        app.state.update(Action::ToggleTheme);
                    }
                    if key == "p" {
                        app.state.update(Action::CycleRenderPackage);
                    }
                    if key == "c" {
                        app.state.update(Action::ToggleRenderColor);
                    }
                }
            });
            if section == Some(Section::Experience) || command.is_some() {
                request_map_assets();
            }
            schedule_frame();
        }
    });
    window.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref())?;
    keydown.forget();

    let stage = document
        .get_element_by_id("stage")
        .ok_or("stage unavailable")?;
    let pointer_down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event.button() != 0 {
            return;
        }
        let point = APP.with(|slot| {
            let mut slot = slot.borrow_mut();
            let app = slot.as_mut()?;
            let point = map_point(app, event.client_x() as f64, event.client_y() as f64, true)?;
            app.drag_from = Some(point);
            Some(point)
        });
        if point.is_some() {
            event.prevent_default();
        }
    });
    stage.add_event_listener_with_callback("pointerdown", pointer_down.as_ref().unchecked_ref())?;
    pointer_down.forget();

    let pointer_move = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let changed = APP.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(app) = slot.as_mut() else {
                return false;
            };
            let Some(from) = app.drag_from else {
                return false;
            };
            let Some(to) = map_point(app, event.client_x() as f64, event.client_y() as f64, false)
            else {
                return false;
            };
            app.drag_from = Some(to);
            app.state
                .update(Action::MapCommand(MapCommand::Drag(from, to)));
            true
        });
        if changed {
            event.prevent_default();
            schedule_frame();
            request_map_assets();
        }
    });
    window
        .add_event_listener_with_callback("pointermove", pointer_move.as_ref().unchecked_ref())?;
    pointer_move.forget();

    let pointer_up = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        if event.button() == 0 {
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    app.drag_from = None;
                }
            });
        }
    });
    window.add_event_listener_with_callback("pointerup", pointer_up.as_ref().unchecked_ref())?;
    window
        .add_event_listener_with_callback("pointercancel", pointer_up.as_ref().unchecked_ref())?;
    pointer_up.forget();

    let wheel = Closure::<dyn FnMut(WheelEvent)>::new(move |event: WheelEvent| {
        let delta = match event.delta_y().partial_cmp(&0.0) {
            Some(std::cmp::Ordering::Less) => 0.30,
            Some(std::cmp::Ordering::Greater) => -0.30,
            _ => return,
        };
        let changed = APP.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(app) = slot.as_mut() else {
                return false;
            };
            let Some(anchor) =
                map_point(app, event.client_x() as f64, event.client_y() as f64, true)
            else {
                return false;
            };
            app.state
                .update(Action::MapCommand(MapCommand::ZoomAt(delta, anchor)));
            true
        });
        if changed {
            event.prevent_default();
            schedule_frame();
            request_map_assets();
        }
    });
    stage.add_event_listener_with_callback("wheel", wheel.as_ref().unchecked_ref())?;
    wheel.forget();

    let click = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
        let destination = APP.with(|slot| {
            let slot = slot.borrow();
            let app = slot.as_ref()?;
            let cell_x = (event.client_x() as f64 / CELL_WIDTH).floor() as u16;
            let cell_y = (event.client_y() as f64 / CELL_HEIGHT).floor() as u16;
            app.state
                .scene()
                .hits
                .iter()
                .rev()
                .filter(|hit| matches!(hit.id.as_str(), "home" | "experience"))
                .find(|hit| {
                    cell_x >= hit.x
                        && cell_x < hit.x.saturating_add(hit.width)
                        && cell_y >= hit.y.saturating_sub(1)
                        && cell_y < hit.y.saturating_add(hit.height).saturating_add(1)
                })
                .and_then(|hit| match hit.id.as_str() {
                    "home" => Some(Section::Home),
                    "experience" => Some(Section::Experience),
                    _ => None,
                })
        });
        let Some(destination) = destination else {
            return;
        };
        event.prevent_default();
        APP.with(|slot| {
            if let Some(app) = slot.borrow_mut().as_mut() {
                app.state.update(Action::Navigate(destination));
            }
        });
        if destination == Section::Experience {
            request_map_assets();
        }
        schedule_frame();
    });
    stage.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    click.forget();
    Ok(())
}

fn map_point(app: &App, x: f64, y: f64, require_inside: bool) -> Option<[f64; 2]> {
    if app.state.section != Section::Experience {
        return None;
    }
    let viewport = app.state.viewport;
    let gutter = if viewport.cols >= 90 { 7.0 } else { 0.0 };
    let cell_width = viewport.width as f64 / viewport.cols as f64;
    let cell_x = (x / cell_width).floor();
    let cell_y = (y / CELL_HEIGHT).floor();
    let map_bottom = viewport.rows.saturating_sub(2) as f64;
    if require_inside
        && (cell_x < gutter
            || cell_x >= viewport.cols as f64
            || cell_y < 1.0
            || cell_y >= map_bottom)
    {
        return None;
    }
    let local_x = (cell_x - gutter).max(0.0);
    let local_y = (cell_y - 1.0).max(0.0);
    Some([local_x * 2.0 + 1.0, local_y * 4.0 + 2.0])
}

fn request_map_assets() {
    let (demand, generation, load_overlay, load_terrain) = APP
        .with(|slot| {
            let mut slot = slot.borrow_mut();
            let app = slot.as_mut()?;
            let demand = app.state.map_demand()?;
            let wanted = demand.tiles.iter().copied().collect::<BTreeSet<_>>();
            let demand_changed = wanted != app.wanted_tiles;
            if demand_changed {
                app.wanted_tiles = wanted;
                // Starting a generation cancels every older PMTiles read. None
                // of those in-flight IDs may suppress its replacement.
                app.requested_tiles.clear();
                app.map_generation = begin_map_generation();
            }
            let load_overlay = app.state.map_needs_overlay() && !app.overlay_loading;
            let load_terrain = app.state.map_needs_terrain() && !app.terrain_loading;
            app.overlay_loading |= load_overlay;
            app.terrain_loading |= load_terrain;
            Some((demand, app.map_generation, load_overlay, load_terrain))
        })
        .unwrap_or_else(|| {
            (
                portfolio_v2_client_core::MapDemand { tiles: Vec::new() },
                0,
                false,
                false,
            )
        });

    if load_overlay {
        spawn_local(async {
            let result = fetch_asset("/map/v2/states.tmap", 2 * 1024 * 1024)
                .await
                .and_then(|bytes| {
                    let text = String::from_utf8(bytes)
                        .map_err(|_| JsValue::from_str("state map is not UTF-8"))?;
                    Ok(termap::data::Tile::new(termap::data::parse_features(&text)))
                });
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    app.overlay_loading = false;
                    if let Ok(tile) = result {
                        app.state.update(Action::MapOverlay(tile));
                    }
                }
            });
            schedule_frame();
        });
    }

    if load_terrain {
        spawn_local(async {
            let result = fetch_asset("/map/v2/terrain.tmhg", 64 * 1024 * 1024)
                .await
                .and_then(|bytes| {
                    termap::terrain::Terrain::from_bytes(bytes)
                        .map_err(|error| JsValue::from_str(&error.to_string()))
                });
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    app.terrain_loading = false;
                    if let Ok(terrain) = result {
                        app.state.update(Action::MapTerrain(terrain));
                    }
                }
            });
            schedule_frame();
        });
    }

    for (z, x, y) in demand.tiles {
        let fresh = APP.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .is_some_and(|app| app.requested_tiles.insert((z, x, y)))
        });
        if !fresh {
            continue;
        }
        spawn_local(async move {
            let result = fetch_map_tile("/map/v2/vector.pmtiles", z, x, y, generation).await;
            let Ok(value) = result else {
                forget_failed_tile(generation, (z, x, y));
                return;
            };
            if value.is_null() || value.is_undefined() {
                forget_failed_tile(generation, (z, x, y));
                return;
            }
            let bytes = Uint8Array::new(&value).to_vec();
            let features = termap::mvt::decode(&bytes, termap::pmtiles::TileId { z, x, y });
            if features.is_empty() {
                forget_failed_tile(generation, (z, x, y));
                return;
            }
            let tile = termap::data::Tile::new(features);
            APP.with(|slot| {
                if let Some(app) = slot.borrow_mut().as_mut() {
                    app.state.update(Action::MapTile { z, x, y, tile });
                }
            });
            schedule_frame();
        });
    }
}

fn forget_failed_tile(generation: u32, tile: (u8, u32, u32)) {
    APP.with(|slot| {
        if let Some(app) = slot.borrow_mut().as_mut() {
            if app.map_generation == generation {
                app.requested_tiles.remove(&tile);
            }
        }
    });
}

fn schedule_frame() {
    let should_schedule = APP.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(app) = slot.as_mut() else {
            return false;
        };
        if app.frame_pending {
            return false;
        }
        app.frame_pending = true;
        true
    });
    if !should_schedule {
        return;
    }
    let callback = Closure::once_into_js(move |timestamp: f64| {
        let animating = APP.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(app) = slot.as_mut() else {
                return false;
            };
            app.frame_pending = false;
            let elapsed = app.last_frame.replace(timestamp).map_or(0.0, |last| {
                ((timestamp - last).max(0.0) / 1000.0).min(MAX_ANIMATION_STEP)
            });
            app.state.update(Action::Tick(elapsed));
            app.state.animating()
        });
        render();
        request_map_assets();
        if animating {
            schedule_frame();
        }
    });
    if let Some(window) = web_sys::window() {
        let _ = window.request_animation_frame(callback.unchecked_ref());
    }
}

async fn fetch_asset(url: &str, max_bytes: usize) -> Result<Vec<u8>, JsValue> {
    let window = web_sys::window().ok_or("window unavailable")?;
    let response = JsFuture::from(window.fetch_with_str(url)).await?;
    let response: Response = response.dyn_into()?;
    if !response.ok() {
        return Err(format!("asset request failed ({})", response.status()).into());
    }
    if response
        .headers()
        .get("content-length")?
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > max_bytes)
    {
        return Err("asset is too large".into());
    }
    let buffer = JsFuture::from(response.array_buffer()?).await?;
    let bytes = Uint8Array::new(&buffer).to_vec();
    if bytes.len() > max_bytes {
        return Err("asset is too large".into());
    }
    Ok(bytes)
}

fn render() {
    APP.with(|slot| {
        let app = slot.borrow();
        let Some(app) = app.as_ref() else { return };
        let frame = app.state.render_frame();
        if let Ok(frame) = serde_json::to_string(&frame) {
            render_package(&frame);
        }
        if let Some(home) = app.state.semantic_home() {
            if let Some(semantic) = app.document.get_element_by_id("semantic") {
                semantic.set_inner_html(&render_semantic(&home.profile));
            }
            set_status("");
        }
        if let Some(root) = app.document.document_element() {
            root.set_attribute(
                "data-theme",
                match app.state.theme {
                    Theme::Dark => "dark",
                    Theme::Light => "light",
                },
            )
            .ok();
        }
        let (package_name, package_next, package_index) = match app.state.render_package {
            portfolio_v2_client_core::RenderPackage::Canonical => ("CANONICAL", "CRT", 0),
            portfolio_v2_client_core::RenderPackage::Crt => ("CRT", "VHS", 1),
            portfolio_v2_client_core::RenderPackage::Vhs => ("VHS", "INK", 2),
            portfolio_v2_client_core::RenderPackage::Ink => ("INK", "CANONICAL", 3),
        };
        if let Some(name) = app.document.get_element_by_id("package-name") {
            name.set_text_content(Some(package_name));
        }
        if let Some(toggle) = app.document.get_element_by_id("package-toggle") {
            toggle
                .set_attribute(
                    "aria-label",
                    &format!("Render package: {package_name}; activate for {package_next}"),
                )
                .ok();
        }
        let theme = match app.state.theme {
            Theme::Dark => "dark",
            Theme::Light => "light",
        };
        if let Some(toggle) = app.document.get_element_by_id("theme-toggle") {
            toggle
                .set_attribute(
                    "aria-label",
                    if app.state.theme == Theme::Dark {
                        "Theme: dark; activate for light"
                    } else {
                        "Theme: light; activate for dark"
                    },
                )
                .ok();
        }
        paint_controls(theme, package_index);
    });
}

fn render_semantic(profile: &portfolio_v2_protocol::Profile) -> String {
    let contacts = profile
        .contacts
        .iter()
        .map(|contact| {
            format!(
                r#"<li><a href="{}">{}: {}</a></li>"#,
                escape(&contact.href),
                escape(&contact.label),
                escape(&contact.value)
            )
        })
        .collect::<String>();
    format!(
        r#"<article><h1>{}</h1><p>{}, {}</p><p>{}</p><h2>Now</h2><p>{}</p><ul>{contacts}</ul></article>"#,
        escape(&profile.name),
        escape(&profile.role),
        escape(&profile.location),
        escape(&profile.pitch),
        escape(&profile.now)
    )
}

fn viewport(window: &Window) -> Viewport {
    let width = window
        .inner_width()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(1280.0) as f32;
    let height = window
        .inner_height()
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(780.0) as f32;
    Viewport {
        width,
        height,
        scale: window.device_pixel_ratio() as f32,
        cols: ((f64::from(width) / CELL_WIDTH).floor() as u16).clamp(20, 180),
        rows: ((f64::from(height) / CELL_HEIGHT).floor() as u16).clamp(6, 60),
    }
}

fn set_status(value: &str) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Some(status) = document.get_element_by_id("status") else {
        return;
    };
    status.set_text_content(Some(value));
    status
        .set_attribute(
            "data-empty",
            if value.is_empty() { "true" } else { "false" },
        )
        .ok();
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
