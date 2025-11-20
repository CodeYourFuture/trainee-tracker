use std::collections::BTreeMap;

use crate::{
    Error,
    newtypes::GithubLogin,
    prs::{CheckStatus, ReviewerStaffOnlyDetails},
    sheets::{Sheet, SheetsClient, cell_bool, cell_string},
};

pub(crate) async fn get_reviewer_staff_info(
    client: SheetsClient,
    sheet_id: &str,
) -> Result<BTreeMap<GithubLogin, ReviewerStaffOnlyDetails>, Error> {
    const EXPECTED_SHEET_NAME: &str = "Sheet1";
    let mut data = client.get(sheet_id).await.map_err(|err| {
        err.with_context(|| {
            format!(
                "Failed to get reviewer staff detail sheet with id {}",
                sheet_id
            )
        })
    })?;
    let sheet = data.remove(EXPECTED_SHEET_NAME);
    if let Some(sheet) = sheet {
        let data = reviewer_staff_detail_from_sheet(&sheet).map_err(|err| {
            err.with_context(|| {
                format!(
                    "Failed to read reviewer staff details from sheet {}",
                    EXPECTED_SHEET_NAME
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

    for (row_index, cells) in sheet.rows.iter().enumerate() {
        if row_index == 0 {
            continue;
        }
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

    Ok(reviewers)
}
