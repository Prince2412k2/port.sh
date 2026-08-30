use serde::{Deserialize, Serialize};

pub const VERSION: u16 = 2;
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub protocol: u16,
    pub revision: String,
    pub profile: Profile,
    pub navigation: Vec<NavigationItem>,
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
