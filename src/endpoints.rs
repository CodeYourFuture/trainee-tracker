use std::{collections::BTreeMap, ops::AddAssign};

use ::octocrab::models::{Author, teams::RequestedTeam};
use anyhow::Context;
use axum::{
    Json,
    extract::{OriginalUri, Path, State},
    response::IntoResponse,
};
use chrono::Utc;
use futures::future::join_all;
use http::HeaderMap;
use indexmap::IndexMap;
use serde::Serialize;
use tower_sessions::Session;

use crate::{
    Error, ServerState,
    github_accounts::get_trainees,
    newtypes::GithubLogin,
    octocrab::{all_pages, octocrab},
    prs::{PrWithReviews, fill_in_reviewers, get_prs},
    register::{Attendance, get_register},
    sheets::sheets_client,
};

pub async fn health_check() -> impl IntoResponse {
    "ok"
}

pub async fn whoami_github(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<String, Error> {
    let user = octocrab(&session, &server_state, original_uri)
        .await?
        .current()
        .user()
        .await
        .context("Failed to get current user")?;
    Ok(format!("You are authenticated as {}", user.login))
}

#[derive(Serialize)]
pub struct GroupMetadata {
    name: String,
    slug: String,
}

#[derive(Serialize)]
pub struct Subgroups {
    groups: Vec<GroupMetadata>,
}

#[derive(Serialize)]
pub struct Courses {
    courses: IndexMap<String, Vec<String>>,
}

pub async fn courses(State(server_state): State<ServerState>) -> Json<Courses> {
    let courses = server_state
        .config
        .courses
        .into_iter()
        .filter_map(|(course_name, course_info)| {
            course_info
                .batches
                .get_index(0)
                .map(|(_batch_name, course_schedule)| {
                    (
                        course_name,
                        course_schedule.sprints.keys().cloned().collect::<Vec<_>>(),
                    )
                })
        })
        .collect();
    Json(Courses { courses })
}

pub async fn trainee_batches(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(course): Path<String>,
) -> Result<Json<Subgroups>, Error> {
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let results = all_pages("child teams", &octocrab, async || {
        octocrab
            .teams(server_state.config.github_org)
            .list_children(format!("{course}-trainees"))
            .send()
            .await
    })
    .await?;
    Ok(Json(Subgroups {
        groups: results
            .into_iter()
            .map(|RequestedTeam { name, slug, .. }| GroupMetadata { name, slug })
            .collect(),
    }))
}

#[derive(Serialize)]
pub struct Batch {
    trainees: Vec<String>,
}

pub async fn trainee_batch(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path((_course, batch)): Path<(String, String)>,
) -> Result<Json<Batch>, Error> {
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let trainees = all_pages("team members", &octocrab, async || {
        octocrab
            .teams(server_state.config.github_org)
            .members(batch)
            .send()
            .await
    })
    .await?
    .into_iter()
    .map(|Author { login, .. }| login)
    .collect();
    Ok(Json(Batch { trainees }))
}

pub async fn teams(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<String, Error> {
    let octocrab = octocrab(&session, &server_state, original_uri).await?;
    let results = all_pages("team members", &octocrab, async || {
        octocrab
            .teams("CodeYourFuture")
            .members("itp-mentors")
            .send()
            .await
    })
    .await?;
    let mut ret = String::new();
    for result in results {
        ret += &result.login;
        ret += "\n";
    }
    Ok(ret)
}

#[derive(Serialize)]
pub struct PrList {
    prs: Vec<PrWithReviews>,
}

pub async fn course_prs(
    session: Session,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(course): Path<String>,
) -> Result<Json<PrList>, Error> {
    let octocrab = octocrab(&session, &server_state, original_uri).await?;

    let mut futures = Vec::new();
    let course = server_state
        .config
        .courses
        .get(&course)
        .ok_or_else(|| Error::Fatal(anyhow::anyhow!("Course not found: {course}")))?;
    for module in course
        .batches
        .get_index(0)
        .iter()
        .flat_map(|(_batch_name, course_schedule)| course_schedule.sprints.keys().cloned())
    {
        let octocrab = octocrab.clone();
        let github_org = &server_state.config.github_org;
        futures.push(async move {
            let prs = get_prs(&octocrab, github_org, &module, true).await?;
            fill_in_reviewers(octocrab.clone(), github_org.to_owned(), prs).await
        });
    }
    let mut prs = Vec::new();
    for future in join_all(futures).await {
        prs.extend(future?)
    }
    Ok(Json(PrList { prs }))
}

#[derive(Serialize)]
pub struct Region {
    region: Option<crate::newtypes::Region>,
}

pub async fn get_region(
    session: Session,
    headers: HeaderMap,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(github_login): Path<String>,
) -> Result<Json<Region>, Error> {
    let sheets_client = sheets_client(
        &session,
        server_state.clone(),
        headers,
        original_uri.clone(),
    )
    .await?;
    let trainees = get_trainees(
        sheets_client,
        &server_state.config.github_email_mapping_sheet_id,
    )
    .await?;
    Ok(Json(Region {
        region: trainees
            .get(&GithubLogin::from(github_login))
            .map(|trainee| trainee.region.clone()),
    }))
}

#[derive(Serialize)]
pub struct AttendanceResponse {
    #[serde(flatten)]
    attendance: Attendance,
    sprint: String,
    module: String,
    batch: String,
}

pub async fn fetch_attendance(
    session: Session,
    headers: HeaderMap,
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
) -> Result<Json<Vec<AttendanceResponse>>, Error> {
    let all_courses = &server_state.config.courses;
    let sheets_client = sheets_client(
        &session,
        server_state.clone(),
        headers,
        original_uri.clone(),
    )
    .await?;

    let mut register_futures = Vec::new();
    for (course_name, course_info) in all_courses {
        for batch_name in course_info.batches.keys() {
            let course_schedule = server_state
                .config
                .get_course_schedule_with_register_sheet_id(course_name.clone(), batch_name)
                .ok_or_else(|| Error::Fatal(anyhow::anyhow!("Course not found: {course_name}")))?;
            let register_future = get_register(
                sheets_client.clone(),
                course_schedule.register_sheet_id.clone(),
                course_schedule.course_schedule.start,
                course_schedule.course_schedule.end,
            );
            register_futures.push(async move {
                (
                    course_name.clone(),
                    batch_name.clone(),
                    register_future.await,
                )
            });
        }
    }
    let register_info = join_all(register_futures).await;

    let mut registered_attendance = Vec::new();

    for (_course_name, batch_name, register_result) in register_info {
        let register = register_result?;
        for (module_name, sprint_info) in register.modules {
            for (sprint_number, attendance_info) in sprint_info.attendance.iter().enumerate() {
                let sprint_name = format!("Sprint-{}", sprint_number + 1);
                for attendance in attendance_info.values() {
                    registered_attendance.push(AttendanceResponse {
                        attendance: attendance.clone(),
                        sprint: sprint_name.clone(),
                        module: module_name.clone(),
                        batch: batch_name.clone(),
                    });
                }
            }
        }
    }
    Ok(Json(registered_attendance))
}

#[derive(Serialize)]
pub struct ExpectedAttendance {
    course: String,
    cohort: String,
    region: crate::newtypes::Region,
    expected_classes: usize,
}

pub async fn expected_attendance(
    State(server_state): State<ServerState>,
) -> Json<Vec<ExpectedAttendance>> {
    let now = Utc::now();

    let mut expected_attendance = Vec::new();
    for (course, course_info) in server_state.config.courses {
        for (cohort, schedule) in course_info.batches {
            let mut region_to_expected_classes: BTreeMap<crate::newtypes::Region, usize> =
                BTreeMap::new();
            for (_module_name, sprints) in schedule.sprints {
                for sprint in sprints {
                    for (region, date) in sprint {
                        let start_time = region.class_start_time(&date);
                        if start_time < now {
                            region_to_expected_classes
                                .entry(region)
                                .or_default()
                                .add_assign(1);
                        }
                    }
                }
            }
            for (region, expected_classes) in region_to_expected_classes {
                expected_attendance.push(ExpectedAttendance {
                    course: course.clone(),
                    cohort: cohort.clone(),
                    region,
                    expected_classes,
                })
            }
        }
    }
    Json(expected_attendance)
}
