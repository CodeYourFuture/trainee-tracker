use std::fmt::Display;
use std::time::Duration;

use askama::Template;
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use moka::future::Cache;
use slack_with_types::client::RateLimiter;
use tracing::error;
use uuid::Uuid;

pub mod auth;
pub mod config;
pub use config::Config;

use crate::google_auth::GoogleScope;
pub mod course;
pub mod endpoints;
pub mod frontend;
pub mod github_accounts;
pub mod google_auth;
pub mod google_groups;
pub mod newtypes;
pub mod octocrab;
pub mod prs;
pub mod register;
pub mod reviewer_staff_info;
pub mod sheets;
pub mod slack;

#[derive(Clone)]
pub struct ServerState {
    pub github_auth_state_cache: Cache<Uuid, Uri>,
    pub google_auth_state_cache: Cache<Uuid, GoogleAuthState>,
    pub slack_auth_state_cache: Cache<Uuid, Uri>,
    pub slack_rate_limiters: Cache<String, RateLimiter>,
    pub config: Config,
}

impl ServerState {
    pub fn new(config: Config) -> ServerState {
        ServerState {
            github_auth_state_cache: Cache::new(1_000_000),
            google_auth_state_cache: Cache::new(1_000_000),
            slack_auth_state_cache: Cache::new(1_000_000),
            slack_rate_limiters: Cache::builder()
                .time_to_idle(Duration::from_secs(300))
                .build(),
            config,
        }
    }
}

#[derive(Clone)]
pub struct GoogleAuthState {
    pub original_uri: Uri,
    pub google_scope: GoogleScope,
}

#[derive(Debug)]
pub enum Error {
    /// An error with a message which should be displayed to an end user.
    /// The error message should clearly explain to the user what has gone wrong, and what to do about it.
    /// Make sure the error message does not leak any private or security-sensitive data.
    UserFacing(String),
    /// An error which cannot be rectified by the user directly, or where we don't know if the error contains sensitive data.
    /// Never display this directly to a user. It can be logged on the server side, but if you're going to show something to a user, instead use a UserFacing.
    Fatal(anyhow::Error),
    /// An error message which was caused by a lack of permissions, and where the caller _may_ want to ignore the lack of data.
    /// It is up to the caller to decide whether to treat this error as fatal, or whether to e.g. fall back to default data.
    PotentiallyIgnorablePermissions(anyhow::Error),
    /// An instruction that we should redirect the user to another page.
    /// Not really an error as such. This tends to be returned by code which require auth to say "please authenticate via OAuth somewhere, and try again".
    Redirect(Uri),
}

impl Error {
    pub fn context(self, context: &'static str) -> Self {
        match self {
            Self::UserFacing(message) => Self::UserFacing(message),
            Self::Fatal(err) => Self::Fatal(err.context(context)),
            Self::PotentiallyIgnorablePermissions(err) => {
                Self::PotentiallyIgnorablePermissions(err.context(context))
            }
            Self::Redirect(redirect) => Self::Redirect(redirect),
        }
    }

    pub fn with_context<F: FnOnce() -> String>(self, f: F) -> Self {
        match self {
            Self::UserFacing(message) => Self::UserFacing(message),
            Self::Fatal(err) => Self::Fatal(err.context(f())),
            Self::PotentiallyIgnorablePermissions(err) => {
                Self::PotentiallyIgnorablePermissions(err.context(f()))
            }
            Self::Redirect(redirect) => Self::Redirect(redirect),
        }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            // We handle PotentiallyIgnorablePermissions like a Fatal error because if it was ignorable, we assume some code would have handled it before we got to making a response.
            Error::Fatal(err) | Error::PotentiallyIgnorablePermissions(err) => {
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
            Error::Fatal(err) | Error::PotentiallyIgnorablePermissions(err) => err.fmt(f),
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
