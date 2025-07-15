use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    str::FromStr,
};

use crate::{
    config::CourseScheduleWithRegisterSheetId,
    github_accounts::{get_trainees, Trainee},
    newtypes::{Email, GithubLogin},
    octocrab::all_pages,
    prs::{get_prs, Pr},
    register::{get_register, Register},
    sheets::SheetsClient,
    Error,
};
use anyhow::Context;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use futures::future::join_all;
use indexmap::{IndexMap, IndexSet};
use octocrab::{
    models::{issues::Issue, teams::RequestedTeam, Author},
    Octocrab,
};
use regex::Regex;
use serde::Serialize;

impl CourseScheduleWithRegisterSheetId {
    pub fn module_names(&self) -> Vec<String> {
        self.course_schedule.sprints.keys().cloned().collect()
    }

    pub async fn with_assignments(
        &self,
        octocrab: &Octocrab,
        github_org: String,
    ) -> Result<Course, Error> {
        let mut modules = IndexMap::new();
        let mut module_futures = Vec::new();

        for (module_name, module_sprint_dates) in &self.course_schedule.sprints {
            modules.insert(
                module_name.clone(),
                Module {
                    sprints: module_sprint_dates
                        .into_iter()
                        .map(|date| Sprint {
                            assignments: vec![Assignment::Attendance {
                                class_date: date.clone(),
                            }],
                            date: date.clone(),
                        })
                        .collect(),
                },
            );
            module_futures.push(Self::fetch_module_assignments(
                octocrab,
                github_org.clone(),
                module_name.clone(),
                module_sprint_dates.len(),
            ));
        }

        for (module_name, sprints_module_assignments) in self
            .module_names()
            .into_iter()
            .zip(join_all(module_futures).await.into_iter())
        {
            for (module_sprint, module_assignments) in
                modules[&module_name]
                    .sprints
                    .iter_mut()
                    .zip(sprints_module_assignments.map_err(|err| {
                        err.with_context(|| {
                            format!("Failed to fetch issues for module {}", module_name)
                        })
                    })?)
            {
                module_sprint.assignments.extend(module_assignments);
            }
        }

        Ok(Course {
            name: self.name.clone(),
            modules,
            register_sheet_id: self.register_sheet_id.clone(),
            start_date: self.course_schedule.start,
            end_date: self.course_schedule.end,
        })
    }

    pub async fn fetch_module_assignments(
        octocrab: &Octocrab,
        github_org: String,
        module_name: String,
        sprint_count: usize,
    ) -> Result<Vec<Vec<Assignment>>, Error> {
        let mut sprints = std::iter::repeat_with(Vec::new)
            .take(sprint_count)
            .collect::<Vec<_>>();

        let mut issues = all_pages("issues", octocrab, async || {
            octocrab.issues(github_org, module_name).list().send().await
        })
        .await
        .map_err(|err| err.context("Failed to fetch module issues"))?;

        issues.sort_by_cached_key(|&Issue { ref title, .. }| title.clone());

        for issue in issues {
            if let Some((sprint_number, assignment)) = parse_issue(&issue)? {
                let sprint_index = usize::from(sprint_number) - 1;
                if sprints.len() <= sprint_index {
                    return Err(Error::Fatal(anyhow::anyhow!(
                        "Found issue {} in sprint {} but module only has {} sprints",
                        issue.html_url,
                        sprint_number,
                        sprints.len()
                    )));
                }
                if let Some(assignment) = assignment {
                    sprints[sprint_index].push(assignment);
                }
            }
        }
        Ok(sprints)
    }
}

