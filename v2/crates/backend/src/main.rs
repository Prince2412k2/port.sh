use std::{
    env,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::get,
    Router,
};
use portfolio_v2_protocol::{Bootstrap, Contact, NavigationItem, Profile, VERSION};
use sha2::{Digest, Sha256};
use tower_http::services::{ServeDir, ServeFile};

// Data-only archive reader: the backend does not link the map renderer.
#[path = "../../../../map/src/pmtiles.rs"]
mod archive;
// Alias used by the shared data-only terrain sampler, not a map renderer.
mod pmtiles {
    pub use super::archive::TileId;
}
mod sessions;
mod terrain_tiles;
mod web_tools;

#[derive(Clone)]
struct AppState {
    bootstrap: Arc<Bootstrap>,
    etag: HeaderValue,
    map: Arc<Mutex<Option<archive::Archive>>>,
    map_revision: String,
    tile_slots: Arc<tokio::sync::Semaphore>,
    searches: Arc<
        tokio::sync::Mutex<
            std::collections::BTreeMap<String, Vec<portfolio_v2_protocol::map::SearchResult>>,
        >,
    >,
    buildings: bool,
    terrain: Option<Arc<terrain_tiles::Source>>,
}

#[tokio::main]
async fn main() {
    let bootstrap = bootstrap();
    bootstrap.validate().expect("valid V2 bootstrap");
    let etag = HeaderValue::from_str(&format!("\"{}\"", bootstrap.revision)).expect("valid etag");
    let web_dir = env::var_os("PORTFOLIO_V2_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/browser/dist"));
    let map_dir = env::var_os("PORTFOLIO_V2_MAP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../map-data"));
    let mut digest = Sha256::new();
    digest.update(include_str!("terrain_tiles.rs").as_bytes());
    digest.update(include_str!("../../../../map/src/terrain.rs").as_bytes());
    for name in [
        "vector.pmtiles",
        "states.tmap",
        "terrain.tmhg",
        "buildings.tmap",
    ] {
        digest.update(name.as_bytes());
        if let Ok(mut file) = std::fs::File::open(map_dir.join(name)) {
            use std::io::Read;
            let mut buffer = [0u8; 65536];
            while let Ok(count) = file.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
        }
    }
    let state = AppState {
        bootstrap: Arc::new(bootstrap),
        etag,
        map: Arc::new(Mutex::new(
            archive::Archive::open(&map_dir.join("vector.pmtiles")).ok(),
        )),
        map_revision: format!("{:x}", digest.finalize()),
        tile_slots: Arc::new(tokio::sync::Semaphore::new(16)),
        searches: Default::default(),
        buildings: map_dir.join("buildings.tmap").is_file(),
        terrain: terrain_tiles::Source::open(&map_dir.join("terrain.tmhg"))
            .ok()
            .map(Arc::new),
    };
    let revision = state.map_revision.clone();
    let session_service = sessions::Service::open(
        env::var_os("PORTFOLIO_V2_SESSION_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("sessions")),
        &state.bootstrap,
    )
    .expect("open authoritative session store");
    let immutable = tower_http::set_header::SetResponseHeaderLayer::if_not_present(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    let assets = Router::new()
        .route_service(
            "/vector.pmtiles",
            ServeFile::new(map_dir.join("vector.pmtiles")),
        )
        .route_service("/states.tmap", ServeFile::new(map_dir.join("states.tmap")))
        .route_service(
            "/terrain.tmhg",
            ServeFile::new(map_dir.join("terrain.tmhg")),
        )
        .route_service(
            "/buildings.tmap",
            ServeFile::new(map_dir.join("buildings.tmap")),
        )
        .route("/tiles/{z}/{x}/{y}", get(get_tile))
        .route("/terrain/{z}/{x}/{y}", get(get_terrain_tile))
        .layer(immutable)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            asset_validators,
        ));
    let app = Router::new()
        .route("/api/v2/health", get(health))
        .route("/api/v2/bootstrap", get(get_bootstrap))
        .route("/api/v2/map", get(get_map))
        .route("/api/v2/geocode", get(geocode))
        .nest(&format!("/map/v2/{revision}"), assets)
        .route_service(
            "/map/v2/vector.pmtiles",
            ServeFile::new(map_dir.join("vector.pmtiles")),
        )
        .route_service(
            "/map/v2/states.tmap",
            ServeFile::new(map_dir.join("states.tmap")),
        )
        .route_service(
            "/map/v2/terrain.tmhg",
            ServeFile::new(map_dir.join("terrain.tmhg")),
        )
        .nest_service(
            "/v2",
            ServeDir::new(web_dir).append_index_html_on_directories(true),
        )
        .nest_service(
            "/downloads/v2",
            ServeDir::new(
                env::var_os("PORTFOLIO_V2_DOWNLOAD_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("downloads")),
            ),
        )
        .with_state(state)
        .merge(sessions::routes(session_service))
        .layer(
            tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            ),
        )
        .layer(middleware::from_fn(web_bundle_cache));

    let address: SocketAddr = env::var("PORTFOLIO_V2_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8322".into())
        .parse()
        .expect("valid PORTFOLIO_V2_ADDR");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind V2 server");
    println!("portfolio V2 listening on http://{address}/v2/");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .expect("serve V2");
}

