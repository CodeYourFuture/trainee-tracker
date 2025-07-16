use std::collections::BTreeMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sheets::types::Sheet;

use crate::{
    newtypes::{Email, GithubLogin, Region},
    sheets::{cell_string, SheetsClient},
    Error,
};

// TODO: Replace this with a serde implementation from a Google Sheet.
pub(crate) async fn get_trainees(
    client: SheetsClient,
    sheet_id: &str,
    extra_trainees: BTreeMap<GithubLogin, Trainee>,
) -> Result<BTreeMap<GithubLogin, Trainee>, Error> {
    const EXPECTED_SHEET_NAME: &str = "Form responses 1";
    let data = client.get(sheet_id, true, &[]).await.map_err(|err| {
        err.with_context(|| {
            format!(
                "Failed to get trainees github accounts sheet with id {}",
                sheet_id
            )
        })
    })?;
    let sheet = data.body.sheets.into_iter().find(|sheet| {
        if let Some(properties) = &sheet.properties {
            properties.title == EXPECTED_SHEET_NAME
        } else {
            false
        }
    });
    if let Some(sheet) = sheet {
        let data = trainees_from_sheet(&sheet, extra_trainees).map_err(|err| {
            err.with_context(|| {
                format!(
                    "Failed to read trainees from sheet {}",
                    sheet
                        .properties
                        .map(|properties| properties.title)
                        .as_deref()
                        .unwrap_or("<unknown>")
                )
            })
        })?;
        Ok(data)
    } else {
        Err(Error::Fatal(anyhow::anyhow!(
            "Didn't find sheet '{}' in trainee GitHub sheet with id {}",
            EXPECTED_SHEET_NAME,
            sheet_id
        )))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Trainee {
    pub name: String,
    pub region: Region,
    pub github_login: GithubLogin,
    pub email: Email,
}

fn trainees_from_sheet(
    sheet: &Sheet,
    extra_trainees: BTreeMap<GithubLogin, Trainee>,
) -> Result<BTreeMap<GithubLogin, Trainee>, Error> {
    let mut trainees = extra_trainees;
    for data in &sheet.data {
        if data.start_column != 0 || data.start_row != 0 {
            return Err(Error::Fatal(anyhow::anyhow!("Reading data from Google Sheets API - got data chunk that didn't start at row=0,column=0 - got row={},column={}", data.start_row, data.start_column)));
        }
        for (row_index, row) in data.row_data.iter().enumerate() {
            if row_index == 0 {
                continue;
            }
            let cells = &row.values;
            if cells.len() < 5 {
                return Err(Error::Fatal(anyhow::anyhow!("Reading trainee data from Google Sheets API, row {} didn't have at least 5 columns", row_index)));
            }

            let github_login = GithubLogin::from(
                cell_string(&cells[3]).context("Failed to read trainee github login")?,
            );

            trainees.insert(
                github_login.clone(),
                Trainee {
                    name: cell_string(&cells[1]).context("Failed to read trainee name")?,
                    region: Region(
                        cell_string(&cells[2]).context("Failed to read trainee region")?,
                    ),
                    github_login,
                    email: Email(cell_string(&cells[4]).context("Failed to read trainee email")?),
                },
            );
        }
    }

    Ok(trainees)
}
