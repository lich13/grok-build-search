use crate::{
    DoctorInput, GrokClient, ResponseFormat, ToolError, ToolResponse, WebFetchInput,
    WebSearchInput, validate_public_url,
};

#[derive(Debug, Clone)]
pub struct SearchService {
    client: Result<GrokClient, ToolError>,
}

impl SearchService {
    pub fn new(client: GrokClient) -> Self {
        Self { client: Ok(client) }
    }

    pub fn unavailable(error: ToolError) -> Self {
        Self { client: Err(error) }
    }

    pub async fn web_search(&self, input: WebSearchInput) -> Result<ToolResponse, ToolError> {
        let cleanup_deferred = self.client()?.cleanup_stale_runtimes();
        let validated = input.validate()?;
        let mut response = self
            .client()?
            .search(&validated.query, validated.response_format)
            .await?;
        if cleanup_deferred {
            response.add_cleanup_deferred_warning();
        }
        Ok(response)
    }

    pub async fn web_fetch(&self, input: WebFetchInput) -> Result<ToolResponse, ToolError> {
        let cleanup_deferred = self.client()?.cleanup_stale_runtimes();
        let validated = input.validate()?;
        let url = validate_public_url(&validated.url).await?;
        let mut response = self
            .client()?
            .fetch(
                url.as_str(),
                validated.instructions.as_deref(),
                validated.max_chars,
            )
            .await?;
        if cleanup_deferred {
            response.add_cleanup_deferred_warning();
        }
        Ok(response)
    }

    pub async fn doctor(&self, input: DoctorInput) -> Result<ToolResponse, ToolError> {
        let client = self.client()?;
        let cleanup_deferred = client.cleanup_stale_runtimes();
        let version = client.probe_version().await?;
        if input.live_search {
            let mut response = client
                .search(
                    "Find the official Rust programming language website and cite its public URL.",
                    ResponseFormat::Concise,
                )
                .await?;
            if cleanup_deferred {
                response.add_cleanup_deferred_warning();
            }
            return Ok(response);
        }

        let mut response = ToolResponse {
            ok: true,
            verified: true,
            answer: format!(
                "Grok CLI {version} is installed and supported. Live search was not requested."
            ),
            sources: Vec::new(),
            backend: "grok-build-cli".to_string(),
            session_id: None,
            stop_reason: None,
            truncated: false,
            warnings: Vec::new(),
            error: None,
        };
        if cleanup_deferred {
            response.add_cleanup_deferred_warning();
        }
        Ok(response)
    }

    fn client(&self) -> Result<&GrokClient, ToolError> {
        self.client.as_ref().map_err(Clone::clone)
    }
}
