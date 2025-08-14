use anyhow::Context;
use http::Uri;
use sheets::{spreadsheets::Spreadsheets, types::CellData};
use tower_sessions::Session;

use crate::{
    google_auth::{make_redirect_uri, redirect_endpoint, GoogleScope},
    Error, ServerState,
};

pub(crate) fn cell_string(cell: &CellData) -> Result<String, anyhow::Error> {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        Ok(value.string_value)
    } else {
        Ok(String::new())
    }
}

pub(crate) fn cell_bool(cell: &CellData) -> Result<bool, anyhow::Error> {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        Ok(value.bool_value)
    } else {
        Ok(false)
    }
}

pub(crate) async fn sheets_client(
    session: &Session,
    server_state: ServerState,
    original_uri: Uri,
) -> Result<SheetsClient, Error> {
    let maybe_token: Option<String> = session
        .get(GoogleScope::Sheets.token_session_key())
        .await
        .context("Session load error")?;

    let redirect_endpoint = redirect_endpoint(&server_state);

    if let Some(token) = maybe_token {
        let client = ::sheets::Client::new(
            server_state.config.google_apis_client_id.clone(),
            server_state.config.google_apis_client_secret.to_string(),
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
            make_redirect_uri(
                &server_state,
                original_uri,
                &redirect_endpoint,
                GoogleScope::Sheets,
            )
            .await?,
        ))
    }
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
                        &redirect_endpoint(&self.server_state),
                        GoogleScope::Sheets,
                    )
                    .await?,
                ))
            }
            Err(err) => Err(Error::Fatal(err.into())),
        }
    }
}
