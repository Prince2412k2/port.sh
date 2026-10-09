use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind},
    execute, terminal,
};
use portfolio_v2_client_core::{
    map_store::{Generation, MapStore, TileKey},
    Action, ClientState, MapCommand, Viewport,
};
use portfolio_v2_scene::{Cell, CellSurface, Rgba8};
use std::{
    env,
    io::{self, IsTerminal, Write},
    sync::{mpsc, Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};

mod renderer;
mod session;
use renderer::AnsiRenderer;

type Mailbox<T> = Arc<(Mutex<Option<T>>, Condvar)>;
fn mailbox<T>() -> Mailbox<T> {
    Arc::new((Mutex::new(None), Condvar::new()))
}
fn offer<T>(queue: &Mailbox<T>, value: T) {
    *queue.0.lock().unwrap() = Some(value);
    queue.1.notify_one();
}
fn take<T>(queue: &Mailbox<T>) -> T {
    let mut slot = queue.0.lock().unwrap();
    loop {
        if let Some(value) = slot.take() {
            return value;
        }
        slot = queue.1.wait(slot).unwrap();
    }
}

fn fetch(endpoint: &str, path: &str, limit: usize) -> Result<Vec<u8>, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .get(format!("{endpoint}{path}"))
        .call()
        .map_err(|e| e.to_string())?;
    response
        .body_mut()
        .with_config()
        .limit(limit as u64)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

struct Terminal;
impl Terminal {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let guard = Self;
        execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide,
            event::EnableMouseCapture
        )?;
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            event::DisableMouseCapture,
            crossterm::style::ResetColor,
            cursor::Show,
            terminal::LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

enum Loaded {
    Generation(Vec<TileKey>, Generation),
    Complete(Vec<TileKey>, Generation, termap::terrain::Terrain),
    Overlay(Vec<u8>),
    Terrain(Vec<u8>),
    Buildings(Vec<u8>),
    Status(String),
}

fn loader(endpoint: String, requests: Mailbox<Vec<TileKey>>, results: mpsc::SyncSender<Loaded>) {
    let mut wanted = take(&requests);
    let manifest = fetch(&endpoint, "/api/v2/map", 8192).and_then(|bytes| {
        serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|e| e.to_string())
    });
    let has_buildings = manifest
        .as_ref()
        .is_ok_and(|value| value["buildings"] == true);
    let tiled_terrain = manifest
        .as_ref()
        .is_ok_and(|value| value["terrain_format"] == "tmhg-tiles-v1");
    let base = match manifest {
        Ok(value) if value["available"] == true => value["base"].as_str().unwrap_or("").to_owned(),
        _ => {
            let _ = results.send(Loaded::Status("Map archive unavailable on backend".into()));
            return;
        }
    };
    if !base.starts_with("/map/v2/") || base.contains("..") {
        return;
    }
    for (name, limit, terrain) in [
        ("states.tmap", 2 * 1024 * 1024, false),
        ("terrain.tmhg", 64 * 1024 * 1024, true),
    ] {
        if terrain && tiled_terrain {
            continue;
        }
        match fetch(&endpoint, &format!("{base}/{name}"), limit) {
            Ok(bytes) => {
                if results
                    .send(if terrain {
                        Loaded::Terrain(bytes)
                    } else {
                        Loaded::Overlay(bytes)
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                let _ = results.send(Loaded::Status(format!("{name}: {error}")));
            }
        }
    }
    let mut cache = MapStore::default();
    let mut buildings_loaded = false;
    loop {
        let mut failed = false;
        if has_buildings && !buildings_loaded && wanted.iter().any(|tile| tile.0 >= 14) {
            if let Ok(bytes) = fetch(
                &endpoint,
                &format!("{base}/buildings.tmap"),
                8 * 1024 * 1024,
            ) {
                if results.send(Loaded::Buildings(bytes)).is_err() {
                    return;
                }
                buildings_loaded = true;
            }
        }
        cache.protect(&wanted);
        for key in wanted.clone() {
            // Navigation replaces pending work rather than queueing obsolete views.
            if let Some(next) = requests.0.lock().unwrap().take() {
                wanted = next;
                break;
            }
            if !cache.contains(&key) {
                match fetch(
                    &endpoint,
                    &format!("{base}/tiles/{}/{}/{}", key.0, key.1, key.2),
                    8 * 1024 * 1024,
                ) {
                    Ok(bytes) if cache.insert(key, &bytes) => (),
                    _ => {
                        failed = true;
                        break;
                    }
                }
            }
            if tiled_terrain && !cache.has_terrain(&key) {
                match fetch(
                    &endpoint,
                    &format!("{base}/terrain/{}/{}/{}", key.0, key.1, key.2),
                    64 * 1024,
                ) {
                    Ok(bytes) => {
                        if !cache.insert_terrain(key, bytes) {
                            failed = true;
                            break;
                        }
                    }
                    Err(_) => {
                        failed = true;
                        break;
                    }
                }
            }
        }
        if let Some(generation) = cache.generation(&wanted) {
            let result = if tiled_terrain {
                cache
                    .terrain_generation(&wanted)
                    .map(|terrain| Loaded::Complete(wanted.clone(), generation, terrain))
            } else {
                Some(Loaded::Generation(wanted.clone(), generation))
            };
            if let Some(result) = result {
                if results.send(result).is_err() {
                    return;
                }
                wanted = take(&requests);
            } else {
                failed = true;
            }
        }
        if failed {
            let _ = results.send(Loaded::Status(
                "Map loading failed; retaining previous view, retrying".into(),
            ));
            thread::sleep(Duration::from_secs(3));
            if let Some(next) = requests.0.lock().unwrap().take() {
                wanted = next;
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut resume = None;
    let mut endpoint =
        env::var("PORTFOLIO_V2_ENDPOINT").unwrap_or_else(|_| "http://127.0.0.1:8322".into());
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--session" => {
                let id = args.next().ok_or("--session requires a resume code")?;
                if !portfolio_v2_protocol::session::valid_id(&id) {
                    return Err("invalid session resume code".into());
                }
                resume = Some(id);
            }
            "--replay" => {
                return replay(&args.next().ok_or("--replay requires a fixture")?);
            }
            "--endpoint" => endpoint = args.next().ok_or("--endpoint requires a URL")?,
            "--help" | "-h" => {
                println!("portfolio-v2-native [--endpoint URL] [--session RESUME_CODE]\n1 home · 2 experience · 3 projects · 4 skills · 5 taste · 6 ask\narrows pan/select · +/- zoom · n/b tour · ? map search · / help · i theme\nAsk: Enter send · Ctrl+X cancel · /new fresh conversation · /reach message\nEscape home · q quit (outside Ask)\nPORTFOLIO_V2_COLOR=truecolor|256|16; PORTFOLIO_V2_ASCII=1");
                return Ok(());
            }
            "--version" => {
                println!(
                    "portfolio-v2-native {} (protocol 2)",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    let endpoint = endpoint.trim_end_matches('/').to_owned();
    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        return Err("endpoint must be an HTTP(S) URL".into());
    }
    let bootstrap = portfolio_v2_protocol::decode_bootstrap(&fetch(
        &endpoint,
        "/api/v2/bootstrap",
        portfolio_v2_protocol::MAX_MESSAGE_BYTES,
    )?)
    .map_err(|error| {
        format!("Backend compatibility error: {error:?}. Update the client and retry.")
    })?;
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        println!(
            "{}\n{} · {}\n\n{}\n\n{}",
            bootstrap.profile.name,
            bootstrap.profile.role,
            bootstrap.profile.location,
            bootstrap.profile.pitch,
            bootstrap.profile.now
        );
        return Ok(());
    }
    let _terminal = Terminal::enter()?;
    let frames: Mailbox<Option<CellSurface>> = mailbox();
    let output = frames.clone();
    let writer = thread::spawn(move || -> io::Result<()> {
        let mut renderer = AnsiRenderer::from_env();
        // Diffs are based only on frames actually written, never dropped frames.
        while let Some(frame) = take(&output) {
            let bytes = renderer.render(&frame);
            let mut stdout = io::stdout().lock();
            stdout.write_all(bytes.as_bytes())?;
            stdout.flush()?;
        }
        Ok(())
    });
    let requests = mailbox();
    let (tx, rx) = mpsc::sync_channel(4);
    let input = requests.clone();
    let map_endpoint = endpoint.clone();
    thread::spawn(move || loader(map_endpoint, input, tx));
    let ask_commands = mailbox();
    let ask_input = ask_commands.clone();
    let (ask_tx, ask_rx) = mpsc::sync_channel(2);
    let searches = mailbox();
    let search_input = searches.clone();
    let search_endpoint = endpoint.clone();
    let (search_tx, search_rx) = mpsc::sync_channel(2);
    thread::spawn(move || searcher(search_endpoint, search_input, search_tx));
    let mut last_search = None;
    let mut search_due = None;
    thread::spawn(move || session::run(endpoint, ask_input, ask_tx, resume));
    let mut pending_question: Option<portfolio_v2_protocol::session::Submit> = None;
    let mut active_session: Option<String> = None;
    let mut state = ClientState::default();
    state.update(Action::BootstrapLoaded(bootstrap));
    resize(&mut state, terminal::size()?);
    let mut last = Instant::now();
    let mut dirty = true;
    let mut wanted = Vec::new();
    let mut drag = None;
    let mut skill_drag: Option<(f64, f64)> = None;
    let mut status: Option<String> = None;
    let result = (|| -> io::Result<()> {
        loop {
            for (query, results) in search_rx.try_iter() {
                state.map_search_results(&query, results);
                dirty = true;
            }
            let query = state.map_search_query();
            if query != last_search {
                last_search = query.clone();
                search_due = Some(Instant::now() + Duration::from_millis(350));
                if let Some(query) = query {
                    if !query.is_empty() {
                        let results = state.local_map_search(&query);
                        state.map_search_results(&query, results);
                        dirty = true;
                    }
                }
            }
            if search_due.is_some_and(|deadline| Instant::now() >= deadline) {
                if let Some(query) = &last_search {
                    if query.len() >= 3 {
                        offer(&searches, query.clone());
                    }
                }
                search_due = None;
            }
            for update in ask_rx.try_iter() {
                match update {
                    session::Update::Snapshot(snapshot) => {
                        if active_session
                            .as_ref()
                            .is_some_and(|id| *id != snapshot.session_id)
                        {
                            continue;
                        }
                        active_session = Some(snapshot.session_id.clone());
                        if pending_question.as_ref().is_some_and(|p| {
                            snapshot
                                .exchanges
                                .iter()
                                .any(|e| e.request_id == p.request_id)
                        }) {
                            pending_question = None;
                        }
                        dirty |= state.ask_snapshot(snapshot);
                    }
                    session::Update::Reset(snapshot) => {
                        active_session = Some(snapshot.session_id.clone());
                        state.reset_session();
                        dirty |= state.ask_snapshot(snapshot);
                    }
                    session::Update::Status(message) => {
                        state.ask_status(&message);
                        dirty = true;
                    }
                }
            }
            for loaded in rx.try_iter() {
                match loaded {
                    Loaded::Generation(keys, tiles) if keys == wanted => {
                        state.update(Action::MapGeneration(tiles));
                        status = None;
                    }
                    Loaded::Generation(..) => continue,
                    Loaded::Complete(keys, tiles, terrain) if keys == wanted => {
                        state.update(Action::MapCompleteGeneration { tiles, terrain });
                        status = None;
                    }
                    Loaded::Complete(..) => continue,
                    Loaded::Overlay(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            state.update(Action::MapOverlay(termap::data::Tile::new(
                                termap::data::parse_features(&text),
                            )));
                        }
                    }
                    Loaded::Terrain(bytes) => {
                        if let Ok(terrain) = termap::terrain::Terrain::from_bytes(bytes) {
                            state.update(Action::MapTerrain(terrain));
                        }
                    }
                    Loaded::Status(message) => {
                        status = Some(message);
                    }
                    Loaded::Buildings(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            state.update(Action::MapBuildings(termap::data::Tile::new(
                                termap::data::parse_features(&text),
                            )));
                        }
                    }
                }
                dirty = true;
            }
            let now = Instant::now();
            if state.animating() {
                state.update(Action::Tick(
                    now.duration_since(last).as_secs_f64().min(10.0),
                ));
                dirty = true;
            }
            last = now;
            if dirty {
                let mut surface = state.cells();
                if let Some(message) = &status {
                    overlay_status(&mut surface, message);
                }
                offer(&frames, Some(surface));
                dirty = false;
                if let Some(demand) = state.map_demand() {
                    if demand.tiles != wanted {
                        wanted = demand.tiles;
                        offer(&requests, wanted.clone());
                    }
                }
            }
            if event::poll(if state.animating() {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(100)
            })? {
                match event::read()? {
                    Event::Resize(cols, rows) => {
                        resize(&mut state, (cols, rows));
                        dirty = true;
                    }
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if (key.code == KeyCode::Char('q')
                            && state.section != portfolio_v2_client_core::Section::Ask)
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL))
                        {
                            break;
                        }
                        if state.section == portfolio_v2_client_core::Section::Ask {
                            if key.code == KeyCode::Char('x')
                                && key.modifiers.contains(KeyModifiers::CONTROL)
                            {
                                offer(&ask_commands, session::Command::Cancel);
                                state.ask_status("cancelling…");
                                dirty = true;
                                continue;
                            }
                            if key.code == KeyCode::Enter {
                                if state.ask_question().trim() == "/new" {
                                    pending_question = None;
                                    state.edit_question("");
                                    offer(&ask_commands, session::Command::New);
                                    dirty = true;
                                    continue;
                                }
                                let question = state.ask_question();
                                if !question.trim().is_empty() {
                                    let command = pending_question.get_or_insert_with(|| {
                                        portfolio_v2_protocol::session::Submit {
                                            request_id: uuid::Uuid::new_v4().simple().to_string(),
                                            question,
                                        }
                                    });
                                    state.mark_submitted(&command.request_id);
                                    offer(&ask_commands, session::Command::Submit(command.clone()));
                                    state.ask_status("sending…");
                                    dirty = true;
                                }
                                continue;
                            }
                        }
                        let name = match key.code {
                            KeyCode::Char(c) => c.to_string(),
                            KeyCode::Enter => "Enter".into(),
                            KeyCode::Esc => "Escape".into(),
                            KeyCode::Left => "ArrowLeft".into(),
                            KeyCode::Right => "ArrowRight".into(),
                            KeyCode::Up => "ArrowUp".into(),
                            KeyCode::Down => "ArrowDown".into(),
                            KeyCode::Backspace => "Backspace".into(),
                            KeyCode::PageDown => "PageDown".into(),
                            KeyCode::PageUp => "PageUp".into(),
                            KeyCode::Tab => "Tab".into(),
                            _ => String::new(),
                        };
                        dirty |= state.key(&name);
                        if name == "6" && state.section == portfolio_v2_client_core::Section::Ask {
                            offer(&ask_commands, session::Command::Open);
                        }
                    }
                    Event::Mouse(mouse) => {
                        if matches!(mouse.kind, MouseEventKind::Down(_))
                            && state.click(mouse.column, mouse.row)
                        {
                            dirty = true;
                            continue;
                        }
                        if matches!(
                            state.section,
                            portfolio_v2_client_core::Section::Skills
                                | portfolio_v2_client_core::Section::Experience
                        ) {
                            dirty |= state.pointer(
                                mouse.column as f64,
                                mouse.row as f64,
                                0.0,
                                0.0,
                                false,
                            );
                        }
                        if state.section == portfolio_v2_client_core::Section::Skills {
                            let point = (mouse.column as f64, mouse.row as f64);
                            match mouse.kind {
                                MouseEventKind::Down(_) => skill_drag = Some(point),
                                MouseEventKind::Drag(_) => {
                                    if let Some(from) = skill_drag.replace(point) {
                                        dirty |= state.pointer(
                                            point.0,
                                            point.1,
                                            point.0 - from.0,
                                            point.1 - from.1,
                                            true,
                                        );
                                    }
                                }
                                MouseEventKind::Up(_) => {
                                    skill_drag = None;
                                    state.release_pointer();
                                    dirty = true;
                                }
                                _ => (),
                            }
                        }
                        if state.section != portfolio_v2_client_core::Section::Experience {
                            match mouse.kind {
                                MouseEventKind::ScrollUp => {
                                    state.scroll(-3);
                                    dirty = true;
                                }
                                MouseEventKind::ScrollDown => {
                                    state.scroll(3);
                                    dirty = true;
                                }
                                _ => (),
                            }
                        }
                        let point = state.map_point(mouse.column as f64, mouse.row as f64);
                        let command = match (mouse.kind, point) {
                            (MouseEventKind::Down(_), Some(point)) => {
                                drag = Some(point);
                                None
                            }
                            (MouseEventKind::Up(_), _) => {
                                drag = None;
                                None
                            }
                            (MouseEventKind::Drag(_), Some(to)) => {
                                let from = drag.replace(to);
                                from.map(|from| MapCommand::Drag(from, to))
                            }
                            (MouseEventKind::ScrollUp, Some(point)) => {
                                Some(MapCommand::ZoomAt(0.3, point))
                            }
                            (MouseEventKind::ScrollDown, Some(point)) => {
                                Some(MapCommand::ZoomAt(-0.3, point))
                            }
                            _ => None,
                        };
                        if let Some(command) = command {
                            state.update(Action::MapCommand(command));
                            dirty = true;
                        }
                    }
                    _ => (),
                }
            }
            if writer.is_finished() {
                return Err(io::Error::other("terminal output closed"));
            }
        }
        Ok(())
    })();
    offer(&frames, None);
    let output_result = writer.join().map_err(|_| "terminal writer failed")?;
    result?;
    output_result?;
    Ok(())
}
fn searcher(
    endpoint: String,
    queries: Mailbox<String>,
    results: mpsc::SyncSender<(String, Vec<portfolio_v2_protocol::map::SearchResult>)>,
) {
    loop {
        let mut query = take(&queries);
        thread::sleep(Duration::from_millis(50));
        if let Some(next) = queries.0.lock().unwrap().take() {
            query = next;
        }
        let encoded = query
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>();
        if let Ok(bytes) = fetch(
            &endpoint,
            &format!("/api/v2/geocode?q={encoded}"),
            128 * 1024,
        ) {
            if let Ok(values) = serde_json::from_slice(&bytes) {
                if results.send((query, values)).is_err() {
                    return;
                }
            }
        }
    }
}

