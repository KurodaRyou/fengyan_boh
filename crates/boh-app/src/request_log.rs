//! Access logging uses explicit metadata, never request or response bodies.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Json;
use axum::extract::{FromRequest, MatchedPath, Request, rejection::JsonRejection};
use axum::middleware::Next;
use axum::response::Response;
use boh_domain::CommandId;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::actor::Actor;

#[derive(Clone, Default)]
pub(crate) struct Context(Arc<Mutex<Identity>>);

#[derive(Clone, Copy, Default)]
struct Identity {
    actor: Option<Actor>,
    command: Option<(&'static str, CommandId)>,
}

impl Context {
    pub(crate) fn actor(&self, actor: Actor) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).actor = Some(actor);
    }

    fn command(&self, kind: &'static str, id: CommandId) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).command = Some((kind, id));
    }
}

#[derive(Clone)]
pub(crate) struct ErrorMetadata {
    code: &'static str,
    reasons: Vec<String>,
}

impl ErrorMetadata {
    pub(crate) fn new(code: &'static str, details: &Map<String, Value>) -> Self {
        let mut reasons: Vec<_> = details
            .get("confirmations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("code").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        reasons.sort();
        reasons.dedup();
        Self { code, reasons }
    }
}

pub(crate) async fn log(mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let context = Context::default();
    request.extensions_mut().insert(context.clone());
    let response = next.run(request).await;
    let identity = *context.0.lock().unwrap_or_else(|e| e.into_inner());
    let status = response.status();
    let error = response.extensions().get::<ErrorMetadata>();
    let code = if status.as_u16() == 500 {
        Some("INTERNAL_ERROR")
    } else {
        error.map(|error| error.code)
    };
    let reasons = error
        .filter(|_| status.as_u16() != 500)
        .filter(|error| !error.reasons.is_empty())
        .map(|error| error.reasons.join(","));
    tracing::info!(
        method = %method,
        route = route.as_deref(),
        status = status.as_u16(),
        error_code = code,
        reason_codes = reasons.as_deref(),
        command_type = identity.command.map(|(kind, _)| kind),
        command_id = identity.command.map(|(_, id)| tracing::field::display(id)),
        actor_id = identity.actor.map(|actor| tracing::field::display(actor.employee_id)),
        device_id = identity.actor.map(|actor| tracing::field::display(actor.device_id)),
        elapsed_ms = started.elapsed().as_millis(),
        "HTTP request completed"
    );
    response
}

/// Like Json, with command metadata captured only after strong-type parsing.
/// Handlers retain their existing permission, validation and execution order.
pub(crate) struct CommandJson<T>(pub T);

pub(crate) trait LoggedCommand {
    const KIND: &'static str;
    fn command_id(&self) -> CommandId;
}

impl<T: DeserializeOwned + LoggedCommand, S: Send + Sync> FromRequest<S> for CommandJson<T> {
    type Rejection = JsonRejection;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let context = request.extensions().get::<Context>().cloned();
        let Json(command) = Json::<T>::from_request(request, state).await?;
        if let Some(context) = context {
            context.command(T::KIND, command.command_id());
        }
        Ok(Self(command))
    }
}

macro_rules! commands {
    ($($ty:ty => $kind:literal),* $(,)?) => { $(
        impl LoggedCommand for $ty {
            const KIND: &'static str = $kind;
            fn command_id(&self) -> CommandId { self.command_id }
        }
    )* };
}
commands! {
    boh_domain::equipment::CreateEquipment => "equipment.create",
    boh_domain::equipment::UpdateEquipment => "equipment.update",
    boh_domain::temperature::LogTemperature => "temperature.log",
    boh_domain::receiving::CreateReceipt => "receipt.create",
    boh_domain::waste::LogWaste => "waste.log",
    boh_domain::master_data::CreateItem => "item.create",
    boh_domain::master_data::UpdateItem => "item.update",
    boh_domain::master_data::CreateRecipe => "recipe.create",
    boh_domain::master_data::UpdateRecipe => "recipe.update",
    boh_domain::master_data::AddRecipeVersion => "recipe.add_version",
    boh_domain::master_data::CreateSupplier => "supplier.create",
    boh_domain::master_data::UpdateSupplier => "supplier.update",
    boh_domain::master_data::CreateWasteReason => "waste_reason.create",
    boh_domain::master_data::UpdateWasteReason => "waste_reason.update",
}
