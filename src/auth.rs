//! Bearer-token auth as an axum extractor.

use axum::{
    async_trait,
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use subtle::ConstantTimeEq;

use crate::error::GatewayError;
use crate::tenant::{ApiKey, ApiKeyScope, Tenant};
use crate::AppState;

pub const ADMIN_KEY_HEADER: &str = "x-admin-key";

/// An authenticated tenant + the api-key that authenticated them.
#[derive(Debug, Clone)]
pub struct AuthedTenant {
    pub tenant: Tenant,
    pub key: ApiKey,
}

impl AuthedTenant {
    pub fn require_scope(&self, scope: ApiKeyScope) -> Result<(), GatewayError> {
        if self.key.scopes.contains(&scope) {
            Ok(())
        } else {
            Err(GatewayError::MissingScope(match scope {
                ApiKeyScope::ToolsInvoke => "tools:invoke",
                ApiKeyScope::Orchestrate => "orchestrate",
                ApiKeyScope::ReadUsage => "read:usage",
            }))
        }
    }
}

#[async_trait]
impl FromRequestParts<AppState> for AuthedTenant {
    type Rejection = GatewayError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let raw = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(GatewayError::Unauthorized)?;

        let token = raw
            .strip_prefix("Bearer ")
            .ok_or(GatewayError::Unauthorized)?
            .trim();
        if token.is_empty() {
            return Err(GatewayError::Unauthorized);
        }

        let (tenant, key) = state.tenants.resolve_api_key(token).await?;
        if key.revoked_at.is_some() {
            return Err(GatewayError::KeyRevoked);
        }
        Ok(AuthedTenant { tenant, key })
    }
}

/// Admin-only auth: `x-admin-key` header must equal `AppState.admin_key`.
/// Constant-time compare via [`subtle::ConstantTimeEq`].
///
/// If `admin_key` is `None`, all admin endpoints refuse with 503 — operators
/// must explicitly opt in to the admin surface by setting `HF_ADMIN_KEY` at
/// boot.
pub struct AdminAuth;

#[async_trait]
impl FromRequestParts<AppState> for AdminAuth {
    type Rejection = GatewayError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(expected) = state.admin_key.as_deref() else {
            return Err(GatewayError::AdminDisabled);
        };
        let presented = parts
            .headers
            .get(ADMIN_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .ok_or(GatewayError::Unauthorized)?;
        if presented.as_bytes().ct_eq(expected.as_bytes()).into() {
            Ok(AdminAuth)
        } else {
            Err(GatewayError::InvalidApiKey)
        }
    }
}
