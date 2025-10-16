use std::collections::BTreeMap;

use sheets::types::Sheet;

use crate::{
    newtypes::GithubLogin,
    prs::{CheckStatus, ReviewerStaffOnlyDetails},
    sheets::{cell_bool, cell_string, SheetsClient},
    Error,
};

pub(crate) async fn get_reviewer_staff_info(
    client: SheetsClient,
    sheet_id: &str,
) -> Result<BTreeMap<GithubLogin, ReviewerStaffOnlyDetails>, Error> {
    const EXPECTED_SHEET_NAME: &str = "Sheet1";
    let data = client.get(sheet_id, true, &[]).await.map_err(|err| {
        err.with_context(|| {
            format!(
                "Failed to get reviewer staff detail sheet with id {}",
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
        let data = reviewer_staff_detail_from_sheet(&sheet).map_err(|err| {
            err.with_context(|| {
                format!(
                    "Failed to read reviewer staff details from sheet {}",
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
            "Didn't find sheet '{}' in reviewer staff detail sheet with id {}",
            EXPECTED_SHEET_NAME,
            sheet_id
        )))
    }
}

fn reviewer_staff_detail_from_sheet(
    sheet: &Sheet,
) -> Result<BTreeMap<GithubLogin, ReviewerStaffOnlyDetails>, Error> {
    let mut reviewers = BTreeMap::new();
    for data in &sheet.data {
        if data.start_column != 0 || data.start_row != 0 {
            return Err(Error::Fatal(anyhow::anyhow!("Reading data from Google Sheets API - got data chunk that didn't start at row=0,column=0 - got row={},column={}", data.start_row, data.start_column)));
        }
        for (row_index, row) in data.row_data.iter().enumerate() {
            if row_index == 0 {
                continue;
            }
            let cells = &row.values;
            if cells.len() < 6 {
                continue;
            }

            let github_login = GithubLogin::from(cell_string(&cells[0]));

            let notes = match cells.get(6) {
                Some(cell) => cell_string(cell),
                None => String::new(),
            };

            let checked = match (cell_bool(&cells[3]), cell_bool(&cells[4])) {
                (true, false) => CheckStatus::CheckedAndOk,
                (true, true) => CheckStatus::CheckedAndCheckAgain,
                (false, _) => CheckStatus::Unchecked,
            };

            reviewers.insert(
                github_login.clone(),
                ReviewerStaffOnlyDetails {
                    name: cell_string(&cells[1]),
                    attended_training: cell_bool(&cells[2]),
                    checked,
                    quality: cell_string(&cells[5]),
                    notes,
                },
            );
        }
    }

    Ok(reviewers)
}
