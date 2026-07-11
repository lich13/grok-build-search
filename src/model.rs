use std::collections::HashSet;

use linkify::{LinkFinder, LinkKind};
use pulldown_cmark::{Event, Parser, Tag};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::url_guard::is_safe_source_url;
use crate::{ErrorCode, ToolError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ResponseFormat {
    #[default]
    Concise,
    Detailed,
}

impl ResponseFormat {
    pub const fn max_chars(self) -> usize {
        match self {
            Self::Concise => 12_000,
            Self::Detailed => 40_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct WebSearchInput {
    #[schemars(length(min = 1, max = 8_000))]
    pub query: String,
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedWebSearch {
    pub query: String,
    pub response_format: ResponseFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct WebFetchInput {
    pub url: String,
    #[schemars(length(max = 8_000))]
    pub instructions: Option<String>,
    #[schemars(range(min = 1_000, max = 60_000))]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedWebFetch {
    pub url: String,
    pub instructions: Option<String>,
    pub max_chars: usize,
}

impl WebFetchInput {
    pub fn validate(self) -> Result<ValidatedWebFetch, ToolError> {
        let url = self.url.trim();
        if url.is_empty() {
            return Err(ToolError::new(
                ErrorCode::InvalidUrl,
                "url must not be empty",
            ));
        }
        let instructions = self
            .instructions
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if instructions
            .as_ref()
            .is_some_and(|value| value.chars().count() > 8_000)
        {
            return Err(ToolError::new(
                ErrorCode::InvalidInstructions,
                "instructions must not exceed 8000 characters",
            ));
        }
        let max_chars = self.max_chars.unwrap_or(20_000);
        if !(1_000..=60_000).contains(&max_chars) {
            return Err(ToolError::new(
                ErrorCode::InvalidMaxChars,
                "max_chars must be between 1000 and 60000",
            ));
        }

        Ok(ValidatedWebFetch {
            url: url.to_string(),
            instructions,
            max_chars,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DoctorInput {
    #[serde(default)]
    #[schemars(default)]
    pub live_search: bool,
}

impl WebSearchInput {
    pub fn validate(self) -> Result<ValidatedWebSearch, ToolError> {
        let query = self.query.trim();
        let query_chars = query.chars().count();
        if query_chars == 0 || query_chars > 8_000 {
            return Err(ToolError::new(
                ErrorCode::InvalidQuery,
                "query must contain between 1 and 8000 characters",
            ));
        }

        Ok(ValidatedWebSearch {
            query: query.to_string(),
            response_format: self.response_format.unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Source {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ToolResponse {
    pub ok: bool,
    pub verified: bool,
    pub answer: String,
    pub sources: Vec<Source>,
    pub backend: String,
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::ToolError>,
}

impl ToolResponse {
    pub fn failure(error: ToolError) -> Self {
        Self {
            ok: false,
            verified: false,
            answer: String::new(),
            sources: Vec::new(),
            backend: "grok-build-cli".to_string(),
            session_id: None,
            stop_reason: None,
            truncated: false,
            error: Some(error),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GrokJson {
    text: String,
    stop_reason: Option<String>,
    session_id: Option<String>,
}

pub fn parse_grok_json(
    raw: &str,
    response_format: ResponseFormat,
) -> Result<ToolResponse, ToolError> {
    parse_grok_json_with_limit(raw, response_format.max_chars(), None)
}

pub(crate) fn parse_grok_fetch_json(
    raw: &str,
    max_chars: usize,
    fetched_url: &str,
) -> Result<ToolResponse, ToolError> {
    parse_grok_json_with_limit(raw, max_chars, Some(fetched_url))
}

fn parse_grok_json_with_limit(
    raw: &str,
    max_chars: usize,
    required_source: Option<&str>,
) -> Result<ToolResponse, ToolError> {
    let parsed: GrokJson = serde_json::from_str(raw).map_err(|error| {
        ToolError::new(
            ErrorCode::BadGrokJson,
            format!("could not parse Grok JSON output: {error}"),
        )
    })?;
    if parsed.text.trim().is_empty() {
        return Err(ToolError::new(
            ErrorCode::BadGrokJson,
            "Grok JSON output contains an empty text field",
        ));
    }

    let mut sources = extract_sources(&parsed.text);
    if let Some(required_source) = required_source {
        sources.retain(|source| source.url != required_source);
        sources.insert(
            0,
            Source {
                url: required_source.to_string(),
            },
        );
    }
    if sources.is_empty() {
        return Err(ToolError::new(
            ErrorCode::NoSources,
            "Grok returned an answer without public HTTP(S) sources",
        ));
    }

    let original_chars = parsed.text.chars().count();
    let truncated = original_chars > max_chars;
    let answer = if truncated {
        parsed.text.chars().take(max_chars).collect()
    } else {
        parsed.text
    };

    Ok(ToolResponse {
        ok: true,
        verified: true,
        answer,
        sources,
        backend: "grok-build-cli".to_string(),
        session_id: parsed.session_id,
        stop_reason: parsed.stop_reason,
        truncated,
        error: None,
    })
}

fn extract_sources(text: &str) -> Vec<Source> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    let mut seen = HashSet::new();
    let mut sources = Vec::new();

    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Link { dest_url, .. }) => {
                push_source(dest_url.as_ref(), &mut seen, &mut sources);
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                for link in finder.links(text.as_ref()) {
                    push_source(link.as_str(), &mut seen, &mut sources);
                }
            }
            _ => {}
        }
    }

    sources
}

fn push_source(candidate: &str, seen: &mut HashSet<String>, sources: &mut Vec<Source>) {
    let Ok(parsed) = Url::parse(candidate) else {
        return;
    };
    if !is_safe_source_url(&parsed) {
        return;
    }
    let normalized = parsed.to_string();
    if seen.insert(normalized.clone()) {
        sources.push(Source { url: normalized });
    }
}
