use crate::error::{Error, Result};
use crate::state::{
    ApprovalPolicy, Environment, Grant, Profile, Route, Source, SourceStatus, Store, TokenIdentity,
    Tool,
};

/// Read-only authorization view. Every call recomputes from the state store.
pub struct Authorizer<'a> {
    store: &'a Store,
}

pub struct AuthorizedCall {
    pub identity: TokenIdentity,
    pub source: Source,
    pub tool: Tool,
    pub profile: Profile,
    pub route: Route,
    pub policy: ApprovalPolicy,
}

impl<'a> Authorizer<'a> {
    pub fn new(store: &'a Store) -> Self {
        Self { store }
    }

    pub fn visible_sources(&self, token_id: &str) -> Result<Vec<Source>> {
        self.store.get_token_identity(token_id)?;
        self.store.list_visible_sources(token_id)
    }

    pub fn visible_tools(&self, token_id: &str) -> Result<Vec<Tool>> {
        self.store.get_token_identity(token_id)?;
        let mut tools = self.store.list_granted_tools(token_id)?;
        tools.push(Tool::ListSources);
        tools.sort_by_key(|tool| tool.order());
        tools.dedup();
        Ok(tools)
    }

    /// Resolve an exact source/tool call. A missing source with one visible source is inferred;
    /// with multiple visible sources the caller must choose one.
    pub fn authorize(
        &self,
        token_id: &str,
        source_id: Option<&str>,
        tool: Tool,
    ) -> Result<AuthorizedCall> {
        let identity = self.store.get_token_identity(token_id)?;
        if tool == Tool::ListSources {
            return Err(Error::ToolNotGranted(
                "list_sources is a discovery tool".to_string(),
            ));
        }

        let source = match source_id {
            Some(id) => self
                .store
                .get_source(id)
                .map_err(|_| Error::SourceNotGranted(id.to_string()))?,
            None => match self.store.list_visible_sources(token_id)?.as_slice() {
                [] => {
                    return Err(Error::SourceNotGranted(
                        "no source is available".to_string(),
                    ))
                }
                [source] => source.clone(),
                _ => return Err(Error::SourceRequired),
            },
        };

        if source.status != SourceStatus::Active {
            return Err(Error::SourceArchived(source.id.clone()));
        }

        let grant: Grant = match self.store.get_grant(token_id, &source.id, tool) {
            Ok(grant) => grant,
            Err(Error::NotFound(_)) => {
                return Err(Error::ToolNotGranted(format!(
                    "{}/{}",
                    source.id,
                    tool.as_str()
                )))
            }
            Err(error) => return Err(error),
        };

        if source.environment == Environment::Production && tool.is_write() {
            return Err(Error::ProductionWriteForbidden);
        }

        let route = self.store.get_route(&source.id, tool).map_err(|error| {
            if matches!(error, Error::NotFound(_)) {
                Error::RouteNotFound(format!("{}/{}", source.id, tool.as_str()))
            } else {
                error
            }
        })?;
        let profile = self.store.get_profile(route.profile_id).map_err(|error| {
            if matches!(error, Error::NotFound(_)) {
                Error::ProfileUnavailable(route.profile_id)
            } else {
                error
            }
        })?;
        if profile.source_id != source.id {
            return Err(Error::ProfileUnavailable(profile.id));
        }
        if !profile.enabled {
            return Err(Error::ProfileDisabled(profile.id));
        }
        if tool.purpose() != Some(profile.purpose) {
            return Err(Error::InvalidInput(format!(
                "tool {} does not match profile purpose {}",
                tool.as_str(),
                profile.purpose.as_str()
            )));
        }

        let policy = if tool == Tool::CreateDatabase {
            ApprovalPolicy::Approval
        } else {
            stricter(grant.policy, route.min_approval_policy)
        };

        Ok(AuthorizedCall {
            identity,
            source,
            tool,
            profile,
            route,
            policy,
        })
    }
}

fn stricter(left: ApprovalPolicy, right: ApprovalPolicy) -> ApprovalPolicy {
    if left == ApprovalPolicy::Approval || right == ApprovalPolicy::Approval {
        ApprovalPolicy::Approval
    } else {
        ApprovalPolicy::Direct
    }
}
