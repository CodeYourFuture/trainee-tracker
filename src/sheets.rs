use std::collections::BTreeMap;

use anyhow::Context;
use chrono::Days;
use google_sheets4::{
    Sheets,
    api::{CellData, ErrorValue},
};
use http::{HeaderMap, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use serde_json::Value;
use tower_sessions::Session;
use tracing::warn;

use crate::{
    Error, ServerState,
    google_auth::{GoogleScope, make_redirect_uri, redirect_endpoint},
};

// This is documented as a union where at most one value is set, per https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets/other#ExtendedValue
#[allow(unused)]
enum ExtendedValue {
    String(String),
    Number(f64),
    Bool(bool),
    Formula(String),
    Error(ErrorValue),
    None,
}

impl From<&CellData> for ExtendedValue {
    fn from(value: &CellData) -> Self {
        if let Some(value) = &value.effective_value {
            if let Some(value) = &value.string_value {
                ExtendedValue::String(value.clone())
            } else if let Some(value) = value.number_value {
                ExtendedValue::Number(value)
            } else if let Some(value) = &value.error_value {
                ExtendedValue::Error(value.clone())
            } else if let Some(value) = &value.formula_value {
                ExtendedValue::Formula(value.clone())
            } else if let Some(value) = value.bool_value {
                ExtendedValue::Bool(value)
            } else {
                ExtendedValue::None
            }
        } else {
            ExtendedValue::None
        }
    }
}

pub(crate) fn cell_string(cell: &CellData) -> String {
    if let ExtendedValue::String(value) = ExtendedValue::from(cell) {
        value
    } else {
        String::new()
    }
}

pub(crate) fn cell_bool(cell: &CellData) -> bool {
    if let ExtendedValue::Bool(value) = ExtendedValue::from(cell) {
        value
    } else {
        false
    }
}

pub(crate) fn cell_date(cell: &CellData) -> Result<chrono::NaiveDate, anyhow::Error> {
    if let ExtendedValue::Number(value) = ExtendedValue::from(cell) {
        // UNWRAP: Statically known valid date.
        let epoch = chrono::NaiveDate::from_ymd_opt(1899, 12, 30).unwrap();
        // AS: Hopefully this is ok, Google Sheets claims it will be valid...
        let days = value as u64;
        epoch
            .checked_add_days(Days::new(days))
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {} as a date", value))
    } else {
        Err(anyhow::anyhow!(
            "Failed to parse cell containing {} as a date",
            cell.formatted_value.clone().unwrap_or_default()
        ))
    }
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
                    "Invalid {} header: {}",
                    AUTHORIZATION_HEADER, e
                )));
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
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(
                    hyper_rustls::HttpsConnectorBuilder::new()
                        .with_native_roots()
                        .unwrap()
                        .https_only()
                        .enable_http1()
                        .enable_http2()
                        .build(),
                );

        let client = Sheets::new(client, token);
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
    client: Sheets<HttpsConnector<HttpConnector>>,
    original_uri: Uri,
    server_state: ServerState,
}

pub struct Sheet {
    pub title: String,
    pub rows: Vec<Vec<CellData>>,
    pub id: String,
    pub url: String,
}

impl SheetsClient {
    pub async fn get(
        self,
        sheet_id: &str,
        // ) -> Result<::sheets::Response<::sheets::types::Spreadsheet>, Error> {
    ) -> Result<BTreeMap<String, Sheet>, Error> {
        let result = self
            .client
            .spreadsheets()
            .get(sheet_id)
            .include_grid_data(true)
            .doit()
            .await;
        match result {
            Ok((_, spreadsheet)) => {
                let mut sheets = BTreeMap::new();
                let Some(url) = spreadsheet.spreadsheet_url else {
                    warn!(
                        "Fetching spreadsheet with ID {} didn't have URL metadata",
                        sheet_id
                    );
                    return Ok(sheets);
                };
                let Some(id) = spreadsheet.spreadsheet_id else {
                    warn!(
                        "Fetching spreadsheet with ID {} didn't have ID metadata",
                        sheet_id
                    );
                    return Ok(sheets);
                };
                for (sheet_index, sheet) in spreadsheet
                    .sheets
                    .unwrap_or_default()
                    .into_iter()
                    .enumerate()
                {
                    let Some(properties) = sheet.properties else {
                        warn!(
                            "Fetching spreadsheet with ID {} - sheet with index {} didn't have properties",
                            sheet_id, sheet_index
                        );
                        continue;
                    };
                    let Some(title) = properties.title else {
                        warn!(
                            "Fetching spreadsheet with ID {} - sheet with index {} didn't have title",
                            sheet_id, sheet_index
                        );
                        continue;
                    };
                    if let Some(data) = sheet.data {
                        if data.is_empty() {
                            warn!(
                                "Fetching spreadsheet with ID {} - sheet with index {} and title {} didn't have data",
                                sheet_id, sheet_index, title
                            );
                            continue;
                        }
                        for data in data {
                            if data.start_column.unwrap_or(0) != 0
                                || data.start_row.unwrap_or(0) != 0
                            {
                                return Err(Error::Fatal(anyhow::anyhow!(
                                    "Error reading spreadsheet ID {} sheet {}: Start column and row were {:?} and {:?}, expected 0 and 0",
                                    sheet_id,
                                    title,
                                    data.start_column,
                                    data.start_row
                                )));
                            }
                            if let Some(row_data) = data.row_data {
                                let rows =
                                    row_data.into_iter().filter_map(|row| row.values).collect();
                                sheets.insert(
                                    title.clone(),
                                    Sheet {
                                        rows,
                                        title: title.clone(),
                                        url: url.clone(),
                                        id: id.clone(),
                                    },
                                );
                            } else {
                                warn!(
                                    "Fetching spreadsheet with ID {} - sheet with index {} and title {} didn't have row_data",
                                    sheet_id, sheet_index, title
                                );
                            }
                        }
                    }
                }
                Ok(sheets)
            }
            Err(
                ::google_sheets4::Error::MissingAPIKey | ::google_sheets4::Error::MissingToken(..),
            ) => Err(Error::Redirect(
                make_redirect_uri(
                    &self.server_state,
                    self.original_uri,
                    &redirect_endpoint(&self.server_state),
                    GoogleScope::Sheets,
                )
                .await?,
            )),
            Err(err) => {
                // TODO: Upgrade to a let guard when https://github.com/rust-lang/rust/issues/51114 stabilises.
                if let ::google_sheets4::Error::BadRequest(ref details) = err
                    && let Value::Object(object) = details
                    && object.get("error").and_then(|error| error.get("code"))
                        == Some(&Value::Number(serde_json::Number::from_u128(403).unwrap()))
                {
                    Err(Error::PotentiallyIgnorablePermissions(err.into()))
                } else {
                    Err(Error::Fatal(err.into()))
                }
            }
        }
    }
}
