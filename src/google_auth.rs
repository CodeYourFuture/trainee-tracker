use anyhow::Context;
use http::Uri;
use uuid::Uuid;

use crate::{Error, GoogleAuthState, ServerState};

pub(crate) fn redirect_endpoint(server_state: &ServerState) -> String {
    format!(
        "{}/api/oauth-callbacks/google-drive",
        server_state.config.public_base_url
    )
}

#[derive(Clone, Copy)]
pub enum GoogleScope {
    Groups,
    Sheets,
}

impl GoogleScope {
    pub fn scope_str(&self) -> &'static str {
        match self {
            Self::Groups => "https://www.googleapis.com/auth/admin.directory.group.readonly",
            Self::Sheets => "https://www.googleapis.com/auth/spreadsheets.readonly",
        }
    }

    pub fn token_session_key(&self) -> &'static str {
        match self {
            Self::Groups => "google_groups_access_token",
            Self::Sheets => "google_drive_access_token",
        }
    }
}

pub(crate) async fn make_redirect_uri(
    server_state: &ServerState,
    original_uri: Uri,
    redirect_uri: &str,
    scope: GoogleScope,
) -> Result<Uri, Error> {
    let scope_str = scope.scope_str();
    let state = Uuid::new_v4();
    server_state
        .google_auth_state_cache
        .insert(
            state,
            GoogleAuthState {
                original_uri,
                google_scope: scope,
            },
        )
        .await;
    let user_consent_url = format!(
        "{}?client_id={}&access_type=offline&response_type=code&redirect_uri={}&state={}&scope={}",
        "https://accounts.google.com/o/oauth2/v2/auth",
        server_state.config.google_apis_client_id,
        redirect_uri,
        state,
        scope_str,
    )
    .parse()
    .context("Statically known correct Google APIs auth Uri couldn't be constructed")?;
    Ok(user_consent_url)
}
