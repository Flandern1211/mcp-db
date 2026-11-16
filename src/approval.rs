use sha2::{Digest, Sha256};

use crate::state::{ApprovalPolicy, Environment, Tool};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Consumed,
}

impl ApprovalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
            Self::Consumed => "consumed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "denied" => Some(Self::Denied),
            "expired" => Some(Self::Expired),
            "consumed" => Some(Self::Consumed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChannel {
    Elicitation,
    Admin,
}

impl ApprovalChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Elicitation => "elicitation",
            Self::Admin => "admin",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "elicitation" => Some(Self::Elicitation),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Succeeded,
    Failed,
    Unknown,
}

impl ApprovalOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalDraft {
    pub token_id: String,
    pub agent: String,
    pub source_id: String,
    pub source_environment: Environment,
    pub tool: Tool,
    pub sql: Option<String>,
    pub params_json: String,
    pub profile_id: i64,
    pub profile_version: i64,
    pub route_id: i64,
    pub route_version: i64,
    pub grant_policy: ApprovalPolicy,
    pub route_min_policy: ApprovalPolicy,
    pub high_risk: bool,
}

impl ApprovalDraft {
    pub fn digest(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"mcpdb:approval:v1:");
        field(&mut hasher, self.token_id.as_bytes());
        field(&mut hasher, self.agent.as_bytes());
        field(&mut hasher, self.source_id.as_bytes());
        field(&mut hasher, self.source_environment.as_str().as_bytes());
        field(&mut hasher, self.tool.as_str().as_bytes());
        field(
            &mut hasher,
            self.sql.as_deref().unwrap_or_default().as_bytes(),
        );
        field(&mut hasher, self.params_json.as_bytes());
        hasher.update(self.profile_id.to_be_bytes());
        hasher.update(self.profile_version.to_be_bytes());
        hasher.update(self.route_id.to_be_bytes());
        hasher.update(self.route_version.to_be_bytes());
        field(&mut hasher, self.grant_policy.as_str().as_bytes());
        field(&mut hasher, self.route_min_policy.as_str().as_bytes());
        hasher.update([u8::from(self.high_risk)]);
        format!("{:x}", hasher.finalize())
    }
}

fn field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewApprovalRequest {
    pub draft: ApprovalDraft,
    pub channel: ApprovalChannel,
    pub status: ApprovalStatus,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub id: i64,
    pub draft: ApprovalDraft,
    pub request_digest: String,
    pub channel: ApprovalChannel,
    pub status: ApprovalStatus,
    pub created_at: i64,
    pub expires_at: i64,
    pub decided_at: Option<i64>,
    pub decided_by: Option<String>,
    pub consumed_at: Option<i64>,
    pub outcome: Option<ApprovalOutcome>,
    pub finished_at: Option<i64>,
    pub row_count: Option<u64>,
    pub error_code: Option<String>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionOutcome {
    Approved,
    Denied,
    AlreadyDecided,
    Expired,
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed(Box<ApprovalRecord>),
    NotFound,
    TokenMismatch,
    DigestMismatch,
    NotApproved,
    Expired,
    Stale(String),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RecoverySummary {
    pub pending_expired: usize,
    pub approved_expired: usize,
    pub unknown_marked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub ts: i64,
    pub event: String,
    pub approval_id: Option<i64>,
    pub token_id: Option<String>,
    pub agent: Option<String>,
    pub source_id: Option<String>,
    pub tool: String,
    pub profile_id: Option<i64>,
    pub profile_version: Option<i64>,
    pub route_id: Option<i64>,
    pub route_version: Option<i64>,
    pub request_digest: Option<String>,
    pub channel: Option<String>,
    pub decision: String,
    pub actor: String,
    pub high_risk: bool,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub duration_ms: Option<i64>,
    pub row_count: Option<i64>,
    pub error_code: Option<String>,
}

impl AuditEvent {
    pub fn for_draft(
        draft: &ApprovalDraft,
        event: &str,
        decision: &str,
        actor: &str,
        ts: i64,
    ) -> Self {
        Self {
            ts,
            event: event.to_string(),
            approval_id: None,
            token_id: Some(draft.token_id.clone()),
            agent: Some(draft.agent.clone()),
            source_id: Some(draft.source_id.clone()),
            tool: draft.tool.as_str().to_string(),
            profile_id: Some(draft.profile_id),
            profile_version: Some(draft.profile_version),
            route_id: Some(draft.route_id),
            route_version: Some(draft.route_version),
            request_digest: Some(draft.digest()),
            channel: None,
            decision: decision.to_string(),
            actor: actor.to_string(),
            high_risk: draft.high_risk,
            reason_code: None,
            reason: None,
            duration_ms: None,
            row_count: None,
            error_code: None,
        }
    }

    pub fn for_approval(
        record: &ApprovalRecord,
        event: &str,
        decision: &str,
        actor: &str,
        ts: i64,
    ) -> Self {
        Self {
            ts,
            event: event.to_string(),
            approval_id: Some(record.id),
            token_id: Some(record.draft.token_id.clone()),
            agent: Some(record.draft.agent.clone()),
            source_id: Some(record.draft.source_id.clone()),
            tool: record.draft.tool.as_str().to_string(),
            profile_id: Some(record.draft.profile_id),
            profile_version: Some(record.draft.profile_version),
            route_id: Some(record.draft.route_id),
            route_version: Some(record.draft.route_version),
            request_digest: Some(record.request_digest.clone()),
            channel: Some(record.channel.as_str().to_string()),
            decision: decision.to_string(),
            actor: actor.to_string(),
            high_risk: record.draft.high_risk,
            reason_code: record.reason_code.clone(),
            reason: record.reason.clone(),
            duration_ms: None,
            row_count: None,
            error_code: None,
        }
    }
}
