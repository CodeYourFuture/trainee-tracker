use ::octocrab::models::{teams::RequestedTeam, Author};
use anyhow::Context;
use axum::{
    extract::{OriginalUri, Path, State},
    response::IntoResponse,
    Json,
};
use futures::future::join_all;
use indexmap::IndexMap;
use serde::Serialize;
use tower_sessions::Session;

use crate::{
    github_accounts::get_trainees,
    newtypes::GithubLogin,
    octocrab::{all_pages, octocrab},
    prs::{fill_in_reviewers, get_prs, PrWithReviews},
    sheets::sheets_client,
    Error, ServerState,
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
        let github_org = server_state.config.github_org.clone();
        futures.push(async move {
            let prs = get_prs(&octocrab, github_org.clone(), module.clone(), true).await?;
            fill_in_reviewers(octocrab.clone(), github_org, prs).await
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
    State(server_state): State<ServerState>,
    OriginalUri(original_uri): OriginalUri,
    Path(github_login): Path<String>,
) -> Result<Json<Region>, Error> {
    let sheets_client = sheets_client(&session, server_state.clone(), original_uri.clone()).await?;
    let trainees = get_trainees(
        sheets_client,
        &server_state.config.github_email_mapping_sheet_id,
        server_state.config.extra_trainee_github_mappings,
    )
    .await?;
    Ok(Json(Region {
        region: trainees
            .get(&GithubLogin::from(github_login))
            .map(|trainee| trainee.region.clone()),
    }))
}
