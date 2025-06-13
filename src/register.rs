use anyhow::Context;
use chrono::{DateTime, NaiveDate, Utc};
use indexmap::IndexMap;
use sheets::{
    spreadsheets::Spreadsheets,
    types::{CellData, GridData},
};
use tracing::warn;

use crate::{newtypes::Email, sheets::cell_string, Error};

#[derive(Debug)]
pub struct Register {
    // Module name -> Sprint -> Email -> Attendance
    pub modules: IndexMap<String, ModuleAttendance>,
}

#[derive(Debug)]
pub struct ModuleAttendance {
    pub register_url: String,
    pub attendance: Vec<IndexMap<Email, Attendance>>,
}

#[derive(Clone, Debug)]
pub struct Attendance {
    pub name: String,
    pub email: Email,
    pub timestamp: DateTime<Utc>,
    pub region: String,
    pub register_url: String,
}

impl Attendance {
    pub fn to_attendance_enum(&self, start_time: DateTime<Utc>) -> crate::course::Attendance {
        let late_by = self.timestamp.signed_duration_since(start_time);
        if late_by.num_minutes() > 10 {
            crate::course::Attendance::Late {
                register_url: self.register_url.clone(),
            }
        } else {
            crate::course::Attendance::OnTime {
                register_url: self.register_url.clone(),
            }
        }
    }
}

pub(crate) async fn get_register(
    client: sheets::Client,
    register_sheet_id: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<Register, Error> {
    let mut modules: IndexMap<String, ModuleAttendance> = IndexMap::new();

    let data = Spreadsheets { client }
        .get(&register_sheet_id, true, &[])
        .await
        .with_context(|| format!("Failed to get spreadsheet with ID {}", register_sheet_id))?;
    for sheet in data.body.sheets {
        if let Some(properties) = &sheet.properties {
            let title = properties.title.clone();
            if modules.contains_key(&title) {
                return Err(Error::Fatal(anyhow::anyhow!(
                    "Failed to read register sheet ID {} - duplicate sheets {}",
                    register_sheet_id,
                    title
                )));
            }
            let register_url = format!(
                "{}{}gid={}",
                data.body.spreadsheet_url,
                if data.body.spreadsheet_url.contains("?") {
                    "&"
                } else {
                    "?"
                },
                properties.sheet_id
            );
            let attendance = read_module(sheet.data, register_url.clone(), start_date, end_date)
                .with_context(|| {
                    format!(
                        "Failed to read register sheet ID {} sheet {}",
                        register_sheet_id, title
                    )
                })?;
            let module = ModuleAttendance {
                register_url,
                attendance,
            };
            // TODO: Unify module names across sources (repo has Module-prefix, register does not)
            modules.insert(format!("Module-{}", title.replace(' ', "-")), module);
        } else {
            warn!("Ignoring sheet in {} with no properties", register_sheet_id);
        }
    }
    Ok(Register { modules })
}

fn read_module(
    sheet_data: Vec<GridData>,
    register_url: String,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<Vec<IndexMap<Email, Attendance>>, anyhow::Error> {
    let mut sprints = Vec::new();
    'sheet: for data in sheet_data {
        if data.start_column != 0 || data.start_row != 0 {
            return Err(anyhow::anyhow!(
                "Start column and row were {} and {}, expected 0 and 0",
                data.start_column,
                data.start_row
            ));
        }
        for (row_number, row) in data.row_data.into_iter().enumerate() {
            let cells = row.values;
            // Some sheets have documentation or pivot table
            if row_number == 0
                && cells.len() >= 1
                && cell_string(&cells[0]).unwrap_or_default() != "Name"
            {
                continue 'sheet;
            }
            if cells.len() < 7 {
                return Err(anyhow::anyhow!(
                    "Not enough columns for row {} - expected at least 7, got {} containing: {}",
                    row_number,
                    cells.len(),
                    format!("{:#?}", cells),
                ));
            }
            if row_number == 0 {
                let headings = cells
                    .iter()
                    .take(7)
                    .enumerate()
                    .map(|(col_number, cell)| {
                        cell_string(cell)
                            .with_context(|| format!("Failed to get row 0 column {}", col_number))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if headings
                    != &[
                        "Name",
                        "Email",
                        "Timestamp",
                        "Course",
                        "Module",
                        "Day",
                        "Location",
                    ]
                {
                    return Err(anyhow::anyhow!(
                        "Register sheet contained wrong headings: {}",
                        headings.join(", ")
                    ));
                }
            } else {
                if cells[0].effective_value.is_none() {
                    break;
                }
                let (sprint_number, attendance) = read_row(&cells, register_url.clone())
                    .with_context(|| {
                        format!("Failed to read attendance from row {}", row_number)
                    })?;
                if attendance.timestamp.date_naive() <= start_date
                    || attendance.timestamp.date_naive() >= end_date
                {
                    continue;
                }
                let sprint_index = sprint_number - 1;
                while sprints.len() < sprint_number {
                    sprints.push(IndexMap::new());
                }
                if sprints[sprint_index].contains_key(&attendance.email) {
                    warn!(
                        "Register sheet contained duplicate entry for sprint {} trainee {}",
                        sprint_number, attendance.email
                    );
                } else {
                    sprints[sprint_index].insert(attendance.email.clone(), attendance);
                }
            }
        }
    }
    Ok(sprints)
}

fn read_row(
    cells: &[CellData],
    register_url: String,
) -> Result<(usize, Attendance), anyhow::Error> {
    let sprint_number = extract_sprint_number(
        &cell_string(&cells[5]).context("Couldn't get sprint value from column 5")?,
    )?;
    let name = cell_string(&cells[0]).context("Failed to read name")?;
    let email = Email(cell_string(&cells[1]).context("Failed to read email")?);
    let timestamp =
        DateTime::parse_from_rfc3339(&cell_string(&cells[2]).context("Failed to read timestamp")?)
            .context("Failed to parse timestamp")?
            .to_utc();
    let region = cell_string(&cells[6]).context("Failed to read region")?;
    Ok((
        sprint_number,
        Attendance {
            name,
            email,
            timestamp,
            region,
            register_url,
        },
    ))
}

fn extract_sprint_number(cell_str: &str) -> Result<usize, anyhow::Error> {
    // TODO: Clean this up in the register.
    if cell_str == "welcome-to-code-your-future" {
        return Ok(1);
    }
    let sprint_number_str = cell_str.strip_prefix("sprint-").ok_or_else(|| {
        anyhow::anyhow!(
            "Sprint '{}' didn't start with expected prefix 'sprint-'",
            cell_str
        )
    })?;
    let number = sprint_number_str
        .parse::<usize>()
        .context("Failed to parse sprint number as number")?;
    if number == 0 || number > 20 {
        Err(anyhow::anyhow!(
            "Sprints must be in range [1..20] but got {}",
            number
        ))
    } else {
        Ok(number)
    }
}
