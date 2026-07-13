#![forbid(unsafe_code)]

//! Core library for the Grok Build Search MCP server.

mod error;
mod grok;
mod mcp;
mod model;
mod runtime;
mod service;
mod url_guard;

pub use error::{ErrorCode, ToolError};
pub use grok::{GrokClient, GrokConfig, GrokLocator};
pub use mcp::GrokMcpServer;
pub use model::{
    DoctorInput, ResponseFormat, Source, ToolResponse, ToolWarning, ValidatedWebFetch,
    ValidatedWebSearch, WarningCode, WebFetchInput, WebSearchInput, parse_grok_json,
};
pub use service::SearchService;
pub use url_guard::{validate_public_url, validate_url_with_resolved_ips};
