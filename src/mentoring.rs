use std::collections::{BTreeMap, btree_map::Entry};

use anyhow::Context;
use chrono::{NaiveDate, Utc};
use google_sheets4::api::CellData;
use serde::Serialize;
use tracing::warn;

use crate::{
    Error,
    sheets::{SheetsClient, cell_date, cell_string},
};

pub struct MentoringRecords {
    records: BTreeMap<String, MentoringRecord>,
}

impl MentoringRecords {
    pub fn get(&self, name: &str) -> Option<MentoringRecord> {
        self.records.get(name).cloned()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct MentoringRecord {
    pub last_date: NaiveDate,
}

impl MentoringRecord {
    pub fn is_recent(&self) -> bool {
        let now = Utc::now().date_naive();
        let time_since = now.signed_duration_since(self.last_date);
        time_since.num_days() <= 14
    }
}

pub async fn get_mentoring_records(
    client: SheetsClient,
    mentoring_records_sheet_id: &str,
) -> Result<MentoringRecords, Error> {
    let sheet_data = get_mentoring_records_grid_data(client, mentoring_records_sheet_id).await?;

    let mut mentoring_records = MentoringRecords {
        records: BTreeMap::new(),
    };

    for (row_number, cells) in sheet_data.into_iter().enumerate() {
        if cells.is_empty() {
            continue;
        }
        if cells.len() < 6 && !cell_string(&cells[0]).is_empty() {
            warn!(
                "Parsing mentoring data from Google Sheet with ID {}: Not enough columns for row {} - expected at least 6, got {} containing: {}",
                mentoring_records_sheet_id,
                row_number,
                cells.len(),
                format!("{:#?}", cells),
            );
            continue;
        }
        if row_number == 0 {
            let headings = cells.iter().take(6).map(cell_string).collect::<Vec<_>>();
            if headings != ["Name", "Region", "Date", "Staff", "Status", "Notes"] {
                return Err(Error::Fatal(anyhow::anyhow!(
                    "Mentoring data sheet contained wrong headings: {}",
                    headings.join(", ")
                )));
            }
        } else {
            if cells[0].effective_value.is_none() {
                break;
            }
            let name = cell_string(&cells[0]);
            let date = cell_date(&cells[2]).with_context(|| {
                format!(
                    "Failed to parse date from row {} in sheet ID {}",
                    row_number + 1,
                    mentoring_records_sheet_id
                )
            })?;
            let entry = mentoring_records.records.entry(name);
            match entry {
                Entry::Vacant(entry) => {
                    entry.insert(MentoringRecord { last_date: date });
                }
                Entry::Occupied(mut entry) => {
                    if entry.get().last_date < date {
                        entry.insert(MentoringRecord { last_date: date });
                    }
                }
            }
        }
    }
    Ok(mentoring_records)
}

async fn get_mentoring_records_grid_data(
    client: SheetsClient,
    mentoring_records_sheet_id: &str,
) -> Result<Vec<Vec<CellData>>, Error> {
    let expected_sheet_title = "Feedback";
    let data_result = client.get(mentoring_records_sheet_id).await;
    let mut data = match data_result {
        Ok(data) => data,
        Err(Error::PotentiallyIgnorablePermissions(_)) => {
            return Ok(Vec::new());
        }
        Err(err) => {
            let err = err.with_context(|| {
                format!(
                    "Failed to get spreadsheet with ID {}",
                    mentoring_records_sheet_id
                )
            });
            return Err(err);
        }
    };
    let sheet = data.remove(expected_sheet_title).ok_or_else(|| {
        Error::Fatal(anyhow::anyhow!(
            "Couldn't find sheet '{}' in spreadsheet with ID {}",
            expected_sheet_title,
            mentoring_records_sheet_id
        ))
    })?;
    Ok(sheet.rows)
}
