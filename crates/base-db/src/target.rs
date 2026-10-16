//! Information about the target.

use std::fmt;

use triomphe::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Arch {
    // Only what we need is present here.
    Wasm32,
    Wasm64,
    Other,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, serde::Serialize, serde::Deserialize)]
pub struct TargetData {
    pub data_layout: Box<str>,
    pub arch: Arch,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct TargetLoadError(Arc<str>);

impl fmt::Debug for TargetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for TargetLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for TargetLoadError {}

impl From<String> for TargetLoadError {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}

impl From<&str> for TargetLoadError {
    fn from(value: &str) -> Self {
        Self(value.into())
    }
}

pub type TargetLoadResult = Result<TargetData, TargetLoadError>;

impl serde::Serialize for TargetLoadError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for TargetLoadError {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(String::deserialize(deserializer)?.into())
    }
}
