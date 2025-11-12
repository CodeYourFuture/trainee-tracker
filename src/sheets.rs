use anyhow::Context;
use http::{HeaderMap, Uri};
use sheets::{spreadsheets::Spreadsheets, types::CellData};
use tower_sessions::Session;

use crate::{
    google_auth::{make_redirect_uri, redirect_endpoint, GoogleScope},
    Error, ServerState,
};

pub(crate) fn cell_string(cell: &CellData) -> String {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        value.string_value
    } else {
        String::new()
    }
}

pub(crate) fn cell_bool(cell: &CellData) -> bool {
    let value = cell.effective_value.clone();
    if let Some(value) = value {
        value.bool_value
    } else {
        false
    }
}

pub(crate) fn cell_date(cell: &CellData) -> Result<chrono::NaiveDate, anyhow::Error> {
    let date_string = &cell.formatted_value;
    chrono::NaiveDate::parse_from_str(date_string, "%Y-%m-%d")
        .with_context(|| format!("Failed to parse {} as a date", date_string))
}

pub(crate) async fn sheets_client(
    session: &Session,
    server_state: ServerState,
    headers: HeaderMap,
    original_uri: Uri,
) -> Result<SheetsClient, Error> {
    const AUTHORIZATION_HEADER: &str = "x-authorization-google";
    let maybe_token = if let Some(auth_header) = headers.get(AUTHORIZATION_HEADER) {
        let token = match auth_header.to_str() {
            Ok(s) => Some(s.to_string()),
            Err(e) => {
                return Err(Error::UserFacing(format!(
                    "Invalid {} in the header: {}",
                    AUTHORIZATION_HEADER, e
                )))
            }
        };
        token
    } else {
        session
            .get(GoogleScope::Sheets.token_session_key())
            .await
            .context("Session load error")?
    };

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
            Err(err @ ::sheets::ClientError::HttpError { status, .. })
                if status.as_u16() == 403 =>
            {
                Err(Error::PotentiallyIgnorablePermissions(err.into()))
            }
            Err(err) => Err(Error::Fatal(err.into())),
        }
    }
}
