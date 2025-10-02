use std::collections::{btree_map::Entry, BTreeMap};

use anyhow::Context;
use chrono::{NaiveDate, Utc};
use serde::Serialize;
use tracing::warn;

use crate::{
    sheets::{cell_date, cell_string, SheetsClient},
    Error,
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
    let data = client
        .get(mentoring_records_sheet_id, true, &[])
        .await
        .map_err(|err| {
            err.with_context(|| {
                format!(
                    "Failed to get spreadsheet with ID {}",
                    mentoring_records_sheet_id
                )
            })
        })?;
    let sheet = data
        .body
        .sheets
        .into_iter()
        .find(|sheet| {
            sheet
                .properties
                .as_ref()
                .map(|properties| properties.title.as_str())
                == Some("Sheet1")
        })
        .ok_or_else(|| {
            Error::Fatal(anyhow::anyhow!(
                "Couldn't find Sheet1 in spreadsheet with ID {}",
                mentoring_records_sheet_id
            ))
        })?;

    let mut mentoring_records = MentoringRecords {
        records: BTreeMap::new(),
    };

    for sheet_data in sheet.data {
        if sheet_data.start_column != 0 || sheet_data.start_row != 0 {
            return Err(Error::Fatal(anyhow::anyhow!(
                "Start column and row were {} and {}, expected 0 and 0",
                sheet_data.start_column,
                sheet_data.start_row
            )));
        }

        for (row_number, row) in sheet_data.row_data.into_iter().enumerate() {
            let cells = row.values;
            if cells.len() < 6 {
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
                let headings = cells
                    .iter()
                    .take(6)
                    .enumerate()
                    .map(|(col_number, cell)| {
                        cell_string(cell)
                            .with_context(|| format!("Failed to get row 0 column {}", col_number))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
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
                let name = cell_string(&cells[0])
                    .with_context(|| format!("Failed to read name from row {}", row_number + 1))?;
                let date = cell_date(&cells[2])
                    .with_context(|| format!("Failed to parse date from row {}", row_number + 1))?;
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
    }
    Ok(mentoring_records)
}
