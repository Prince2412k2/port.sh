use serde::{Deserialize, Serialize};
pub const MAX_QUESTION_BYTES: usize = 4096;
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestStatus {
    Running,
    Completed,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    #[serde(default)]
    pub created_at: u64,
    pub request_id: String,
    pub question: String,
    pub answer: String,
    pub status: RequestStatus,
    pub error: Option<String>,
    #[serde(default)]
    pub presentations: Vec<Presentation>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Presentation {
    Map {
        title: String,
        lon: f64,
        lat: f64,
        zoom: f64,
    },
    Project {
        id: String,
    },
    Diagram {
        title: String,
        nodes: Vec<DiagramNode>,
        edges: Vec<DiagramEdge>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramNode {
    pub id: String,
    pub label: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}
impl Presentation {
    pub fn valid(&self) -> bool {
        match self {
            Self::Map {
                title,
                lon,
                lat,
                zoom,
            } => {
                title.len() <= 256
                    && lon.is_finite()
                    && lat.is_finite()
                    && zoom.is_finite()
                    && (-180.0..=180.0).contains(lon)
                    && (-85.0..=85.0).contains(lat)
                    && (4.0..=18.0).contains(zoom)
            }
            Self::Project { id } => id.len() <= 128,
            Self::Diagram {
                title,
                nodes,
                edges,
            } => {
                title.len() <= 128
                    && !nodes.is_empty()
                    && nodes.len() <= 8
                    && edges.len() <= 16
                    && nodes
                        .iter()
                        .all(|n| !n.id.is_empty() && n.id.len() <= 32 && n.label.len() <= 64)
                    && edges.iter().all(|e| {
                        e.label.len() <= 64
                            && nodes.iter().any(|n| n.id == e.from)
                            && nodes.iter().any(|n| n.id == e.to)
                    })
                    && nodes
                        .iter()
                        .map(|n| &n.id)
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        == nodes.len()
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub protocol: u16,
    pub session_id: String,
    pub sequence: u64,
    pub exchanges: Vec<Exchange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Submit {
    pub request_id: String,
    pub question: String,
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
impl Snapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol != super::VERSION
            || !valid_id(&self.session_id)
            || self.exchanges.len() > 32
        {
            return Err("incompatible or oversized session");
        }
        for e in &self.exchanges {
            if !valid_id(&e.request_id)
                || e.question.len() > MAX_QUESTION_BYTES
                || e.answer.len() > MAX_ANSWER_BYTES
            {
                return Err("oversized exchange");
            }
            if e.presentations.len() > 8 {
                return Err("too many presentations");
            }
            if e.presentations.iter().any(|p| !p.valid())
                || serde_json::to_vec(&e.presentations)
                    .map_err(|_| "invalid presentation")?
                    .len()
                    > 4096
            {
                return Err("invalid or oversized presentation");
            }
        }
        Ok(())
    }
}
