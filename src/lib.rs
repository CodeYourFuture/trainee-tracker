use std::fmt::Display;

use askama::Template;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use moka::future::Cache;
use tracing::error;
use uuid::Uuid;

pub mod auth;
pub mod config;
pub use config::Config;
pub mod course;
pub mod endpoints;
pub mod frontend;
pub mod github_accounts;
pub mod newtypes;
pub mod octocrab;
pub mod prs;
pub mod register;
pub mod sheets;

#[derive(Clone)]
pub struct ServerState {
    pub auth_state_cache: Cache<Uuid, Uri>,
    pub config: Config,
}

impl ServerState {
    pub fn new(config: Config) -> ServerState {
        ServerState {
            auth_state_cache: Cache::new(1_000_000),
            config,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    UserFacing(String),
    Fatal(anyhow::Error),
    Redirect(Uri),
}

impl Error {
    fn context(self, context: &'static str) -> Self {
        match self {
            Self::UserFacing(message) => Self::UserFacing(message),
            Self::Fatal(err) => Self::Fatal(err.context(context)),
            Self::Redirect(redirect) => Self::Redirect(redirect),
        }
    }

    fn with_context<F: FnOnce() -> String>(self, f: F) -> Self {
        match self {
            Self::UserFacing(message) => Self::UserFacing(message),
            Self::Fatal(err) => Self::Fatal(err.context(f())),
            Self::Redirect(redirect) => Self::Redirect(redirect),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Error::Fatal(err) => {
                error!("Fatal error: {error:?}", error = err);
                (StatusCode::INTERNAL_SERVER_ERROR, "An error occurred").into_response()
            }
            Error::UserFacing(message) => {
                error!("Fatal user-facing error: {message}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("An error occurred: {message}"),
                )
                    .into_response()
            }
            Error::Redirect(redirect_uri) => {
                let rendered = crate::frontend::Redirect { redirect_uri }
                    .render()
                    .map_err(|err| Error::Fatal(err.into()).context("Failed to render Redirect"));
                match rendered {
                    Ok(str) => Html(str).into_response(),
                    Err(err) => err.into_response(),
                }
            }
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Fatal(err) => err.fmt(f),
            Error::UserFacing(message) => write!(f, "{}", message),
            Error::Redirect(_) => write!(f, "<redirect>"),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Error::Fatal(error)
    }
}
