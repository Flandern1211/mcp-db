use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::json;
use rmcp::service::{ElicitationError, ElicitationMode, RequestContext};
use rmcp::{tool, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::approval::{
    ApprovalChannel, ApprovalDraft, ApprovalOutcome, ApprovalRecord, ApprovalStatus, ClaimOutcome,
    NewApprovalRequest,
};
use crate::executor::SqlExecutor;
use crate::metadata::MetadataProvider;
use crate::sql_policy::{validate_for_tool, ValidatedSql};
use crate::{ApprovalPolicy, AuthorizedCall, Authorizer, Error, Store, TokenIdentity, Tool};

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Mutex<Store>>,
    pub metadata: Arc<dyn MetadataProvider>,
    pub executor: Arc<dyn SqlExecutor>,
    pub rate_limiter: Arc<crate::limits::TokenRateLimiter>,
    pub elicitation: Arc<dyn ElicitationGate>,
    pub max_rows: usize,
    pub approval_timeout_ms: i64,
    pub approval_execution_ttl_ms: i64,
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
struct SqlArgs {
    #[schemars(description = "Source id; may be omitted when the token has one source")]
    source: Option<String>,
    sql: String,
    #[serde(default)]
    params: Vec<Value>,
    #[schemars(description = "Approval id returned by a previous APPROVAL_REQUIRED response")]
    approval_id: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ApprovalResponse {
    #[schemars(description = "Approve this exact operation?")]
    approved: bool,
}

rmcp::elicit_safe!(ApprovalResponse);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitationOutcome {
    Accepted,
    Declined,
    Cancelled,
    Unsupported,
    Failed,
}

#[async_trait::async_trait]
pub trait ElicitationGate: Send + Sync {
    async fn ask(&self, context: &RequestContext<RoleServer>, prompt: &str) -> ElicitationOutcome;
}

pub struct PeerElicitation;

#[async_trait::async_trait]
impl ElicitationGate for PeerElicitation {
    async fn ask(&self, context: &RequestContext<RoleServer>, prompt: &str) -> ElicitationOutcome {
        if !context
            .peer
            .supported_elicitation_modes()
            .contains(&ElicitationMode::Form)
        {
            return ElicitationOutcome::Unsupported;
        }
        match context.peer.elicit::<ApprovalResponse>(prompt).await {
            Ok(Some(response)) if response.approved => ElicitationOutcome::Accepted,
            Ok(Some(_)) | Ok(None) | Err(ElicitationError::UserDeclined) => {
                ElicitationOutcome::Declined
            }
            Err(ElicitationError::UserCancelled) => ElicitationOutcome::Cancelled,
            Err(ElicitationError::CapabilityNotSupported) => ElicitationOutcome::Unsupported,
            Err(_) => ElicitationOutcome::Failed,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ExplainArgs {
    #[schemars(description = "Source id; may be omitted when the token has one source")]
    source: Option<String>,
    sql: String,
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

    fn build_approval_draft(
        &self,
        call: &AuthorizedCall,
        validated: &ValidatedSql,
        params: &[Value],
    ) -> ApprovalDraft {
        ApprovalDraft {
            token_id: call.identity.token_id.clone(),
            agent: call.identity.agent.clone(),
            source_id: call.source.id.clone(),
            source_environment: call.source.environment,
            tool: call.tool,
            sql: Some(validated.sql.clone()),
            params_json: serde_json::to_string(params).unwrap_or_else(|_| "[]".to_string()),
            profile_id: call.profile.id,
            profile_version: call.profile.version,
            route_id: call.route.id,
            route_version: call.route.version,
            grant_policy: call.grant_policy,
            route_min_policy: call.route_min_policy,
            high_risk: validated.high_risk,
        }
    }

    fn insert_approval(
        &self,
        draft: &ApprovalDraft,
        channel: ApprovalChannel,
        status: ApprovalStatus,
        now: i64,
        decided_by: Option<&str>,
    ) -> std::result::Result<ApprovalRecord, CallToolResult> {
        let expires_at = if status == ApprovalStatus::Pending {
            now + self.state.approval_timeout_ms
        } else {
            now + self.state.approval_execution_ttl_ms
        };
        let request = NewApprovalRequest {
            draft: draft.clone(),
            channel,
            status,
            created_at: now,
            expires_at,
            decided_at: (status != ApprovalStatus::Pending).then_some(now),
            decided_by: decided_by.map(str::to_string),
            reason_code: None,
            reason: None,
        };
        let store = match self.state.store.lock() {
            Ok(store) => store,
            Err(_) => return Err(state_unavailable()),
        };
        store.insert_approval_request(&request).map_err(tool_error)
    }

    async fn execute_claimed(
        &self,
        context: &RequestContext<RoleServer>,
        record: ApprovalRecord,
        params: &[Value],
    ) -> Result<CallToolResult, McpError> {
        let profile = {
            let store = self.lock_store()?;
            store
                .get_profile(record.draft.profile_id)
                .map_err(store_error)?
        };
        let sql = record.draft.sql.as_deref().unwrap_or_default();
        let result = tokio::select! {
            biased;
            _ = context.ct.cancelled() => Err(Error::ExecutionUnknown),
            result = self.state.executor.mutate(&profile, sql, params) => result,
        };
        match result {
            Ok(outcome) => {
                let finished = {
                    let store = self.lock_store()?;
                    store.finish_approval(
                        record.id,
                        ApprovalOutcome::Succeeded,
                        Some(outcome.row_count),
                        None,
                        now_ms(),
                    )
                };
                if finished.is_err() {
                    return Ok(CallToolResult::structured_error(json!({
                        "code": "AUDIT_WRITE_FAILED",
                        "approvalId": record.id,
                    })));
                }
                Ok(CallToolResult::structured(json!({
                    "source": record.draft.source_id,
                    "rowCount": outcome.row_count,
                    "highRisk": record.draft.high_risk,
                    "approvalId": record.id,
                })))
            }
            Err(error) => {
                let not_started =
                    matches!(error, Error::ExecutionNotStarted(_) | Error::SqlRejected(_));
                let _ = {
                    let store = self.lock_store()?;
                    store.finish_approval(
                        record.id,
                        if not_started {
                            ApprovalOutcome::Failed
                        } else {
                            ApprovalOutcome::Unknown
                        },
                        None,
                        Some(if not_started {
                            "EXECUTION_NOT_STARTED"
                        } else {
                            "EXECUTION_UNKNOWN"
                        }),
                        now_ms(),
                    )
                };
                Ok(CallToolResult::structured_error(json!({
                    "code": if not_started { "EXECUTION_NOT_STARTED" } else { "EXECUTION_UNKNOWN" },
                    "message": if not_started { "database statement was not sent; approval is consumed" } else { "external execution result is unknown; do not retry automatically" },
                    "approvalId": record.id,
                    "detail": error.to_string(),
                })))
            }
        }
    }

    async fn claim_and_execute(
        &self,
        context: &RequestContext<RoleServer>,
        record: ApprovalRecord,
        params: Vec<Value>,
    ) -> Result<CallToolResult, McpError> {
        let claim = {
            let store = self.lock_store()?;
            store.claim_approval(
                record.id,
                &record.draft.token_id,
                &record.request_digest,
                now_ms(),
            )
        };
        match claim {
            Ok(ClaimOutcome::Claimed(record)) => {
                self.execute_claimed(context, *record, &params).await
            }
            Ok(other) => Ok(claim_error(other)),
            Err(error) => Ok(tool_error(error)),
        }
    }

    async fn execute_write(
        &self,
        context: &RequestContext<RoleServer>,
        call: AuthorizedCall,
        validated: ValidatedSql,
        params: Vec<Value>,
        approval_id: Option<i64>,
    ) -> Result<CallToolResult, McpError> {
        let draft = self.build_approval_draft(&call, &validated, &params);
        let now = now_ms();

        if let Some(id) = approval_id {
            let claim = {
                let store = self.lock_store()?;
                store.claim_approval(id, &draft.token_id, &draft.digest(), now)
            };
            return match claim {
                Ok(ClaimOutcome::Claimed(record)) => {
                    self.execute_claimed(context, *record, &params).await
                }
                Ok(other) => Ok(claim_error(other)),
                Err(error) => Ok(tool_error(error)),
            };
        }

        if call.policy == ApprovalPolicy::Direct && !validated.high_risk {
            let result = tokio::select! {
                biased;
                _ = context.ct.cancelled() => Err(Error::ExecutionUnknown),
                result = self.state.executor.mutate(&call.profile, &validated.sql, &params) => result,
            };
            match result {
                Ok(outcome) => {
                    let mut event = crate::approval::AuditEvent::for_draft(
                        &draft,
                        "direct_executed",
                        "allowed",
                        "mcp-client",
                        now,
                    );
                    event.row_count = Some(outcome.row_count as i64);
                    let _ = {
                        let store = self.lock_store()?;
                        store.record_audit_event(&event)
                    };
                    return Ok(CallToolResult::structured(json!({
                        "source": call.source.id,
                        "rowCount": outcome.row_count,
                        "highRisk": false,
                    })));
                }
                Err(error) => {
                    let unknown = matches!(error, Error::ExecutionUnknown);
                    let mut event = crate::approval::AuditEvent::for_draft(
                        &draft,
                        if unknown {
                            "direct_unknown"
                        } else {
                            "direct_failed"
                        },
                        if unknown { "unknown" } else { "error" },
                        "mcp-client",
                        now,
                    );
                    event.error_code = Some(
                        if unknown {
                            "EXECUTION_UNKNOWN"
                        } else {
                            "EXECUTION_FAILED"
                        }
                        .to_string(),
                    );
                    let _ = {
                        let store = self.lock_store()?;
                        store.record_audit_event(&event)
                    };
                    return Ok(tool_error(error));
                }
            }
        }

        let mut admin_required = validated.high_risk || call.tool == Tool::CreateDatabase;
        if !admin_required {
            let prompt = format!(
                "Approve {} on source {}?",
                call.tool.as_str(),
                call.source.id
            );
            match self.state.elicitation.ask(context, &prompt).await {
                ElicitationOutcome::Accepted => {
                    let record = match self.insert_approval(
                        &draft,
                        ApprovalChannel::Elicitation,
                        ApprovalStatus::Approved,
                        now,
                        Some("mcp-client"),
                    ) {
                        Ok(record) => record,
                        Err(result) => return Ok(result),
                    };
                    return self.claim_and_execute(context, record, params).await;
                }
                ElicitationOutcome::Declined | ElicitationOutcome::Cancelled => {
                    let _ = self.insert_approval(
                        &draft,
                        ApprovalChannel::Elicitation,
                        ApprovalStatus::Denied,
                        now,
                        Some("mcp-client"),
                    );
                    return Ok(CallToolResult::structured_error(json!({
                        "code": "APPROVAL_DENIED",
                        "message": "approval was declined or cancelled",
                    })));
                }
                ElicitationOutcome::Unsupported => admin_required = true,
                ElicitationOutcome::Failed => {
                    return Ok(CallToolResult::structured_error(json!({
                        "code": "ELICITATION_FAILED",
                        "message": "elicitation failed",
                    })));
                }
            }
        }

        if admin_required {
            let record = match self.insert_approval(
                &draft,
                ApprovalChannel::Admin,
                ApprovalStatus::Pending,
                now,
                None,
            ) {
                Ok(record) => record,
                Err(result) => return Ok(result),
            };
            return Ok(approval_required(&record));
        }

        Ok(CallToolResult::structured_error(json!({
            "code": "APPROVAL_REQUIRED",
            "message": "approval could not be completed",
        })))
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

    #[tool(description = "Execute one read-only SQL statement with bound parameters.")]
    async fn query_sql(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::QuerySql) {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        let validated = match validate_for_tool(&args.sql, Tool::QuerySql) {
            Ok(validated) => validated,
            Err(error) => return Ok(tool_error(error)),
        };
        let outcome = match self
            .state
            .executor
            .query(
                &call.profile,
                &validated.sql,
                &args.params,
                self.state.max_rows,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => return Ok(tool_error(error)),
        };
        Ok(CallToolResult::structured(json!({
            "source": call.source.id,
            "columns": outcome.columns,
            "rows": outcome.rows,
            "rowCount": outcome.row_count,
            "returnedRows": outcome.returned_rows,
            "truncated": outcome.truncated,
        })))
    }

    #[tool(description = "Execute one data-modifying SQL statement.")]
    async fn mutate_sql(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::MutateSql) {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        let validated = match validate_for_tool(&args.sql, Tool::MutateSql) {
            Ok(validated) => validated,
            Err(error) => return Ok(tool_error(error)),
        };
        self.execute_write(&context, call, validated, args.params, args.approval_id)
            .await
    }

    #[tool(description = "Execute one database-local DDL statement.")]
    async fn execute_ddl(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(args): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let identity = request_identity(&context)?;
        let call = match self.authorize_call(&identity, args.source.as_deref(), Tool::ExecuteDdl) {
            Ok(call) => call,
            Err(result) => return Ok(result),
        };
        let validated = match validate_for_tool(&args.sql, Tool::ExecuteDdl) {
            Ok(validated) => validated,
            Err(error) => return Ok(tool_error(error)),
        };
        self.execute_write(&context, call, validated, args.params, args.approval_id)
            .await
    }

    #[tool(description = "Return the database query plan for one read-only statement.")]
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
        let validated = match validate_for_tool(&args.sql, Tool::ExplainQuery) {
            Ok(validated) => validated,
            Err(error) => return Ok(tool_error(error)),
        };
        let plan = match self
            .state
            .executor
            .explain(&call.profile, &validated.explain_sql())
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
        let authorized = if tool != Tool::ListSources {
            let source_id = request
                .arguments
                .as_ref()
                .and_then(|args| args.get("source"))
                .and_then(Value::as_str);
            let result = {
                let store = self.lock_store()?;
                Authorizer::new(&store).authorize(&identity.token_id, source_id, tool)
            };
            match result {
                Ok(call) => Some(call),
                Err(error) => return Ok(tool_error(error)),
            }
        } else {
            None
        };
        if !self.state.rate_limiter.check(&identity.token_id) {
            let event = crate::approval::AuditEvent {
                ts: now_ms(),
                event: "rate_limited".to_string(),
                approval_id: None,
                token_id: Some(identity.token_id.clone()),
                agent: Some(identity.agent.clone()),
                source_id: authorized.as_ref().map(|call| call.source.id.clone()),
                tool: request.name.to_string(),
                profile_id: authorized.as_ref().map(|call| call.profile.id),
                profile_version: authorized.as_ref().map(|call| call.profile.version),
                route_id: authorized.as_ref().map(|call| call.route.id),
                route_version: authorized.as_ref().map(|call| call.route.version),
                request_digest: None,
                channel: None,
                decision: "rejected".to_string(),
                actor: "system".to_string(),
                high_risk: false,
                reason_code: Some("RATE_LIMITED".to_string()),
                reason: None,
                duration_ms: None,
                row_count: None,
                error_code: None,
            };
            self.lock_store()?
                .record_audit_event(&event)
                .map_err(store_error)?;
            return Ok(CallToolResult::structured_error(json!({
                "code": "RATE_LIMITED",
                "message": "token tool-call rate exceeded",
            })));
        }
        let cancellation = context.ct.clone();
        let router = Self::tool_router();
        let result = if tool.is_write() {
            // Write handlers persist the conservative outcome before returning from cancellation.
            router
                .call(ToolCallContext::new(self, request, context))
                .await
        } else {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => Ok(CallToolResult::structured_error(json!({
                    "code": "REQUEST_CANCELLED",
                    "message": "database request cancelled",
                }))),
                result = router.call(ToolCallContext::new(self, request, context)) => result,
            }
        };
        if let (Some(call), Ok(output)) = (authorized.as_ref(), &result) {
            if output.is_error == Some(true) {
                let code = output
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("TOOL_REJECTED");
                let event = crate::approval::AuditEvent {
                    ts: now_ms(),
                    event: "tool_rejected".to_string(),
                    approval_id: None,
                    token_id: Some(call.identity.token_id.clone()),
                    agent: Some(call.identity.agent.clone()),
                    source_id: Some(call.source.id.clone()),
                    tool: tool.as_str().to_string(),
                    profile_id: Some(call.profile.id),
                    profile_version: Some(call.profile.version),
                    route_id: Some(call.route.id),
                    route_version: Some(call.route.version),
                    request_digest: None,
                    channel: None,
                    decision: "rejected".to_string(),
                    actor: "system".to_string(),
                    high_risk: false,
                    reason_code: Some(code.to_string()),
                    reason: None,
                    duration_ms: None,
                    row_count: None,
                    error_code: None,
                };
                self.lock_store()?
                    .record_audit_event(&event)
                    .map_err(store_error)?;
            }
        }
        result
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
        Error::ResourceLimit(_) => "RESOURCE_LIMIT",
        Error::ExecutionNotStarted(_) => "EXECUTION_NOT_STARTED",
        Error::ExecutionUnknown => "EXECUTION_UNKNOWN",
        Error::SqlRejected(_) => "SQL_REJECTED",
        _ => "REQUEST_REJECTED",
    };
    CallToolResult::structured_error(json!({
        "code": code,
        "message": error.to_string(),
    }))
}

fn state_unavailable() -> CallToolResult {
    CallToolResult::structured_error(json!({
        "code": "STATE_UNAVAILABLE",
        "message": "state store lock poisoned",
    }))
}

fn approval_required(record: &ApprovalRecord) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "code": "APPROVAL_REQUIRED",
        "message": "admin approval is required before execution",
        "approvalId": record.id,
        "expiresAt": record.expires_at,
        "channel": record.channel.as_str(),
        "highRisk": record.draft.high_risk,
    }))
}

fn claim_error(outcome: ClaimOutcome) -> CallToolResult {
    match outcome {
        ClaimOutcome::NotFound => CallToolResult::structured_error(json!({
            "code": "APPROVAL_NOT_FOUND",
            "message": "approval request was not found",
        })),
        ClaimOutcome::TokenMismatch | ClaimOutcome::DigestMismatch => {
            CallToolResult::structured_error(json!({
                "code": "APPROVAL_STALE",
                "message": "approval does not match the exact request",
            }))
        }
        ClaimOutcome::NotApproved => CallToolResult::structured_error(json!({
            "code": "APPROVAL_NOT_PENDING",
            "message": "approval is not available for consumption",
        })),
        ClaimOutcome::Expired => CallToolResult::structured_error(json!({
            "code": "APPROVAL_EXPIRED",
            "message": "approval expired before execution",
        })),
        ClaimOutcome::Stale(reason) => CallToolResult::structured_error(json!({
            "code": "APPROVAL_STALE",
            "message": "approval is no longer valid",
            "reasonCode": reason,
        })),
        ClaimOutcome::Claimed(_) => CallToolResult::structured_error(json!({
            "code": "APPROVAL_STATE_ERROR",
        })),
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
