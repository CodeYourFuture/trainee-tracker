use anyhow::{anyhow, Context};
use askama::Template;
use axum::{
    extract::{Query, State},
    response::{Html, Redirect},
};
use http::Uri;
use serde::Deserialize;
use sheets::Client;
use tower_sessions::Session;
use uuid::Uuid;

use crate::{Config, Error, ServerState};

#[derive(Deserialize)]
pub struct OauthCallbackParams {
    code: String,
    state: Uuid,
}

pub(crate) const GITHUB_ACCESS_TOKEN_SESSION_KEY: &str = "github_access_token";
pub(crate) const GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY: &str = "google_drive_access_token";

pub async fn handle_github_oauth_callback(
    State(server_state): State<ServerState>,
    session: Session,
    params: Query<OauthCallbackParams>,
) -> Result<String, Error> {
    let access_token =
        exchange_github_oauth_code_for_access_token(&server_state.config, &params.code)
            .await
            .context("Failed to exchange GitHub oauth token")?;
    session
        .insert(GITHUB_ACCESS_TOKEN_SESSION_KEY, access_token)
        .await
        .context("Session load error")?;
    let redirect_uri = server_state.auth_state_cache.remove(&params.state).await;
    if let Some(redirect_uri) = redirect_uri {
        Err(Error::Redirect(Redirect::to(&redirect_uri.to_string())))
    } else {
        Err(Error::Fatal(anyhow!("Unrecognised state")))
    }
}

pub(crate) async fn github_auth_redirect_url(
    server_state: &ServerState,
    original_uri: Uri,
) -> String {
    let uuid = Uuid::new_v4();
    let redirect_url = format!("https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}/api/oauth-callback&scope=read:user&scope=read:org&state={}", server_state.config.github_client_id, server_state.config.public_base_url, uuid);
    server_state
        .auth_state_cache
        .insert(uuid, original_uri)
        .await;
    redirect_url
}

async fn exchange_github_oauth_code_for_access_token(
    config: &Config,
    code: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();

    let response: GitHubOauthExchangeResponse = client
        .get(format!("https://github.com/login/oauth/access_token?client_id={client_id}&client_secret={client_secret}&code={code}", client_id = config.github_client_id, client_secret = *config.github_client_secret, code = code))
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(response.access_token)
}

#[derive(Deserialize)]
struct GitHubOauthExchangeResponse {
    access_token: String,
}

pub async fn handle_google_oauth_callback(
    State(server_state): State<ServerState>,
    session: Session,
    params: Query<OauthCallbackParams>,
) -> Result<Html<String>, Error> {
    let redirect_uri = format!(
        "{}/api/oauth-callbacks/google-drive",
        server_state.config.public_base_url
    );
    let mut client = Client::new(
        server_state.config.google_sheets_client_id.clone(),
        (*server_state.config.google_sheets_client_secret).clone(),
        String::from(redirect_uri),
        String::new(),
        String::new(),
    );

    let access_token = client
        .get_access_token(&params.code, params.state.to_string().as_str())
        .await
        .context("Failed to get access token")?;
    session
        .insert(
            GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY,
            &access_token.access_token,
        )
        .await
        .context("Session insert error")?;

    let redirect_uri = server_state.auth_state_cache.remove(&params.state).await;
    if let Some(redirect_uri) = redirect_uri {
        Ok(Html(
            crate::frontend::Redirect { redirect_uri }
                .render()
                .context("Failed to render")?,
        ))
    } else {
        Err(Error::Fatal(anyhow!("Unrecognised state")))
    }
}
