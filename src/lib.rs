use std::error::Error as StdError;
use std::fmt::Display;

use anyhow::Context;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use moka::future::Cache;
use tower_sessions::Session;
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

use crate::auth::GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY;

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
    Redirect(Redirect),
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
            Error::Redirect(redirect) => redirect.into_response(),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Fatal(err) => err.source(),
            Error::UserFacing(_) => None,
            Error::Redirect(_) => None,
        }
    }

    fn description(&self) -> &str {
        #[allow(deprecated)]
        match self {
            Error::Fatal(err) => err.description(),
            Error::UserFacing(_) => "description is deprecated - use display",
            Error::Redirect(_) => "description is deprecated - use display",
        }
    }

    fn cause(&self) -> Option<&dyn StdError> {
        #[allow(deprecated)]
        match self {
            Error::Fatal(err) => err.cause(),
            Error::UserFacing(_) => None,
            Error::Redirect(_) => None,
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

pub(crate) async fn sheets_client(
    session: &Session,
    server_state: &ServerState,
    original_uri: Uri,
) -> Result<::sheets::Client, Error> {
    let maybe_token: Option<String> = session
        .get(GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY)
        .await
        .context("Session load error")?;

    let redirect_uri = format!(
        "{}/api/oauth-callbacks/google-drive",
        server_state.config.public_base_url
    );

    if let Some(token) = maybe_token {
        let client = ::sheets::Client::new(
            server_state.config.google_sheets_client_id.clone(),
            server_state.config.google_sheets_client_secret.to_string(),
            redirect_uri,
            token,
            "",
        );
        Ok(client)
    } else {
        let state = Uuid::new_v4();
        server_state
            .auth_state_cache
            .insert(state, original_uri)
            .await;
        let user_consent_url = format!(
                "{}?client_id={}&access_type=offline&response_type=code&redirect_uri={}&state={}&scope=https://www.googleapis.com/auth/spreadsheets.readonly",
                "https://accounts.google.com/o/oauth2/v2/auth", server_state.config.google_sheets_client_id, redirect_uri, state
            );
        Err(Error::Redirect(Redirect::to(&user_consent_url)))
    }
}
