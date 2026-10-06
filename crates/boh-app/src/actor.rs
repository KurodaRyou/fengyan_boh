//! Debug-only development identity. No HTTP fallback to system identities.

use axum::extract::FromRequestParts;
use axum::http::{StatusCode, request::Parts};
use boh_domain::AggregateId;

use crate::{AppState, http::ApiError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Staff,
    Manager,
}

#[derive(Debug, Clone, Copy)]
pub struct Actor {
    pub employee_id: AggregateId,
    pub device_id: AggregateId,
    pub role: Role,
}

impl Actor {
    pub fn require_manager(self) -> Result<Self, ApiError> {
        if self.role == Role::Manager {
            Ok(self)
        } else {
            Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                "manager role required",
            ))
        }
    }
}

impl FromRequestParts<AppState> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let rejected = || {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "UNAUTHENTICATED",
                "valid identity required",
            )
        };
        if !cfg!(debug_assertions) || !state.dev_actor_stub {
            return Err(rejected());
        }
        let header = |name| {
            parts
                .headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(rejected)
        };
        let employee_id =
            AggregateId::parse(header("X-Dev-Employee-Id")?).map_err(|_| rejected())?;
        let device_id = AggregateId::parse(header("X-Dev-Device-Id")?).map_err(|_| rejected())?;
        // Parse before comparing so alternate UUID text cannot impersonate reserved IDs.
        for id in [employee_id, device_id] {
            if matches!(
                id.to_string().as_str(),
                "00000000-0000-7000-8000-000000000000" | "00000000-0000-7000-8000-000000000001"
            ) {
                return Err(rejected());
            }
        }
        let role = match header("X-Dev-Role")? {
            "STAFF" => Role::Staff,
            "MANAGER" => Role::Manager,
            _ => return Err(rejected()),
        };
        Ok(Self {
            employee_id,
            device_id,
            role,
        })
    }
}

pub struct Manager(pub Actor);

impl FromRequestParts<AppState> for Manager {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            Actor::from_request_parts(parts, state)
                .await?
                .require_manager()?,
        ))
    }
}
