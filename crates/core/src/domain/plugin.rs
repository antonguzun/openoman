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

impl PublishPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::OnValidationSuccess => "on_validation_success",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PluginValueError> {
        match value {
            "never" => Ok(Self::Never),
            "on_validation_success" => Ok(Self::OnValidationSuccess),
            _ => Err(PluginValueError::InvalidPublishPolicy(value.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginValueError {
    EmptyCheckProfile,
    InvalidPublishPolicy(String),
}

impl Display for PluginValueError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCheckProfile => write!(f, "check profile must not be empty"),
            Self::InvalidPublishPolicy(value) => {
                write!(f, "invalid publish policy: {value}")
            }
        }
    }
}

impl std::error::Error for PluginValueError {}
