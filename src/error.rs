use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    GrokNotFound,
    GrokUnsupportedVersion,
    GrokTimeout,
    GrokExitFailed,
    BadGrokJson,
    NoSources,
    InvalidUrl,
    PrivateUrl,
    InvalidQuery,
    InvalidInstructions,
    InvalidMaxChars,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolError {
    pub code: ErrorCode,
    pub message: String,
}

impl ToolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for ToolError {}
