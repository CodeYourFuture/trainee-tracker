use std::{sync::Arc, time::Duration};

use anyhow::Context;
use axum::response::Redirect;
use http::{header::USER_AGENT, HeaderValue, Uri};
use hyper_rustls::HttpsConnectorBuilder;
use octocrab::{
    service::middleware::{
        auth_header::AuthHeaderLayer, base_uri::BaseUriLayer, extra_headers::ExtraHeadersLayer,
        retry::RetryConfig,
    },
    AuthState, Octocrab, OctocrabBuilder,
};
use serde::de::DeserializeOwned;
use tower::retry::RetryLayer;
use tower_sessions::Session;

use crate::{
    auth::{github_auth_redirect_url, GITHUB_ACCESS_TOKEN_SESSION_KEY},
    Error, ServerState,
};

pub(crate) async fn octocrab(
    session: &Session,
    server_state: &ServerState,
    original_uri: Uri,
) -> Result<Octocrab, Error> {
    let maybe_token: Option<String> = session
        .get(GITHUB_ACCESS_TOKEN_SESSION_KEY)
        .await
        .context("Session load error")?;

    if let Some(token) = maybe_token {
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_only()
            .enable_all_versions()
            .build();

        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(connector);

        const GITHUB_BASE_URI: &str = "https://api.github.com";
        const GITHUB_BASE_UPLOAD_URI: &str = "https://uploads.github.com";

        let octocrab = OctocrabBuilder::new_empty()
            .with_service(client)
            .with_layer(&BaseUriLayer::new(Uri::from_static(GITHUB_BASE_URI)))
            .with_layer(&octocrab_rate_limiter::AccessTokenRateLimitLayer::new(
                // Keep rate limit semaphores around for 5 minutes.
                // We could probably drop this lower if we wanted.
                // If our rate limit enforcement starts looking back over more data (e.g. hour-long request counts), we may want to increase this.
                Duration::from_secs(300),
            ))
            .with_layer(&RetryLayer::new(RetryConfig::Simple(3)))
            .with_layer(&tower_http::follow_redirect::FollowRedirectLayer::new())
            .with_layer(&ExtraHeadersLayer::new(Arc::new(vec![(
                USER_AGENT,
                HeaderValue::from_static("octocrab"),
            )])))
            .with_layer(&AuthHeaderLayer::new(
                Some(
                    HeaderValue::from_str(&format!("Bearer {token}"))
                        .context("Token couldn't used as a header")?,
                ),
                Uri::from_static(GITHUB_BASE_URI),
                Uri::from_static(GITHUB_BASE_UPLOAD_URI),
            ))
            .with_auth(AuthState::None)
            .build()
            // UNWRAP: build is infallible.
            .unwrap();
        Ok(octocrab)
    } else {
        Err(Error::Redirect(Redirect::to(
            &github_auth_redirect_url(server_state, original_uri).await,
        )))
    }
}

pub(crate) async fn all_pages<T: DeserializeOwned>(
    description: &str,
    octocrab: &Octocrab,
    func: impl AsyncFnOnce() -> Result<octocrab::Page<T>, octocrab::Error>,
) -> Result<Vec<T>, Error> {
    let page = func()
        .await
        .with_context(|| format!("Failed to get first page of {description}"))?;
    let all = octocrab
        .all_pages(page)
        .await
        .with_context(|| format!("Failed to get all pages of {description}"))?;
    Ok(all)
}
