use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckProfile(String);

impl CheckProfile {
    pub fn new(value: impl Into<String>) -> Result<Self, PluginValueError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(PluginValueError::EmptyCheckProfile);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishPolicy {
    Never,
    OnValidationSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginValueError {
    EmptyCheckProfile,
}

impl Display for PluginValueError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCheckProfile => write!(f, "check profile must not be empty"),
        }
    }
}

impl std::error::Error for PluginValueError {}
