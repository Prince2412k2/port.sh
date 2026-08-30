use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use portfolio_v2_protocol::{Bootstrap, Contact, NavigationItem, Profile, VERSION};
use sha2::{Digest, Sha256};
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    bootstrap: Arc<Bootstrap>,
    etag: HeaderValue,
}

#[tokio::main]
async fn main() {
    let bootstrap = bootstrap();
    bootstrap.validate().expect("valid V2 bootstrap");
    let etag = HeaderValue::from_str(&format!("\"{}\"", bootstrap.revision)).expect("valid etag");
    let state = AppState {
        bootstrap: Arc::new(bootstrap),
        etag,
    };
    let web_dir = env::var_os("PORTFOLIO_V2_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("crates/browser/dist"));
    let map_dir = env::var_os("PORTFOLIO_V2_MAP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../map-data"));
    let app = Router::new()
        .route("/api/v2/health", get(health))
        .route("/api/v2/bootstrap", get(get_bootstrap))
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
        .with_state(state);

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

async fn get_bootstrap(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if headers.get(header::IF_NONE_MATCH) == Some(&state.etag) {
        return (StatusCode::NOT_MODIFIED, HeaderMap::new(), String::new());
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::ETAG, state.etag.clone());
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let body = serde_json::to_string(state.bootstrap.as_ref()).expect("serialize bootstrap");
    (StatusCode::OK, response_headers, body)
}

fn bootstrap() -> Bootstrap {
    let source = load_about();
    let profile = parse_about(&source);
    let revision = format!("{:x}", Sha256::digest(source.as_bytes()));
    Bootstrap {
        protocol: VERSION,
        revision,
        profile,
        navigation: ["home", "experience", "projects", "skills", "taste", "ask"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| NavigationItem {
                id: id.into(),
                label: format!("{}  {}", index + 1, id.to_uppercase()),
                available: matches!(id, "home" | "experience"),
            })
            .collect(),
    }
}

fn load_about() -> String {
    env::var_os("PORTFOLIO_V2_ABOUT")
        .and_then(|path| std::fs::read_to_string(path).ok())
        .or_else(|| std::fs::read_to_string("../portfolio/data/about.txt").ok())
        .or_else(|| std::fs::read_to_string("portfolio/data/about.txt").ok())
        .unwrap_or_else(|| include_str!("../../../../portfolio/data/about.txt").into())
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