async fn health() -> &'static str {
    "ok\n"
}
async fn geocode(
    State(state): State<AppState>,
    Query(query): Query<std::collections::BTreeMap<String, String>>,
) -> Result<axum::Json<Vec<portfolio_v2_protocol::map::SearchResult>>, (StatusCode, &'static str)> {
    let text = query.get("q").map(String::as_str).unwrap_or("").trim();
    if text.len() < 3 || text.len() > 128 {
        return Err((StatusCode::BAD_REQUEST, "Search requires 3–128 bytes"));
    }
    let mut cache = state.searches.try_lock().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Search is busy; retry shortly",
        )
    })?;
    if let Some(results) = cache.get(text) {
        return Ok(axum::Json(results.clone()));
    }
    // Coalesce identical lookups and respect the public geocoder's rate limit.
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("portfolio-v2/0.1 (sniffkin.tech)")
        .build()
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "Search unavailable"))?;
    let response = client
        .get("https://nominatim.openstreetmap.org/search")
        .query(&[("q", text), ("format", "jsonv2"), ("limit", "8")])
        .send()
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "Search unavailable"))?;
    if !response.status().is_success() {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Search unavailable"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "Search unavailable"))?;
    if bytes.len() > 128 * 1024 {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "Search result too large"));
    }
    let values: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "Invalid search result"))?;
    let results = values
        .into_iter()
        .take(8)
        .filter_map(|value| {
            let result = portfolio_v2_protocol::map::SearchResult {
                id: format!("osm:{}", value["place_id"]),
                name: value["display_name"].as_str()?.chars().take(180).collect(),
                lon: value["lon"].as_str()?.parse().ok()?,
                lat: value["lat"].as_str()?.parse().ok()?,
            };
            result.valid().then_some(result)
        })
        .collect::<Vec<_>>();
    if cache.len() >= 128 {
        cache.pop_first();
    }
    cache.insert(text.into(), results.clone());
    Ok(axum::Json(results))
}

