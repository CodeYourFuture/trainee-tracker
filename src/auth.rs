use anyhow::{anyhow, Context};
use askama::Template;
use axum::{
    extract::{Query, State},
    response::Html,
};
use http::Uri;
use serde::Deserialize;
use sheets::Client;
use tower_sessions::Session;
use uuid::Uuid;

use crate::{
    slack::{make_slack_redirect_uri, SLACK_ACCESS_TOKEN_SESSION_KEY},
    Config, Error, ServerState,
};

#[derive(Deserialize)]
pub struct OauthCallbackParams {
    code: String,
    state: Uuid,
}

pub(crate) const GITHUB_ACCESS_TOKEN_SESSION_KEY: &str = "github_access_token";

pub async fn handle_github_oauth_callback(
    State(server_state): State<ServerState>,
    session: Session,
    params: Query<OauthCallbackParams>,
) -> Result<Html<String>, Error> {
    let access_token =
        exchange_github_oauth_code_for_access_token(&server_state.config, &params.code)
            .await
            .context("Failed to exchange GitHub oauth token")?;
    session
        .insert(GITHUB_ACCESS_TOKEN_SESSION_KEY, access_token)
        .await
        .context("Session insert error")?;
    let redirect_uri = server_state
        .github_auth_state_cache
        .remove(&params.state)
        .await;
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

pub(crate) async fn github_auth_redirect_url(
    server_state: &ServerState,
    original_uri: Uri,
) -> Result<Uri, Error> {
    let uuid = Uuid::new_v4();
    let redirect_url = format!("https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}/api/oauth-callbacks/github&scope=read:user%20read:org&state={}", server_state.config.github_client_id, server_state.config.public_base_url, uuid);
    server_state
        .github_auth_state_cache
        .insert(uuid, original_uri)
        .await;
    Ok(redirect_url
        .parse()
        .context("Statically known correct GitHub auth Uri couldn't be constructed")?)
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
    let auth_state = if let Some(auth_state) = server_state
        .google_auth_state_cache
        .remove(&params.state)
        .await
    {
        auth_state
    } else {
        return Err(Error::Fatal(anyhow!("Unrecognised state")));
    };

    let redirect_uri = format!(
        "{}/api/oauth-callbacks/google-drive",
        server_state.config.public_base_url
    );
    let mut client = Client::new(
        server_state.config.google_apis_client_id.clone(),
        (*server_state.config.google_apis_client_secret).clone(),
        redirect_uri,
        String::new(),
        String::new(),
    );

    let access_token = client
        .get_access_token(&params.code, params.state.to_string().as_str())
        .await
        .context("Failed to get access token")?;
    session
        .insert(
            auth_state.google_scope.token_session_key(),
            &access_token.access_token,
        )
        .await
        .context("Session insert error")?;

    Err(Error::Redirect(auth_state.original_uri))
}

pub async fn handle_slack_oauth_callback(
    State(server_state): State<ServerState>,
    session: Session,
    Query(params): Query<OauthCallbackParams>,
) -> Result<Html<String>, Error> {
    let client = slack_with_types::client::Client::new_without_auth(
        reqwest::Client::new(),
        slack_with_types::client::RateLimiter::new(),
    );
    let response: slack_with_types::oauth::OauthExchangeResponse = client
        .post(
            "oauth.v2.access",
            &slack_with_types::oauth::OauthExchangeRequest {
                client_id: server_state.config.slack_client_id,
                client_secret: server_state.config.slack_client_secret.to_string(),
                code: params.code,
                redirect_uri: Some(make_slack_redirect_uri(
                    &server_state.config.public_base_url,
                )),
            },
        )
        .await
        .context("Failed to exchange oauth token")?;

    session
        .insert(SLACK_ACCESS_TOKEN_SESSION_KEY, response.access_token)
        .await
        .context("Session insert error")?;
    let redirect_uri = server_state
        .slack_auth_state_cache
        .remove(&params.state)
        .await;
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
