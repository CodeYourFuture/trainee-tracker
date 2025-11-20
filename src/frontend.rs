use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use askama::Template;
use axum::{
    extract::{OriginalUri, Path, Query, State},
    response::{Html, IntoResponse, Response},
};
use futures::future::join_all;
use http::{HeaderMap, StatusCode, Uri, header::CONTENT_TYPE};
use serde::Deserialize;
use tower_sessions::Session;

use crate::{
    Error, ServerState,
    config::CourseScheduleWithRegisterSheetId,
    course::{
        Attendance, Batch, BatchMetadata, Course, Submission, TraineeStatus, fetch_batch_metadata,
        get_batch_with_submissions,
    },
    google_groups::{GoogleGroup, get_groups, groups_client},
    octocrab::octocrab,
    prs::{MaybeReviewerStaffOnlyDetails, PrState, ReviewerInfo},
    reviewer_staff_info::get_reviewer_staff_info,
    sheets::sheets_client,
    slack::list_groups_with_members,
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
            .map(|course_name| fetch_batch_metadata(&octocrab, github_org.clone(), course_name)),
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
    headers: HeaderMap,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path((course, batch_github_slug)): Path<(String, String)>,
) -> Result<Html<String>, Error> {
    let sheets_client = sheets_client(
        &session,
        server_state.clone(),
        headers,
        original_uri.clone(),
    )
    .await?;
    let github_org = &server_state.config.github_org;
    let course_schedule = server_state
        .config
        .get_course_schedule_with_register_sheet_id(course.clone(), &batch_github_slug)
        .ok_or_else(|| Error::Fatal(anyhow::anyhow!("Course not found: {course}")))?;
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let course = course_schedule
        .with_assignments(&octocrab, github_org)
        .await?;
    let mut batch = get_batch_with_submissions(
        &octocrab,
        sheets_client,
        &server_state.config.github_email_mapping_sheet_id,
        &server_state.config.mentoring_records_sheet_id,
        github_org,
        &batch_github_slug,
        &course,
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
    fn css_classes_for_submission(&self, submission: &Submission) -> String {
        match submission {
            Submission::Attendance(Attendance::Absent { .. }) => String::from("attendance-absent"),
            Submission::Attendance(Attendance::OnTime { .. }) => String::from("attendance-present"),
            Submission::Attendance(Attendance::Late { .. }) => String::from("attendance-late"),
            Submission::Attendance(Attendance::WrongDay { .. }) => {
                String::from("attendance-wrong-day")
            }
            Submission::PullRequest { pull_request, .. } => match pull_request.state {
                PrState::NeedsReview => "pr-needs-review".to_owned(),
                PrState::Reviewed => "pr-reviewed".to_owned(),
                PrState::Complete => "pr-complete".to_owned(),
                PrState::Unknown => "pr-unknown".to_owned(),
            },
        }
    }

    fn css_classes_for_trainee_status(&self, trainee_status: &TraineeStatus) -> String {
        match trainee_status {
            TraineeStatus::OnTrack => "trainee-on-track",
            TraineeStatus::Behind => "trainee-behind",
            TraineeStatus::AtRisk => "trainee-at-risk",
        }
        .to_owned()
    }

    fn on_track_and_total_for_region(&self, region: Option<&str>) -> (usize, usize) {
        let mut on_track = 0;
        let mut total = 0;
        for trainee in &self.batch.trainees {
            if let Some(region) = region {
                if trainee.trainee.region.as_str() != region {
                    continue;
                }
            }
            if trainee.status() == TraineeStatus::OnTrack {
                on_track += 1;
            }
            total += 1;
        }
        (on_track, total)
    }
}

pub async fn get_reviewers(
    session: Session,
    headers: HeaderMap,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(course): Path<String>,
) -> Result<Html<String>, Error> {
    let sheets_client = sheets_client(
        &session,
        server_state.clone(),
        headers,
        original_uri.clone(),
    )
    .await?;
    let mut is_staff = true;
    let mut staff_details = get_reviewer_staff_info(
        sheets_client,
        &server_state.config.reviewer_staff_info_sheet_id,
    )
    .await
    .or_else(|err| match err {
        Error::PotentiallyIgnorablePermissions(_) => {
            is_staff = false;
            Ok(BTreeMap::new())
        }
        err => Err(err),
    })?;

    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let github_org = &server_state.config.github_org;
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

pub async fn index() -> Html<String> {
    Html(Index {}.render().unwrap())
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index {}

#[derive(Template)]
#[template(path = "redirect.html")]
pub(crate) struct Redirect {
    pub redirect_uri: Uri,
}

#[derive(Template)]
#[template(path = "google-groups.html")]
struct GoogleGroups {
    pub groups: BTreeSet<GoogleGroup>,
}

#[derive(Deserialize)]
pub struct GroupListParams {
    #[serde(default)]
    expand: bool,
}

pub async fn list_google_groups(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Query(params): Query<GroupListParams>,
) -> Result<Html<String>, Error> {
    let client = groups_client(&session, server_state, original_uri).await?;
    let mut groups = get_groups(&client).await?;
    if params.expand {
        groups
            .expand_recursively()
            .context("Failed to expand groups recursively")?;
    }
    Ok(Html(
        GoogleGroups {
            groups: groups.groups,
        }
        .render()
        .unwrap(),
    ))
}

pub async fn list_google_groups_csv(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Query(params): Query<GroupListParams>,
) -> Result<Csv, Error> {
    let client = groups_client(&session, server_state, original_uri).await?;
    let mut groups = get_groups(&client).await?;
    if params.expand {
        groups
            .expand_recursively()
            .context("Failed to expand groups recursively")?;
    }

    let member_count = groups
        .groups
        .iter()
        .map(|group| group.members.len())
        .max()
        .unwrap_or(0);

    // Manually writing a CSV because the CSV crate doesn't like different numbers of fields per record.
    let mut out = String::new();
    out += "group";
    for i in 0..member_count {
        out += &format!(",member{}", i + 1);
    }
    out += "\n";

    for group in groups.groups {
        out += group.email.as_str();
        for member in group.members {
            out += ",";
            out += member.as_str();
        }
        out += "\n"
    }
    Ok(Csv(out))
}

pub struct Csv(String);

impl IntoResponse for Csv {
    fn into_response(self) -> axum::response::Response {
        Response::builder()
            .header(CONTENT_TYPE, "text/csv")
            .status(StatusCode::OK)
            .body(axum::body::Body::from(self.0))
            .expect("Failed to build response")
    }
}

pub async fn list_slack_groups_csv(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<Csv, Error> {
    let client = crate::slack::slack_client(&session, server_state, original_uri).await?;
    let groups = list_groups_with_members(client).await?;

    let member_count = groups
        .iter()
        .map(|group| group.members.len())
        .max()
        .unwrap_or(0);

    let mut out = String::new();
    out += "id,handle,name";
    for i in 0..member_count {
        out += &format!(",member{}email,member{}name", i + 1, i + 1);
    }
    out += "\n";

    for group in groups {
        out += group.id.as_str();
        out += ",";
        out += &group.handle;
        out += ",";
        out += &group.name;
        for member in group.members {
            out += ",";
            out += &member
                .profile
                .email
                .map_or_else(|| "unknown".to_owned(), |email| email.to_string());
            out += ",";
            out += &member.real_name;
        }
        out += "\n"
    }
    Ok(Csv(out))
}
