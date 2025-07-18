use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use anyhow::Context;
use askama::Template;
use axum::{
    extract::{OriginalUri, Path, Query, State},
    response::Html,
};
use email_address::EmailAddress;
use futures::future::join_all;
use gsuite_api::{
    types::{Group, Member},
    Response,
};
use http::Uri;
use serde::Deserialize;
use tower_sessions::Session;

use crate::{
    auth::github_auth_redirect_url,
    config::CourseScheduleWithRegisterSheetId,
    course::{
        fetch_batch_metadata, get_batch, Attendance, Batch, BatchMetadata, Course, Submission,
    },
    google_groups::{groups_client, GoogleGroup},
    octocrab::octocrab,
    prs::{MaybeReviewerStaffOnlyDetails, PrState, ReviewerInfo},
    reviewer_staff_info::get_reviewer_staff_info,
    sheets::sheets_client,
    Error, ServerState,
};

pub async fn list_courses(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<Html<String>, Error> {
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let courses = &server_state.config.courses;
    let github_org = server_state.config.github_org.clone();
    let batch_metadata = join_all(
        courses
            .keys()
            .map(|course_name| fetch_batch_metadata(&octocrab, github_org.clone(), &course_name)),
    )
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    let courses_with_batch_metadata = courses
        .keys()
        .zip(batch_metadata)
        .filter_map(|(course_name, batch_metadata)| {
            server_state
                .config
                .courses
                .get(course_name)
                .and_then(|course| {
                    course
                        .batches
                        .get_index(0)
                        .map(
                            |(_batch_name, course_schedule)| CourseScheduleWithBatchMetadata {
                                course: CourseScheduleWithRegisterSheetId {
                                    course_schedule: course_schedule.clone(),
                                    name: course_name.clone(),
                                    register_sheet_id: course.register_sheet_id.clone(),
                                },
                                batch_metadata: batch_metadata
                                    .into_iter()
                                    .filter(|batch| {
                                        course.batches.contains_key(&batch.github_team_slug)
                                    })
                                    .collect(),
                            },
                        )
                })
        })
        .collect();
    Ok(Html(
        ListCoursesTemplate {
            courses_with_batch_metadata,
        }
        .render()
        .unwrap(),
    ))
}

#[derive(Template)]
#[template(path = "list-courses.html")]
struct ListCoursesTemplate {
    pub courses_with_batch_metadata: Vec<CourseScheduleWithBatchMetadata>,
}

struct CourseScheduleWithBatchMetadata {
    pub course: CourseScheduleWithRegisterSheetId,
    pub batch_metadata: Vec<BatchMetadata>,
}

pub async fn get_trainee_batch(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path((course, batch_github_slug)): Path<(String, String)>,
) -> Result<Html<String>, Error> {
    let sheets_client = sheets_client(&session, server_state.clone(), original_uri.clone()).await?;
    let github_org = server_state.config.github_org.clone();
    let course_schedule = server_state
        .config
        .get_course_schedule_with_register_sheet_id(course.clone(), &batch_github_slug)
        .ok_or_else(|| Error::Fatal(anyhow::anyhow!("Course not found: {course}")))?;
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let course = course_schedule
        .with_assignments(&octocrab, github_org.clone())
        .await?;
    let mut batch = get_batch(
        &octocrab,
        sheets_client,
        &server_state.config.github_email_mapping_sheet_id,
        github_org,
        batch_github_slug,
        &course,
        server_state.config.extra_trainee_github_mappings,
    )
    .await?;
    batch
        .trainees
        .sort_by_cached_key(|trainee| trainee.progress_score());
    batch.trainees.reverse();
    Ok(Html(
        TraineeBatchTemplate { course, batch }.render().unwrap(),
    ))
}

#[derive(Template)]
#[template(path = "trainee-batch.html")]
struct TraineeBatchTemplate {
    course: Course,
    batch: Batch,
}

impl TraineeBatchTemplate {
    fn css_classes(&self, submission: &Submission) -> String {
        match submission {
            Submission::Attendance(Attendance::Absent { .. }) => String::from("attendance-absent"),
            Submission::Attendance(Attendance::OnTime { .. }) => String::from("attendance-present"),
            Submission::Attendance(Attendance::Late { .. }) => String::from("attendance-late"),
            Submission::PullRequest { pull_request } => match pull_request.state {
                PrState::NeedsReview => "pr-needs-review".to_owned(),
                PrState::Reviewed => "pr-reviewed".to_owned(),
                PrState::Complete => "pr-complete".to_owned(),
                PrState::Unknown => "pr-unknown".to_owned(),
            },
        }
    }
}

#[derive(Deserialize)]
pub struct ReviewerParams {
    staff: Option<bool>,
}

pub async fn get_reviewers(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(course): Path<String>,
    Query(reviewer_params): Query<ReviewerParams>,
) -> Result<Html<String>, Error> {
    let is_staff = reviewer_params.staff.unwrap_or(false);
    let mut staff_details = if is_staff {
        let sheets_client =
            sheets_client(&session, server_state.clone(), original_uri.clone()).await?;
        get_reviewer_staff_info(
            sheets_client,
            &server_state.config.reviewer_staff_info_sheet_id,
        )
        .await?
    } else {
        BTreeMap::new()
    };

    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let github_org = server_state.config.github_org.clone();
    let module_names = server_state
        .config
        .get_course_module_names(&course)
        .ok_or_else(|| Error::Fatal(anyhow::anyhow!("Course not found: {course}")))?;
    let reviewers = crate::prs::get_reviewers(octocrab, github_org, &module_names)
        .await?
        .into_iter()
        .map(|mut reviewer| {
            reviewer.staff_only_details = if is_staff {
                match staff_details.remove(&reviewer.login) {
                    Some(details) => MaybeReviewerStaffOnlyDetails::Some(details),
                    None => MaybeReviewerStaffOnlyDetails::Unknown,
                }
            } else {
                MaybeReviewerStaffOnlyDetails::NotAuthenticated
            };
            reviewer
        })
        .collect();

    let now = chrono::Utc::now();

    Ok(Html(
        ReviewersTemplate {
            course,
            reviewers,
            now,
        }
        .render()
        .unwrap(),
    ))
}

#[derive(Template)]
#[template(path = "reviewers.html")]
struct ReviewersTemplate {
    pub course: String,
    pub reviewers: BTreeSet<ReviewerInfo>,
    pub now: chrono::DateTime<chrono::Utc>,
}

pub async fn index(
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<Html<String>, Error> {
    let login_url = github_auth_redirect_url(&server_state, original_uri).await?;
    Ok(Html(Index { login_url }.render().unwrap()))
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index {
    pub login_url: Uri,
}

#[derive(Template)]
#[template(path = "redirect.html")]
pub(crate) struct Redirect {
    pub redirect_uri: Uri,
}

#[derive(Template)]
#[template(path = "google-groups.html")]
struct GoogleGroups {
    pub groups: Vec<GoogleGroup>,
}

pub async fn list_google_groups(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<Html<String>, Error> {
    let client = groups_client(&session, server_state, original_uri).await?;
    let groups_response = client
        .groups()
        .list_all(
            "my_customer",
            "codeyourfuture.io",
            gsuite_api::types::DirectoryGroupsListOrderBy::Email,
            "",
            gsuite_api::types::SortOrder::Ascending,
            "",
        )
        .await
        .context("Failed to list Google groups")?;
    let groups = error_for_status(groups_response)?;
    let group_member_futures = groups
        .iter()
        .map(|Group { id, .. }| async { client.members().list_all(id, false, "").await })
        .collect::<Vec<_>>();
    let group_members = join_all(group_member_futures).await;

    let result = groups
        .into_iter()
        .zip(group_members.into_iter())
        .map(|(group, members)| {
            let members =
                error_for_status(members.context("Failed to list Google group members")?)?;
            Ok(GoogleGroup {
                email: EmailAddress::from_str(&group.email).with_context(|| {
                    format!("Failed to parse group email address {}", group.email)
                })?,
                members: members
                    .into_iter()
                    .map(|Member { email, .. }| {
                        Ok(EmailAddress::from_str(&email).with_context(|| {
                            format!(
                                "Failed to parse group member email address {} (member of {})",
                                email, group.email
                            )
                        })?)
                    })
                    .collect::<Result<_, anyhow::Error>>()?,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(Html(GoogleGroups { groups: result }.render().unwrap()))
}

fn error_for_status<T: std::fmt::Debug>(response: Response<T>) -> Result<T, Error> {
    if !response.status.is_success() {
        Err(Error::Fatal(anyhow::anyhow!(
            "Got bad response from Google Groups API: {:?}",
            response
        )))
    } else {
        Ok(response.body)
    }
}