fn replay(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    let fixture: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let bootstrap =
        portfolio_v2_protocol::decode_bootstrap(&serde_json::to_vec(&fixture["bootstrap"])?)
            .map_err(|e| format!("{e:?}"))?;
    let mut state = ClientState::default();
    state.update(Action::BootstrapLoaded(bootstrap));
    let mut hashes = Vec::new();
    for action in fixture["actions"].as_array().ok_or("missing actions")? {
        match action["type"].as_str().unwrap_or("") {
            "resize" => {
                let w = action["width"].as_f64().ok_or("width")? as f32;
                let h = action["height"].as_f64().ok_or("height")? as f32;
                state.update(Action::Resize(Viewport {
                    width: w,
                    height: h,
                    scale: 1.0,
                    cols: ((w / 8.0).floor() as u16).clamp(20, 180),
                    rows: ((h / 17.0).floor() as u16).clamp(6, 60),
                }));
            }
            "key" => {
                state.key(action["key"].as_str().unwrap_or(""));
            }
            "tick" => state.update(Action::Tick(action["seconds"].as_f64().unwrap_or(0.0))),
            "motion" => state.update(Action::SetReducedMotion(
                action["reduced"].as_bool().unwrap_or(false),
            )),
            "frame" => hashes.push(format!("{:x}", Sha256::digest(state.cells().packed()))),
            _ => return Err("unsupported replay action".into()),
        }
    }
    println!("{}", serde_json::to_string(&hashes)?);
    Ok(())
}

fn resize(state: &mut ClientState, (cols, rows): (u16, u16)) {
    let (cols, rows) = (cols.clamp(1, 240), rows.clamp(1, 100));
    state.update(Action::Resize(Viewport {
        width: cols as f32 * 8.0,
        height: rows as f32 * 17.0,
        scale: 1.0,
        cols,
        rows,
    }));
}

fn overlay_status(surface: &mut CellSurface, message: &str) {
    let start = surface.rows.saturating_sub(1) as usize * surface.cols as usize;
    for (cell, glyph) in surface.cells[start..]
        .iter_mut()
        .zip(message.chars().chain(std::iter::repeat(' ')))
    {
        cell.glyph = glyph;
    }
}
