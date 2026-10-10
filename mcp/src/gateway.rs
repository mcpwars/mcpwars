//! stdio <-> Streamable HTTP gateway: lets stdio-only MCP clients join the
//! shared multiplayer universe as the player owning `token`.

use rmcp::model::*;
use rmcp::service::{Peer, RequestContext, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{ErrorData as McpError, RoleClient, RoleServer, ServerHandler, ServiceExt};

pub struct Gateway {
    peer: Peer<RoleClient>,
    info: ServerConfig,
    _client: std::sync::Arc<RunningService<RoleClient, ()>>,
}

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(format!("upstream: {e}"), None)
}

impl Gateway {
    pub async fn connect(server: &str, token: &str) -> anyhow::Result<Self> {
        let url = format!("{}/mcp/{}", server.trim_end_matches('/'), token);
        let transport = StreamableHttpClientTransport::from_uri(url);
        let client = ().serve(transport).await?;
        let peer = client.peer().clone();
        let info = peer
            .peer_info()
            .map(|i| {
                let mut s = ServerConfig::new(i.capabilities.clone());
                s.instructions = i.instructions.clone();
                if let Some(si) = i.server_info.clone() {
                    s.server_info = si;
                }
                s
            })
            .unwrap_or_else(|| {
                ServerConfig::new(
                    ServerCapabilities::builder()
                        .enable_tools()
                        .enable_resources()
                        .enable_prompts()
                        .build(),
                )
            });
        Ok(Gateway {
            peer,
            info,
            _client: std::sync::Arc::new(client),
        })
    }
}

impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerConfig {
        self.info.clone()
    }
    async fn list_tools(&self, r: Option<PaginatedRequestParams>, _c: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError> {
        self.peer.list_tools(r).await.map_err(err)
    }
    async fn call_tool(&self, req: CallToolRequestParams, _c: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError> {
        self.peer.call_tool_once(req).await.map_err(err)
    }
    async fn list_resources(
        &self,
        r: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        self.peer.list_resources(r).await.map_err(err)
    }
    async fn list_resource_templates(
        &self,
        r: Option<PaginatedRequestParams>,
        _c: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        self.peer.list_resource_templates(r).await.map_err(err)
    }
    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _c: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        self.peer.read_resource_once(req).await.map_err(err)
    }
    async fn list_prompts(&self, r: Option<PaginatedRequestParams>, _c: RequestContext<RoleServer>) -> Result<ListPromptsResult, McpError> {
        self.peer.list_prompts(r).await.map_err(err)
    }
    async fn get_prompt(&self, req: GetPromptRequestParams, _c: RequestContext<RoleServer>) -> Result<GetPromptResponse, McpError> {
        self.peer.get_prompt_once(req).await.map_err(err)
    }
}