fn parse_issue(issue: &Issue) -> Result<Option<(NonZeroUsize, Option<Assignment>)>, Error> {
    if issue.pull_request.is_some() {
        return Ok(None);
    }

    let Issue {
        labels,
        title,
        html_url,
        ..
    } = issue;

    let mut sprint = None;
    let mut assignment = None;
    for label in labels {
        if let Some(sprint_number) = label.name.strip_prefix("📅 Sprint ") {
            if sprint.is_some() {
                return Err(Error::UserFacing(format!(
                    "Failed to parse issue {} - duplicate sprint labels",
                    html_url
                )));
            }
            match NonZeroUsize::from_str(sprint_number) {
                Ok(sprint_number) => {
                    sprint = Some(sprint_number);
                }
                Err(_err) => {
                    return Err(Error::UserFacing(format!(
                        "Failed to parse issue {} - sprint label wasn't (non-zero) number: {}",
                        html_url, label.name
                    )))
                }
            }
        }
        if let Some(submit_label) = label.name.strip_prefix("Submit:") {
            if assignment.is_some() {
                return Err(Error::UserFacing(format!(
                    "Failed to parse issue {} - duplicate submit labels",
                    html_url
                )));
            }
            match submit_label {
                "None" => {
                    assignment = Some(None);
                }
                "PR" => {
                    assignment = Some(Some(Assignment::ExpectedPullRequest {
                        title: title.clone(),
                    }));
                }
                "Issue" => {
                    // TODO: Handle these.
                    assignment = Some(None);
                }
                other => {
                    return Err(Error::UserFacing(format!(
                        "Failed to parse issue {} - submit label wasn't recognised: {}",
                        html_url, other
                    )));
                }
            }
        }
    }
    let sprint = sprint.ok_or_else(|| {
        Error::UserFacing(format!(
            "Failed to parse issue {} - no sprint label",
            html_url
        ))
    })?;
    // TODO
    // let assignment = assignment.ok_or_else(|| Error::UserFacing(format!("Failed to parse issue {} - no submit label", html_url)))?;
    let assignment = assignment.or_else(|| Some(None)).unwrap();
    Ok(Some((sprint, assignment)))
}

#[derive(Serialize)]
pub struct Course {
    pub name: String,
    pub modules: IndexMap<String, Module>,
    pub register_sheet_id: String,

    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Serialize)]
pub struct Module {
    pub sprints: Vec<Sprint>,
}

impl Module {
    pub fn assignment_count(&self) -> usize {
        self.sprints
            .iter()
            .map(|sprint| sprint.assignment_count())
            .sum()
    }
}

#[derive(Clone, Serialize)]
pub struct Sprint {
    pub assignments: Vec<Assignment>,
    pub date: NaiveDate,
}

impl Sprint {
    pub fn assignment_count(&self) -> usize {
        self.assignments.len()
    }

    pub fn is_in_past(&self) -> bool {
        // TODO: Handle time zones
        self.date <= Utc::now().date_naive()
    }
}

#[derive(Clone, Debug, Serialize)]
pub enum Assignment {
    Attendance { class_date: chrono::NaiveDate },
    ExpectedPullRequest { title: String },
}

impl Assignment {
    pub fn heading(&self) -> String {
        match self {
            Assignment::Attendance { class_date } => format!("Attendance {class_date}"),
            Assignment::ExpectedPullRequest { title } => format!("PR: {title}"),
        }
    }
}

#[derive(Debug)]
pub struct Batch {
    pub name: String,
    pub trainees: Vec<TraineeWithSubmissions>,
}

impl Batch {
    pub fn unknown_prs(&self) -> Vec<Pr> {
        self.trainees
            .iter()
            .flat_map(|TraineeWithSubmissions { modules, .. }| modules.values())
            .flat_map(|ModuleWithSubmissions { unknown_prs, .. }| unknown_prs.iter().cloned())
            .collect()
    }
}

#[derive(Debug)]
pub struct TraineeWithSubmissions {
    pub github_login: GithubLogin,
    pub name: String,
    pub region: String,
    pub modules: IndexMap<String, ModuleWithSubmissions>,
}

#[derive(Debug)]
pub struct ModuleWithSubmissions {
    pub sprints: Vec<SprintWithSubmissions>,
    pub unknown_prs: Vec<Pr>,
}

