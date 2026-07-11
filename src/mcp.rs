use rmcp::{
    ErrorData, ServerHandler,
    handler::server::tool::IntoCallToolResult,
    handler::server::wrapper::{Json, Parameters},
    model::CallToolResult,
    tool, tool_handler, tool_router,
};

use crate::{DoctorInput, SearchService, ToolResponse, WebFetchInput, WebSearchInput};

#[derive(Debug, Clone)]
pub struct GrokMcpServer {
    service: SearchService,
}

#[derive(Debug)]
pub struct ToolFailure(Box<ToolResponse>);

impl ToolFailure {
    pub fn response(&self) -> &ToolResponse {
        &self.0
    }
}

impl IntoCallToolResult for ToolFailure {
    fn into_call_tool_result(self) -> Result<CallToolResult, ErrorData> {
        let value = serde_json::to_value(*self.0).map_err(|error| {
            ErrorData::internal_error(
                format!("failed to serialize structured tool error: {error}"),
                None,
            )
        })?;
        Ok(CallToolResult::structured_error(value))
    }
}

impl GrokMcpServer {
    pub fn new(service: SearchService) -> Self {
        Self { service }
    }
}

#[tool_router(router = tool_router, vis = "pub")]
impl GrokMcpServer {
    #[tool(
        description = "Search the public web through the locally installed Grok Build CLI. Returns a verified answer and exact HTTP(S) source URLs."
    )]
    pub async fn web_search(
        &self,
        Parameters(input): Parameters<WebSearchInput>,
    ) -> Result<Json<ToolResponse>, ToolFailure> {
        into_tool_result(self.service.web_search(input).await)
    }

    #[tool(
        description = "Fetch one known public HTTP(S) URL through the locally installed Grok Build CLI. Private and reserved network targets are rejected."
    )]
    pub async fn web_fetch(
        &self,
        Parameters(input): Parameters<WebFetchInput>,
    ) -> Result<Json<ToolResponse>, ToolFailure> {
        into_tool_result(self.service.web_fetch(input).await)
    }

    #[tool(
        description = "Check the local Grok Build CLI installation. Set live_search=true only when a real search probe is required."
    )]
    pub async fn doctor(
        &self,
        Parameters(input): Parameters<DoctorInput>,
    ) -> Result<Json<ToolResponse>, ToolFailure> {
        into_tool_result(self.service.doctor(input).await)
    }
}

#[tool_handler(
    router = Self::tool_router(),
    name = "grok-build-search",
    instructions = "Use web_search for discovery, web_fetch for a known public URL, and doctor for local diagnostics."
)]
impl ServerHandler for GrokMcpServer {}

fn into_tool_result(
    result: Result<ToolResponse, crate::ToolError>,
) -> Result<Json<ToolResponse>, ToolFailure> {
    result
        .map(Json)
        .map_err(|error| ToolFailure(Box::new(ToolResponse::failure(error))))
}
