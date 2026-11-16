use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::json;
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::metadata::MetadataProvider;
use crate::{AuthorizedCall, Authorizer, Error, Store, TokenIdentity, Tool};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub metadata: Arc<dyn MetadataProvider>,
    /// MCP session id -> token id. Prevents a session from switching identity.
    pub sessions: Arc<Mutex<HashMap<String, String>>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SourceArgs {
    #[schemars(description = "Source id; may be omitted when the token has one source")]
    source: Option<String>,
    #[schemars(description = "Optional schema name")]
    schema: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DescribeArgs {
    #[schemars(description = "Source id; may be omitted when the token has one source")]
    source: Option<String>,
    table: String,
    #[schemars(description = "Optional schema name")]
    schema: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExplainArgs {
    #[schemars(description = "Source id; may be omitted when the token has one source")]
    source: Option<String>,
    sql: String,
    #[serde(default)]
    params: Vec<Value>,
}

#[derive(Clone)]
pub struct McpDbServer {
    state: AppState,
}

impl McpDbServer {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    fn lock_store(&self) -> Result<std::sync::MutexGuard<'_, Store>, McpError> {
        self.state
            .store
            .lock()
            .map_err(|_| McpError::internal_error("state store lock poisoned", None))
    }

    fn authorize_call(
        &self,
        identity: &TokenIdentity,
        source: Option<&str>,
        tool: Tool,
    ) -> std::result::Result<AuthorizedCall, CallToolResult> {
        let store = match self.state.store.lock() {
            Ok(store) => store,
            Err(_) => {
                return Err(CallToolResult::structured_error(json!({
                    "code": "STATE_UNAVAILABLE",
                    "message": "state store lock poisoned",
                })))
            }
        };
        Authorizer::new(&store)
            .authorize(&identity.token_id, source, tool)
            .map_err(tool_error)
    }
}

#[tool_router]
impl McpDbServer {
    #[tool(description = "List the database sources this token can access.")]
    async fn list_sources(
        &self,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let sources = {
            let store = self.lock_store()?;
            Authorizer::new(&store)
                .visible_sources(&identity.token_id)
                .map_err(store_error)?
        };
        let sources = sources
            .into_iter()
            .map(|source| {
                json!({
                    "id": source.id,
                    "driver": source.driver.as_str(),
                    "environment": source.environment.as_str(),
                })
            })
            .collect::<Vec<_>>();
        Ok(CallToolResult::structured(json!({ "sources": sources })))
    }

    #[tool(description = "List tables and views in a source.")]
    async fn list_tables(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<SourceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::ListTables) {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        let tables = match self
            .state
            .metadata
            .list_tables(&call.profile, args.schema.as_deref())
            .await
        {
            Ok(tables) => tables,
            Err(error) => return Ok(tool_error(error)),
        };
        Ok(CallToolResult::structured(json!({
            "source": call.source.id,
            "tables": tables,
        })))
    }

    #[tool(description = "Describe a table, including columns, keys and relationships.")]
    async fn describe_table(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<DescribeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::DescribeTable)
        {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        let description = match self
            .state
            .metadata
            .describe_table(&call.profile, &args.table, args.schema.as_deref())
            .await
        {
            Ok(description) => description,
            Err(error) => return Ok(tool_error(error)),
        };
        Ok(CallToolResult::structured(json!({
            "source": call.source.id,
            "table": args.table,
            "description": description,
        })))
    }

    #[tool(description = "Return the database query plan for a statement.")]
    async fn explain_query(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<ExplainArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::ExplainQuery)
        {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        // shortcut: SQL class and non-executing plan policy are owned by T-05.
        let plan = match self
            .state
            .metadata
            .explain(&call.profile, &args.sql, &args.params)
            .await
        {
            Ok(plan) => plan,
            Err(error) => return Ok(tool_error(error)),
        };
        Ok(CallToolResult::structured(json!({
            "source": call.source.id,
            "plan": plan,
        })))
    }
}

impl ServerHandler for McpDbServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Authorized SQL access through per-source tool grants.")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let identity = request_identity(&context)?;
        let allowed = {
            let store = self.lock_store()?;
            Authorizer::new(&store)
                .visible_tools(&identity.token_id)
                .map_err(store_error)?
        };
        let tools = Self::tool_router()
            .list_all()
            .into_iter()
            .filter(|tool| {
                allowed
                    .iter()
                    .any(|allowed| allowed.as_str() == tool.name.as_ref())
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let tool = Tool::parse(&request.name)
            .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
        if tool != Tool::ListSources {
            let source_id = request
                .arguments
                .as_ref()
                .and_then(|arguments| arguments.get("source"))
                .and_then(|value| value.as_str());
            let authorization = {
                let store = self.lock_store()?;
                Authorizer::new(&store).authorize(&identity.token_id, source_id, tool)
            };
            if let Err(error) = authorization {
                return Ok(tool_error(error));
            }
        }
        Self::tool_router()
            .call(ToolCallContext::new(self, request, context))
            .await
    }
}

fn request_identity(context: &RequestContext<RoleServer>) -> Result<TokenIdentity, McpError> {
    let parts = context
        .extensions
        .get::<http::request::Parts>()
        .ok_or_else(|| McpError::internal_error("missing HTTP request context", None))?;
    parts
        .extensions
        .get::<TokenIdentity>()
        .cloned()
        .ok_or_else(|| McpError::internal_error("missing authenticated token", None))
}

fn store_error(error: Error) -> McpError {
    match error {
        Error::Unauthenticated | Error::TokenRevoked => {
            McpError::invalid_params(error.to_string(), None)
        }
        _ => McpError::internal_error(error.to_string(), None),
    }
}

fn tool_error(error: Error) -> CallToolResult {
    let code = match error {
        Error::SourceRequired => "SOURCE_REQUIRED",
        Error::SourceNotGranted(_) => "SOURCE_NOT_GRANTED",
        Error::SourceArchived(_) => "SOURCE_ARCHIVED",
        Error::ToolNotGranted(_) => "TOOL_NOT_GRANTED",
        Error::ProductionWriteForbidden => "PRODUCTION_WRITE_FORBIDDEN",
        Error::RouteNotFound(_) => "ROUTE_NOT_FOUND",
        Error::ProfileUnavailable(_) => "PROFILE_UNAVAILABLE",
        Error::ProfileDisabled(_) => "PROFILE_DISABLED",
        Error::MetadataUnavailable => "METADATA_UNAVAILABLE",
        Error::CredentialUnavailable => "CREDENTIAL_UNAVAILABLE",
        _ => "REQUEST_REJECTED",
    };
    CallToolResult::structured_error(json!({
        "code": code,
        "message": error.to_string(),
    }))
}
