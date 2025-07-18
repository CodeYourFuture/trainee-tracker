use std::collections::BTreeSet;

use anyhow::Context;
use email_address::EmailAddress;
use gsuite_api::Client;
use http::Uri;
use tower_sessions::Session;

use crate::{
    google_auth::{make_redirect_uri, redirect_endpoint, GoogleScope},
    Error, ServerState,
};

pub async fn groups_client(
    session: &Session,
    server_state: ServerState,
    original_uri: Uri,
) -> Result<Client, Error> {
    let maybe_token: Option<String> = session
        .get(GoogleScope::Groups.token_session_key())
        .await
        .context("Session load error")?;

    let redirect_endpoint = redirect_endpoint(&server_state);

    if let Some(token) = maybe_token {
        let client = Client::new(
            server_state.config.google_apis_client_id.clone(),
            server_state.config.google_apis_client_secret.to_string(),
            &redirect_endpoint,
            token,
            "",
        );
        Ok(client)
    } else {
        Err(Error::Redirect(
            make_redirect_uri(
                &server_state,
                original_uri,
                &redirect_endpoint,
                GoogleScope::Groups,
            )
            .await?,
        ))
    }
}

pub(crate) struct GoogleGroup {
    pub email: EmailAddress,
    pub members: BTreeSet<EmailAddress>,
}

impl GoogleGroup {
    pub(crate) fn link(&self) -> String {
        let user = self.email.local_part();
        let domain = self.email.domain();
        format!("https://groups.google.com/a/{domain}/g/{user}")
    }
}
