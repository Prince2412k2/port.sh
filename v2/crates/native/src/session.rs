use super::*;
use portfolio_v2_protocol::session::{valid_id, RequestStatus, Snapshot, Submit};
#[derive(Clone)]
pub enum Command {
    Open,
    New,
    Submit(Submit),
    Cancel,
}
pub enum Update {
    Snapshot(Snapshot),
    Reset(Snapshot),
    Status(String),
}
fn request(
    endpoint: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<Snapshot, String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = if let Some(body) = body {
        agent.post(format!("{endpoint}{path}")).send_json(body)
    } else {
        agent.get(format!("{endpoint}{path}")).call()
    }
    .map_err(|_| "Session connection failed; Enter retries the same request.".to_string())?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(3 * 1024 * 1024)
        .read_to_vec()
        .map_err(|_| "Session response unavailable".to_string())?;
    let snapshot: Snapshot =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid session response".to_string())?;
    snapshot.validate().map_err(str::to_owned)?;
    Ok(snapshot)
}
pub fn run(
    endpoint: String,
    commands: Mailbox<Command>,
    updates: mpsc::SyncSender<Update>,
    resume: Option<String>,
) {
    use sha2::{Digest, Sha256};
    let name = format!("{:x}", Sha256::digest(endpoint.as_bytes()));
    let directory = env::var_os("PORTFOLIO_V2_STATE_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            env::var_os("HOME")
                .map(|p| std::path::PathBuf::from(p).join(".local/state/portfolio-v2"))
        });
    let path = directory
        .filter(|_| env::var_os("PORTFOLIO_V2_EPHEMERAL").is_none())
        .map(|p| p.join(format!("{name}.session")));
    let mut id = resume.or_else(|| {
        env::var("PORTFOLIO_V2_SESSION")
            .ok()
            .or_else(|| path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()))
            .filter(|id| valid_id(id))
    });
    let mut snapshot: Option<Snapshot> = None;
    let mut streaming = false;
    let stream_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut command = take(&commands);
    loop {
        let new_conversation = matches!(command, Command::New);
        if new_conversation {
            snapshot = None;
            id = None;
            streaming = false;
            stream_generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let result = (|| -> Result<Snapshot, String> {
            if snapshot.is_none() {
                snapshot = Some(if let Some(id) = &id {
                    request(&endpoint, &format!("/api/v2/sessions/{id}"), None)?
                } else {
                    request(&endpoint, "/api/v2/sessions", Some(serde_json::json!({})))?
                });
                id = Some(snapshot.as_ref().unwrap().session_id.clone());
                if !streaming {
                    streaming = true;
                    let endpoint = endpoint.clone();
                    let session = id.clone().unwrap();
                    let updates = updates.clone();
                    let generation = stream_generation.load(std::sync::atomic::Ordering::Relaxed);
                    let guard = stream_generation.clone();
                    thread::spawn(move || stream(endpoint, session, updates, guard, generation));
                }
                if let (Some(path), Some(id)) = (&path, &id) {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let mut options = std::fs::OpenOptions::new();
                    options.create(true).truncate(true).write(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        options.mode(0o600);
                    }
                    if let Ok(mut file) = options.open(path) {
                        let _ = file.write_all(id.as_bytes());
                    }
                }
            }
            let id = id.as_ref().unwrap();
            match &command {
                Command::Open | Command::New => {
                    request(&endpoint, &format!("/api/v2/sessions/{id}"), None)
                }
                Command::Submit(command) => request(
                    &endpoint,
                    &format!("/api/v2/sessions/{id}/requests"),
                    Some(serde_json::to_value(command).unwrap()),
                ),
                Command::Cancel => {
                    let Some(exchange) = snapshot
                        .as_ref()
                        .unwrap()
                        .exchanges
                        .iter()
                        .find(|e| e.status == RequestStatus::Running)
                    else {
                        return Ok(snapshot.clone().unwrap());
                    };
                    request(
                        &endpoint,
                        &format!(
                            "/api/v2/sessions/{id}/requests/{}/cancel",
                            exchange.request_id
                        ),
                        Some(serde_json::json!({})),
                    )
                }
            }
        })();
        match result {
            Ok(value) => {
                let running = value
                    .exchanges
                    .iter()
                    .any(|e| e.status == RequestStatus::Running);
                snapshot = Some(value.clone());
                if updates
                    .send(if new_conversation {
                        Update::Reset(value)
                    } else {
                        Update::Snapshot(value)
                    })
                    .is_err()
                {
                    return;
                }
                let _ = running;
                command = take(&commands);
            }
            Err(message) => {
                if updates.send(Update::Status(message)).is_err() {
                    return;
                }
                command = take(&commands);
            }
        }
    }
}
fn stream(
    endpoint: String,
    id: String,
    updates: mpsc::SyncSender<Update>,
    guard: Arc<std::sync::atomic::AtomicU64>,
    generation: u64,
) {
    let url = format!(
        "{}/api/v2/session?session={id}",
        endpoint
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1)
    );
    loop {
        if guard.load(std::sync::atomic::Ordering::Relaxed) != generation {
            return;
        }
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(3 * 1024 * 1024))
            .max_frame_size(Some(3 * 1024 * 1024));
        if let Ok((mut socket, _)) =
            tungstenite::client::connect_with_config(url.as_str(), Some(config), 3)
        {
            if updates.send(Update::Status(String::new())).is_err() {
                return;
            }
            loop {
                if guard.load(std::sync::atomic::Ordering::Relaxed) != generation {
                    return;
                }
                match socket.read() {
                    Ok(tungstenite::Message::Binary(bytes)) => {
                        if guard.load(std::sync::atomic::Ordering::Relaxed) != generation {
                            return;
                        }
                        let Ok(snapshot) = ciborium::from_reader::<Snapshot, _>(&bytes[..]) else {
                            break;
                        };
                        if snapshot.validate().is_err() {
                            break;
                        }
                        if updates.send(Update::Snapshot(snapshot)).is_err() {
                            return;
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                    _ => (),
                }
            }
        }
        if guard.load(std::sync::atomic::Ordering::Relaxed) != generation {
            return;
        }
        if updates
            .send(Update::Status(
                "Disconnected; reconnecting to the saved conversation…".into(),
            ))
            .is_err()
        {
            return;
        }
        thread::sleep(Duration::from_secs(2));
    }
}