async fn web_bundle_cache(request: Request, next: Next) -> axum::response::Response {
    let immutable = request.uri().path().starts_with("/v2/build/");
    let mut response = next.run(request).await;
    if immutable
        && (response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED)
    {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

async fn asset_validators(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> axum::response::Response {
    let etag = HeaderValue::from_str(&format!("\"{}\"", state.map_revision)).unwrap();
    let unchanged = request.headers().get(header::IF_NONE_MATCH) == Some(&etag);
    if request.headers().get(header::IF_RANGE) == Some(&etag) {
        // ServeFile knows Last-Modified, while the publication layer owns the
        // content digest. An exact digest validates the requested byte range.
        request.headers_mut().remove(header::IF_RANGE);
    }
    let mut response = next.run(request).await;
    if response.status().is_success() {
        if unchanged {
            response = StatusCode::NOT_MODIFIED.into_response();
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            );
        }
        response.headers_mut().insert(header::ETAG, etag);
    }
    response
}

async fn get_bootstrap(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::ETAG, state.etag.clone());
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    if headers.get(header::IF_NONE_MATCH) == Some(&state.etag) {
        return (StatusCode::NOT_MODIFIED, response_headers, String::new());
    }
    let body = serde_json::to_string(state.bootstrap.as_ref()).expect("serialize bootstrap");
    (StatusCode::OK, response_headers, body)
}

async fn get_map(State(state): State<AppState>) -> impl IntoResponse {
    let available = state.map.lock().map(|map| map.is_some()).unwrap_or(false);
    (
        [(header::CACHE_CONTROL, "no-cache")],
        axum::Json(serde_json::json!({
            "protocol": VERSION, "revision": state.map_revision,
            "base": format!("/map/v2/{}", state.map_revision), "available": available,
            "buildings":state.buildings,
            "terrain_format":"tmhg-tiles-v1","terrain_available":state.terrain.is_some(),
        })),
    )
}

async fn get_tile(
    State(state): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> axum::response::Response {
    if z > 14 || x >= (1u32 << z) || y >= (1u32 << z) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(permit) = state.tile_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::RETRY_AFTER, "1"),
            ],
        )
            .into_response();
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut map = state.map.lock().map_err(|_| ())?;
        map.as_mut()
            .ok_or(())?
            .tile(archive::TileId { z, x, y })
            .map_err(|_| ())
    })
    .await;
    match result {
        Ok(Ok(Some(bytes))) if bytes.len() <= 8 * 1024 * 1024 => (
            [(header::CONTENT_TYPE, "application/vnd.mapbox-vector-tile")],
            bytes,
        )
            .into_response(),
        Ok(Ok(None)) => StatusCode::NO_CONTENT.into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CACHE_CONTROL, "no-store")],
            "map data unavailable",
        )
            .into_response(),
    }
}
async fn get_terrain_tile(
    State(state): State<AppState>,
    Path((z, x, y)): Path<(u8, u32, u32)>,
) -> axum::response::Response {
    if z > 14 || x >= (1u32 << z) || y >= (1u32 << z) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(permit) = state.tile_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response();
    };
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        terrain_tiles::tile(state.terrain.as_deref(), z, x, y)
    })
    .await
    {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn bootstrap() -> Bootstrap {
    let source = load_about();
    let profile = parse_about(&source);
    let mut value = Bootstrap {
        protocol: VERSION,
        revision: String::new(),
        profile,
        navigation: ["home", "experience", "projects", "skills", "taste", "ask"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| NavigationItem {
                id: id.into(),
                label: format!("{}  {}", index + 1, id.to_uppercase()),
                available: true,
            })
            .collect(),
        projects: portfolio_v2_protocol::parse_projects(&load_sheet(
            "PORTFOLIO_V2_PROJECTS",
            include_str!("../../../../skills/data/projects.txt"),
        ))
        .expect("authored projects"),
        taste: portfolio_v2_protocol::taste::parse(&load_sheet(
            "PORTFOLIO_V2_TASTE",
            include_str!("../../../../portfolio/data/taste.txt"),
        )),
        places: portfolio_v2_protocol::parse_places(&load_sheet(
            "PORTFOLIO_V2_PLACES",
            include_str!("../../../../map/data/places.txt"),
        ))
        .expect("authored experience"),
    };
    value.revision = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&value).expect("bootstrap encoding"))
    );
    value
}

fn load_about() -> String {
    env::var_os("PORTFOLIO_V2_ABOUT")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .or_else(|| std::fs::read_to_string("../portfolio/data/about.txt").ok())
        .or_else(|| std::fs::read_to_string("portfolio/data/about.txt").ok())
        .unwrap_or_else(|| include_str!("../../../../portfolio/data/about.txt").into())
}
fn load_sheet(variable: &str, baked: &str) -> String {
    match env::var_os(variable) {
        Some(path) => std::fs::read_to_string(path).expect("read configured content sheet"),
        None => baked.into(),
    }
}

fn parse_about(source: &str) -> Profile {
    let mut values = std::collections::BTreeMap::<String, String>::new();
    let mut last = String::new();
    for line in source.lines() {
        let line = line.trim_end();
        let bare = line.trim_start();
        if bare.is_empty() || bare.starts_with('#') {
            continue;
        }
        let indent = line.len() - bare.len();
        if indent >= 4 && !last.is_empty() {
            values.entry(last.clone()).and_modify(|value| {
                value.push(' ');
                value.push_str(bare);
            });
            continue;
        }
        let (key, value) = bare.split_once(char::is_whitespace).unwrap_or((bare, ""));
        last = key.into();
        values.insert(last.clone(), value.trim().into());
    }
    let mut take = |key: &str| values.remove(key).unwrap_or_default();
    let name = take("name");
    let role = take("role");
    let location = take("where");
    let handle = take("handle");
    let pitch = take("pitch");
    let now = take("now");
    let email = take("email");
    let github = take("github");
    let ssh = take("ssh");
    let mosh = take("mosh");
    Profile {
        name,
        role,
        location,
        handle,
        pitch,
        now,
        contacts: vec![
            Contact {
                id: "email".into(),
                label: "Email".into(),
                href: format!("mailto:{email}"),
                value: email,
            },
            Contact {
                id: "github".into(),
                label: "GitHub".into(),
                href: format!("https://{github}"),
                value: github,
            },
            Contact {
                id: "ssh".into(),
                label: "SSH".into(),
                href: "ssh://sniffkin.tech".into(),
                value: ssh,
            },
            Contact {
                id: "mosh".into(),
                label: "Mosh".into(),
                href: "https://mosh.org".into(),
                value: mosh,
            },
        ],
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
