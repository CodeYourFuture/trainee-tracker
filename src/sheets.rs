use anyhow::Context;
use http::Uri;
use sheets::{spreadsheets::Spreadsheets, types::CellData};
use tower_sessions::Session;
use uuid::Uuid;

use crate::{auth::GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY, Error, ServerState};

pub(crate) fn cell_string(cell: &CellData) -> Result<String, anyhow::Error> {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        Ok(value.string_value)
    } else {
        Ok(String::new())
    }
}

pub(crate) async fn sheets_client(
    session: &Session,
    server_state: ServerState,
    original_uri: Uri,
) -> Result<SheetsClient, Error> {
    let maybe_token: Option<String> = session
        .get(GOOGLE_DRIVE_ACCESS_TOKEN_SESSION_KEY)
        .await
        .context("Session load error")?;

    let redirect_endpoint = redirect_endpoint(&server_state);

    if let Some(token) = maybe_token {
        let client = ::sheets::Client::new(
            server_state.config.google_sheets_client_id.clone(),
            server_state.config.google_sheets_client_secret.to_string(),
            &redirect_endpoint,
            token,
            "",
        );
        Ok(SheetsClient {
            client,
            original_uri,
            server_state,
        })
    } else {
        Err(Error::Redirect(
            make_redirect_uri(&server_state, original_uri, &redirect_endpoint).await,
        ))
    }
}

fn redirect_endpoint(server_state: &ServerState) -> String {
    format!(
        "{}/api/oauth-callbacks/google-drive",
        server_state.config.public_base_url
    )
}

async fn make_redirect_uri(
    server_state: &ServerState,
    original_uri: Uri,
    redirect_uri: &str,
) -> String {
    let state = Uuid::new_v4();
    server_state
        .auth_state_cache
        .insert(state, original_uri)
        .await;
    format!(
        "{}?client_id={}&access_type=offline&response_type=code&redirect_uri={}&state={}&scope=https://www.googleapis.com/auth/spreadsheets.readonly",
        "https://accounts.google.com/o/oauth2/v2/auth", server_state.config.google_sheets_client_id, redirect_uri, state
    )
}

#[derive(Clone)]
pub struct SheetsClient {
    client: ::sheets::Client,
    original_uri: Uri,
    server_state: ServerState,
}

impl SheetsClient {
    pub async fn get(
        self,
        sheet_id: &str,
        include_grid_data: bool,
        ranges: &[String],
    ) -> Result<::sheets::Response<::sheets::types::Spreadsheet>, Error> {
        let result = Spreadsheets {
            client: self.client,
        }
        .get(sheet_id, include_grid_data, ranges)
        .await;
        match result {
            Ok(value) => Ok(value),
            Err(::sheets::ClientError::HttpError { status, .. }) if status.as_u16() == 401 => {
                Err(Error::Redirect(
                    make_redirect_uri(
                        &self.server_state,
                        self.original_uri,
                        &&redirect_endpoint(&self.server_state),
                    )
                    .await,
                ))
            }
            Err(err) => Err(Error::Fatal(err.into())),
        }
    }
}
