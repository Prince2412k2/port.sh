use serde::{Deserialize, Serialize};

pub const VERSION: u16 = 2;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;
#[path = "../../../../skills/src/data.rs"]
#[allow(unexpected_cfgs)]
mod project_data;
pub use project_data::parse as parse_projects;
pub use project_data::{Beat, Project};
#[path = "../../../../map/src/geo.rs"]
#[allow(dead_code)]
mod geo;
pub mod map;
#[path = "../../../../map/src/place.rs"]
#[allow(unexpected_cfgs, dead_code)]
mod place_data;
pub mod session;
pub mod taste;
pub use place_data::{parse as parse_places, Place};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub protocol: u16,
    pub revision: String,
    pub profile: Profile,
    pub navigation: Vec<NavigationItem>,
    #[serde(default)]
    pub projects: Vec<Project>,
    #[serde(default)]
    pub taste: taste::Sheet,
    #[serde(default)]
    pub places: Vec<Place>,
}

impl Bootstrap {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol != VERSION {
            return Err("unsupported protocol version");
        }
        if self.revision.is_empty() || self.profile.name.is_empty() {
            return Err("bootstrap is missing identity fields");
        }
        if self.navigation.is_empty() {
            return Err("bootstrap has no navigation");
        }
        if self.navigation.len() > 16
            || self.projects.len() > 64
            || self.profile.contacts.len() > 16
            || serde_json::to_vec(self)
                .map_err(|_| "invalid bootstrap")?
                .len()
                > MAX_MESSAGE_BYTES
        {
            return Err("bootstrap exceeds collection or byte bounds");
        }
        if self.places.len() > 64
            || self.places.iter().any(|p| {
                !p.lonlat.0.is_finite()
                    || !p.lonlat.1.is_finite()
                    || !p.zoom.is_finite()
                    || !p.tilt.is_finite()
                    || !p.bearing.is_finite()
                    || !(-180.0..=180.0).contains(&p.lonlat.0)
                    || !(-85.0..=85.0).contains(&p.lonlat.1)
                    || !(2.0..=20.0).contains(&p.zoom)
                    || (0.0..=1.2).contains(&p.tilt) == false
            })
        {
            return Err("invalid experience locations");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub role: String,
    pub location: String,
    pub handle: String,
    pub pitch: String,
    pub now: String,
    pub contacts: Vec<Contact>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub id: String,
    pub label: String,
    pub value: String,
    pub href: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NavigationItem {
    pub id: String,
    pub label: String,
    pub available: bool,
}

pub fn decode_bootstrap(input: &[u8]) -> Result<Bootstrap, DecodeError> {
    if input.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let value: Bootstrap = serde_json::from_slice(input).map_err(|_| DecodeError::InvalidJson)?;
    value.validate().map_err(DecodeError::InvalidMessage)?;
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooLarge,
    InvalidJson,
    InvalidMessage(&'static str),
}
