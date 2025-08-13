use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use askama::Template;
use axum::{
    extract::{OriginalUri, Path, Query, State},
    response::{Html, IntoResponse, Response},
};
use futures::future::{join, join_all};
use http::{header::CONTENT_TYPE, StatusCode, Uri};
use octocrab::Octocrab;
use serde::Deserialize;
use tower_sessions::Session;

use crate::{
    config::CourseScheduleWithRegisterSheetId,
    course::{
        fetch_batch_metadata, get_batch_with_submissions, Attendance, Batch, BatchMetadata, Course,
        Submission, TraineeStatus,
    },
    google_groups::{get_groups, groups_client, GoogleGroup},
    octocrab::{all_pages, octocrab},
    prs::{MaybeReviewerStaffOnlyDetails, PrState, ReviewerInfo},
    reviewer_staff_info::get_reviewer_staff_info,
    sheets::sheets_client,
    slack::list_groups_with_members,
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

    let is_staff = is_staff(&octocrab, &server_state.config.github_org)
        .await
        .unwrap_or(false);

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
            is_staff,
        }
        .render()
        .unwrap(),
    ))
}

async fn is_staff(octocrab: &Octocrab, github_org: &str) -> Result<bool, Error> {
    let team_future = all_pages("staff team members", octocrab, async || {
        octocrab
            .teams(github_org)
            .members("cyf-staff-team")
            .send()
            .await
    });
    let current = octocrab.current();
    let self_future = current.user();
    let (team_members, user) = join(team_future, self_future).await;
    let team_members = team_members?;
    let user = user.context("Failed to get current user")?;
    for team_member in team_members {
        if team_member.login == user.login {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Template)]
#[template(path = "list-courses.html")]
struct ListCoursesTemplate {
    pub courses_with_batch_metadata: Vec<CourseScheduleWithBatchMetadata>,
    pub is_staff: bool,
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
    let mut batch = get_batch_with_submissions(
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
    fn css_classes_for_submission(&self, submission: &Submission) -> String {
        match submission {
            Submission::Attendance(Attendance::Absent { .. }) => String::from("attendance-absent"),
            Submission::Attendance(Attendance::OnTime { .. }) => String::from("attendance-present"),
            Submission::Attendance(Attendance::Late { .. }) => String::from("attendance-late"),
            Submission::Attendance(Attendance::WrongDay { .. }) => {
                String::from("attendance-wrong-day")
            }
            Submission::PullRequest { pull_request } => match pull_request.state {
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
