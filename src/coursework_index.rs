use std::collections::BTreeMap;

use anyhow::Context;
use serde::Deserialize;

use crate::{Error, newtypes::TaskId};

pub async fn fetch() -> Result<BTreeMap<TaskId, CourseworkTask>, Error> {
    let index: CourseworkIndex =
        reqwest::get("https://portal-backend.codeyourfuture.io/functions/v1/coursework-index")
            .await
            .context("Failed to get courswork index")?
            .json()
            .await
            .context("Failed to deserialize coursework index")?;
    let mut map = BTreeMap::new();
    for course in index.courses.into_values() {
        for module in course.modules {
            for sprint in module.sprints {
                for task in sprint.backlog_tasks {
                    map.insert(task.code.clone(), task);
                }
            }
        }
    }
    Ok(map)
}

#[derive(Deserialize)]
pub struct CourseworkIndex {
    pub courses: BTreeMap<String, CourseworkCourse>,
}

#[derive(Deserialize)]
pub struct CourseworkCourse {
    pub modules: Vec<CourseworkModule>,
}

#[derive(Deserialize)]
pub struct CourseworkModule {
    pub sprints: Vec<CourseworkSprint>,
}

#[derive(Deserialize)]
pub struct CourseworkSprint {
    pub backlog_tasks: Vec<CourseworkTask>,
}

#[derive(Deserialize)]
pub struct CourseworkTask {
    pub code: TaskId,
    // TODO: This should really be an Option<Regex> but https://github.com/serde-rs/serde/issues/723 is fiddly.
    pub change_dir: Option<String>,
}
