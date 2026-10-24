//! Request adapters for delivered identity capabilities.

pub(crate) mod account;
pub(crate) mod admission;
pub(crate) mod authentication;
pub(crate) mod avatar;
mod cancellation;
pub mod challenge;
mod correction;
pub(crate) mod current;
pub(crate) mod person;
mod registration;
mod request;
mod revocation;
pub(crate) mod session;
pub mod settings;
mod suspension;
mod transfer;

use axum::extract::{DefaultBodyLimit, FromRef};
use domain::startup::Services;
use utoipa_axum::router::OpenApiRouter;

pub(crate) use crate::identity::settings::Settings;
use crate::source;

#[derive(Clone)]
pub(crate) struct Context {
    services: Services,
    settings: Settings,
    source: source::Settings,
    admission: admission::Admission,
}

impl Context {
    pub(crate) fn new(
        services: Services,
        settings: Settings,
        source: source::Settings,
        admission: admission::Admission,
    ) -> Self {
        Self {
            services,
            settings,
            source,
            admission,
        }
    }
}

impl FromRef<Context> for source::Settings {
    fn from_ref(context: &Context) -> Self {
        context.source.clone()
    }
}

pub(crate) fn router(context: Context) -> OpenApiRouter {
    authentication::router(&context)
        .merge(challenge::router())
        .merge(registration::router(&context))
        .merge(current::router(&context))
        .merge(revocation::router(&context))
        .merge(account::router(&context))
        .merge(suspension::router(&context))
        .merge(cancellation::router(&context))
        .merge(correction::router(&context))
        .merge(transfer::router(&context))
        .merge(avatar::router())
        .layer(DefaultBodyLimit::max(8192))
        .with_state(context)
}