#[derive(Debug)]
pub struct SprintWithSubmissions {
    pub submissions: Vec<SubmissionState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmissionState {
    Some(Submission),
    MissingButExpected,
    MissingButNotExpected,
}

impl SubmissionState {
    fn is_missing(&self) -> bool {
        match self {
            Self::Some(_) => false,
            Self::MissingButExpected => true,
            Self::MissingButNotExpected => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Submission {
    Attendance(Attendance),
    PullRequest { pull_request: Pr },
}

impl Submission {
    pub fn display_text(&self) -> String {
        match self {
            Self::Attendance(Attendance::Absent { .. }) => String::from("Absent"),
            Self::Attendance(Attendance::OnTime { .. }) => String::from("On time"),
            Self::Attendance(Attendance::Late { .. }) => String::from("Late"),
            Self::PullRequest { pull_request } => format!("#{}", pull_request.number),
        }
    }

    pub fn link(&self) -> String {
        match self {
            Self::Attendance(attendance) => attendance.register_url().to_owned(),
            Self::PullRequest { pull_request } => pull_request.url.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attendance {
    Absent { register_url: String },
    OnTime { register_url: String },
    Late { register_url: String },
}

impl Attendance {
    pub fn register_url(&self) -> &str {
        match self {
            Attendance::Absent { register_url } => &register_url,
            Attendance::OnTime { register_url } => &register_url,
            Attendance::Late { register_url } => &register_url,
        }
    }
}

pub(crate) struct BatchMetadata {
    pub name: String,
    pub github_team_slug: String,
}

pub(crate) async fn fetch_batch_metadata(
    octocrab: &Octocrab,
    github_org: String,
    course_name: &str,
) -> Result<Vec<BatchMetadata>, Error> {
    let teams = all_pages("teams", octocrab, async || {
        octocrab
            .teams(github_org)
            .list_children(&format!("{}-trainees", course_name))
            .send()
            .await
    })
    .await?;
    Ok(teams
        .into_iter()
        .map(|RequestedTeam { slug, name, .. }| BatchMetadata {
            name,
            github_team_slug: slug,
        })
        .collect())
}

pub async fn get_batch(
    octocrab: &Octocrab,
    sheets_client: SheetsClient,
    github_email_mapping_sheet_id: &str,
    github_org: String,
    batch_github_slug: String,
    course: &Course,
    extra_trainees: BTreeMap<GithubLogin, Trainee>,
) -> Result<Batch, Error> {
    let trainee_info = get_trainees(
        sheets_client.clone(),
        github_email_mapping_sheet_id,
        extra_trainees,
    )
    .await?;

    let register_info = get_register(
        sheets_client,
        course.register_sheet_id.clone(),
        course.start_date,
        course.end_date,
    )
    .await?;

    let team = octocrab
        .teams(github_org.clone())
        .get(batch_github_slug.clone())
        .await
        .context("Failed to get team")?;
    let name = team.name;

    let members = all_pages("members", octocrab, async || {
        octocrab
            .teams(github_org.clone())
            .members(batch_github_slug)
            .send()
            .await
    })
    .await?;

    let member_logins = members
        .iter()
        .map(|Author { login, .. }| GithubLogin(login.clone()))
        .collect::<BTreeSet<_>>();

    let pr_futures = course
        .modules
        .keys()
        .map(|module| get_prs(octocrab, github_org.clone(), module.clone()))
        .collect::<Vec<_>>();
    let prs_by_module = join_all(pr_futures)
        .await
        .into_iter()
        .collect::<Result<Vec<Vec<Pr>>, Error>>()?;
    let mut member_to_module_to_prs = BTreeMap::new();
    for member in &member_logins {
        let mut module_to_prs = IndexMap::new();
        for module in course.modules.keys() {
            module_to_prs.insert(module, Vec::new());
        }
        member_to_module_to_prs.insert(member.clone(), module_to_prs);
    }
    for (module_name, prs) in course.modules.keys().zip(prs_by_module) {
        for pr in prs {
            let author = pr.author.clone();
            if member_logins.contains(&author) {
                member_to_module_to_prs
                    .get_mut(&author)
                    // UNWRAP: By construction above.
                    .unwrap()
                    .get_mut(module_name)
                    // UNWRAP: By construction above.
                    .unwrap()
                    .push(pr);
            }
        }
    }
    let mut trainees = Vec::with_capacity(member_logins.len());
    for (github_login, module_to_prs) in member_to_module_to_prs {
        let trainee_specific_info = trainee_info.get(&github_login);
        let trainee_name =
            trainee_specific_info.map_or_else(|| "unknown".to_owned(), |t| t.name.clone());
        let trainee_email = trainee_specific_info.map(|t| t.email.clone());
        let region =
            trainee_specific_info.map_or_else(|| "unknown".to_owned(), |t| t.region.clone());

        let mut modules = IndexMap::new();
        for (module_name, module) in &course.modules {
            let module_attendance = get_trainee_module_attendance(
                &register_info,
                module_name,
                trainee_email.clone(),
                &course,
            )?;
            let module_with_submissions = match_prs_to_assignments(
                &module,
                module_to_prs[&module_name].clone(),
                module_attendance,
            )
            .map_err(|err| err.context("Failed to match PRs to assignments"))?;

            modules.insert(module_name.clone(), module_with_submissions);
        }
        let trainee = TraineeWithSubmissions {
            github_login,
            name: trainee_name,
            region,
            modules,
        };
        trainees.push(trainee);
    }

    Ok(Batch { name, trainees })
}

fn get_trainee_module_attendance(
    register_info: &Register,
    module_name: &str,
    trainee_email: Option<Email>,
    course: &Course,
) -> Result<Vec<SubmissionState>, Error> {
    if let Some(ref trainee_email) = trainee_email {
        let module_attendance = register_info.modules.get(module_name).with_context(|| {
            format!(
                "Register contained no attendance for module {}",
                module_name
            )
        })?;
        let sprints = course
            .modules
            .get(module_name)
            .map(|module| module.sprints.clone())
            .ok_or_else(|| anyhow::anyhow!("Tried to get trainee module attendance for course {} module {} which doesn't seem to exist", course.name, module_name))?;
        let result = sprints
            .iter()
            .enumerate()
            .map(|(sprint_index, sprint)| {
                let dates = sprint
                    .assignments
                    .iter()
                    .filter_map(|assignment| {
                        if let Assignment::Attendance { class_date } = assignment {
                            Some(class_date.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<chrono::NaiveDate>>();
                let attendance = match dates.as_slice() {
                    [date] => {
                        // TODO: Handle Cape Town
                        let start_time = DateTime::<Tz>::from_naive_utc_and_offset(
                            NaiveDateTime::new(
                                date.clone(),
                                NaiveTime::from_hms_opt(10, 00, 00).expect("TODO"),
                            ),
                            chrono_tz::Europe::London.offset_from_utc_date(date),
                        )
                        .to_utc();
                        let attendance = module_attendance
                            .attendance
                            .get(sprint_index)
                            .and_then(|attendance| attendance.get(trainee_email))
                            .map(|a| a.to_attendance_enum(start_time));
                        match attendance {
                            Some(attendance) => {
                                SubmissionState::Some(Submission::Attendance(attendance))
                            }
                            None => {
                                if sprint.is_in_past() {
                                    SubmissionState::Some(Submission::Attendance(
                                        Attendance::Absent {
                                            register_url: module_attendance.register_url.clone(),
                                        },
                                    ))
                                } else {
                                    SubmissionState::MissingButNotExpected
                                }
                            }
                        }
                    }
                    _ => SubmissionState::MissingButNotExpected,
                };
                attendance
            })
            .collect();
        Ok(result)
    } else {
        Ok(Vec::new())
    }
}

pub fn match_prs_to_assignments(
    module: &Module,
    prs: Vec<Pr>,
    attendance: Vec<SubmissionState>,
) -> Result<ModuleWithSubmissions, Error> {
    let mut sprints = Vec::with_capacity(module.sprints.len());
    for (sprint_index, sprint) in module.sprints.iter().enumerate() {
        sprints.push(SprintWithSubmissions {
            submissions: vec![
                if sprint.is_in_past() {
                    SubmissionState::MissingButExpected
                } else {
                    SubmissionState::MissingButNotExpected
                };
                sprint.assignment_count()
            ],
        });

        for (assignment_index, assignment) in sprint.assignments.iter().enumerate() {
            if let Assignment::Attendance {
                class_date: _class_date,
            } = assignment
            {
                if let Some(submission_state) = attendance.get(sprint_index) {
                    sprints[sprint_index].submissions[assignment_index] = submission_state.clone();
                }
            }
        }
    }

    let mut unknown_prs = Vec::new();
    for pr in prs {
        let title_lower = pr.title.to_lowercase();
        let title_parts = title_lower
            .split("|")
            .map(|title| title.trim())
            .collect::<Vec<_>>();
        let mut sprint_index = None;
        for title_part in title_parts {
            if title_part.starts_with("sprint") || title_part.starts_with("week") {
                let number_regex = Regex::new(r"(\d+)").unwrap();
                if let Some(number_match) = number_regex
                    .captures(title_part)
                    .and_then(|captures| captures.get(1))
                {
                    let number_str = number_match.as_str();
                    let number = usize::from_str(number_str)
                        .with_context(|| format!("Failed to parse '{}' as number", number_str))?;
                    if number == 0 || number > 20 {
                        return Err(Error::Fatal(anyhow::anyhow!("Sprint number was impractical - expected something between 1 and 20 but was {}", number)));
                    }

                    sprint_index = Some(number - 1);
                }
            }
        }
        match_pr_to_assignment(
            pr,
            sprint_index,
            &module.sprints,
            &mut sprints,
            &mut unknown_prs,
        );
    }

    Ok(ModuleWithSubmissions {
        sprints,
        unknown_prs,
    })
}

fn match_pr_to_assignment(
    pr: Pr,
    claimed_sprint_index: Option<usize>,
    assignments: &Vec<Sprint>,
    submissions: &mut Vec<SprintWithSubmissions>,
    unknown_prs: &mut Vec<Pr>,
) {
    #[derive(Clone, Copy)]
    struct Match {
        match_count: usize,
        sprint_index: usize,
        assignment_index: usize,
    }

    let mut best_match: Option<Match> = None;
    for (sprint_index, sprint) in assignments.iter().enumerate() {
        if let Some(claimed_sprint_index) = claimed_sprint_index {
            if claimed_sprint_index != sprint_index {
                continue;
            }
        }
        let mut pr_title_words = title_word_set(&pr.title);
        if let Some(claimed_sprint_index) = claimed_sprint_index {
            let claimed_sprint_number = claimed_sprint_index + 1;
            pr_title_words.insert(format!("sprint{}", claimed_sprint_number));
        }

        for (assignment_index, assignment) in sprint.assignments.iter().enumerate() {
            match assignment {
                Assignment::ExpectedPullRequest {
                    title: expected_title,
                } => {
                    let mut assignment_title_words = title_word_set(expected_title);
                    if let Some(claimed_sprint_index) = claimed_sprint_index {
                        let claimed_sprint_number = claimed_sprint_index + 1;
                        if assignment_title_words.contains("sprint") {
                            assignment_title_words
                                .insert(format!("sprint{}", claimed_sprint_number));
                            assignment_title_words.insert(format!("week{}", claimed_sprint_number));
                        }
                    }
                    let match_count = assignment_title_words.intersection(&pr_title_words).count();
                    if submissions[sprint_index].submissions[assignment_index].is_missing()
                        && match_count
                            > best_match
                                .as_ref()
                                .map(|best_match| best_match.match_count)
                                .unwrap_or_default()
                    {
                        best_match = Some(Match {
                            match_count,
                            sprint_index,
                            assignment_index,
                        });
                    }
                }
                Assignment::Attendance { .. } => {}
            }
        }
    }
    if let Some(Match {
        sprint_index,
        assignment_index,
        ..
    }) = best_match
    {
        submissions[sprint_index].submissions[assignment_index] =
            SubmissionState::Some(Submission::PullRequest { pull_request: pr });
    } else {
        unknown_prs.push(pr);
    }
}

fn title_word_set(title: &str) -> IndexSet<String> {
    title
        .to_lowercase()
        .split(" ")
        .flat_map(|word| word.split("_"))
        .flat_map(|word| word.split("-"))
        .flat_map(|word| word.split("/"))
        .flat_map(|word| word.split("|"))
        .map(|s| s.to_owned())
        .collect()
}

#[cfg(test)]
mod test {
    use crate::{
        course::{Assignment, Module, Sprint, Submission, SubmissionState},
        newtypes::GithubLogin,
        prs::{Pr, PrState},
    };
    use chrono::DateTime;

    #[test]
    fn match_prs_to_assignments_module_complexity() {
        let titles = vec![
            "02 Implement a linked list in Python",
            "03 Implement an LRU cache in Python",
            "04 Improve code with caches",
            "5 Improve code with precomputing",
            "Stretch: Implement a Skip List",
        ];

        let module = Module {
            sprints: vec![
                Sprint {
                    assignments: vec![Assignment::ExpectedPullRequest {
                        title: "3. Analyse and Refactor Functions".to_owned(),
                    }],
                },
                Sprint {
                    assignments: titles
                        .into_iter()
                        .map(|title| Assignment::ExpectedPullRequest {
                            title: title.to_owned(),
                        })
                        .collect(),
                },
            ],
        };

        let prs = vec![
            make_pr(
                16,
                "London | March-2025 | First Last | Sprint 2 | implement_linked_list",
            ),
            make_pr(
                17,
                "London | March-2025 | First Last | Sprint 2 | implement_lru_cache",
            ),
            make_pr(
                18,
                "London | March-2025 | First Last | Sprint 2 | improve_with_caches",
            ),
            make_pr(
                19,
                "London | March-2025 | First Last | Sprint 2 | improve_with_precomputing",
            ),
        ];

        let module_with_submissions =
            super::match_prs_to_assignments(&module, prs.clone(), Vec::new()).unwrap();
        let expected_unknowns: Vec<Pr> = Vec::new();
        assert_eq!(expected_unknowns, module_with_submissions.unknown_prs);

        let expected: Vec<_> = prs
            .into_iter()
            .map(|pull_request| SubmissionState::Some(Submission::PullRequest { pull_request }))
            .chain(std::iter::once(SubmissionState::MissingButNotExpected))
            .collect();

        assert_eq!(module_with_submissions.sprints[1].submissions, expected);
    }

    fn make_pr(number: u64, title: &str) -> Pr {
        Pr {
            repo_name: "Module-Complexity".to_owned(),
            number: number,
            url: format!("https://github.com/CodeYourFuture/Module-Complexity/pull/{number}"),
            title: title.to_owned(),
            author: GithubLogin("username".to_owned()),
            state: PrState::NeedsReview,
            updated_at: DateTime::from_timestamp_millis(i64::try_from(number).unwrap()).unwrap(),
        }
    }
}
