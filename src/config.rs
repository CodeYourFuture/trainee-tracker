use std::{collections::BTreeMap, net::IpAddr};

use chrono::NaiveDate;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_env_field::EnvField;

use crate::{
    github_accounts::Trainee,
    newtypes::{GithubLogin, Region},
};

#[derive(Clone, Deserialize)]
pub struct Config {
    pub github_org: String,
    pub github_client_id: String,
    pub github_client_secret: EnvField<String>,
    pub addr: Option<IpAddr>,
    pub port: u16,
    pub public_base_url: String,
    /// Courses being tracked. Keys are things like "itp" or "sdc".
    /// Ideally this would be less hard-coded.
    /// Possible sources of truth for this are:
    ///  * GitHub team structure (except that lacks dates)
    ///  * Class Planner API (except that has fiddly auth)
    /// We assume the following GitHub team structure:
    ///  ${course}-trainees contains groups of batches of trainees.
    ///  ${course}-mentors is a group of reviewers.
    /// e.g. for itp, we'd expect itp-trainees/2025-05 and itp-mentors to exist.
    pub courses: IndexMap<String, CourseInfo>,

    pub google_apis_client_id: String,
    pub google_apis_client_secret: EnvField<String>,

    pub slack_client_id: String,
    pub slack_client_secret: EnvField<String>,

    pub github_email_mapping_sheet_id: String,

    pub reviewer_staff_info_sheet_id: String,

    // Legacy hack until all trainees are in the sheet.
    pub extra_trainee_github_mappings: BTreeMap<GithubLogin, Trainee>,
}

#[derive(Clone, Deserialize)]
pub struct CourseInfo {
    pub register_sheet_id: String,
    pub batches: IndexMap<String, CourseSchedule>,
}

impl Config {
    pub fn get_course_schedule_with_register_sheet_id(
        &self,
        course_name: String,
        batch: &str,
    ) -> Option<CourseScheduleWithRegisterSheetId> {
        if let Some(course_info) = self.courses.get(&course_name) {
            if let Some(course_schedule) = course_info.batches.get(batch) {
                Some(CourseScheduleWithRegisterSheetId {
                    name: course_name,
                    course_schedule: course_schedule.clone(),
                    register_sheet_id: course_info.register_sheet_id.clone(),
                })
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn get_course_module_names(&self, course_name: &str) -> Option<Vec<String>> {
        if let Some(course_info) = self.courses.get(course_name) {
            if let Some((_batch_name, course_schedule)) = course_info.batches.get_index(0) {
                Some(course_schedule.sprints.keys().cloned().collect())
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct CourseSchedule {
    pub start: NaiveDate,
    pub end: NaiveDate,
    // Module -> [{region: Date}]
    pub sprints: IndexMap<String, Vec<BTreeMap<Region, NaiveDate>>>,
}

pub struct CourseScheduleWithRegisterSheetId {
    pub name: String,
    pub course_schedule: CourseSchedule,
    pub register_sheet_id: String,
}
