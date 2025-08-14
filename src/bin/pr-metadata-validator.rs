use std::process::exit;

use chrono::NaiveDate;
use indexmap::IndexMap;
use maplit::btreemap;
use octocrab::Octocrab;
use regex::Regex;
use trainee_tracker::{
    config::{CourseSchedule, CourseScheduleWithRegisterSheetId},
    course::match_prs_to_assignments,
    newtypes::Region,
    octocrab::octocrab_for_token,
    prs::get_prs,
    Error,
};

const ARBITRARY_REGION: Region = Region(String::new());

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    let pr_url = match args.as_slice() {
        [_argv0, pr_url] => pr_url,
        _ => {
            eprintln!("Expected one arg - PR URL");
            exit(1);
        }
    };
    let pr_parts: Vec<_> = pr_url.split("/").collect();
    let (github_org_name, module_name, pr_number) = match pr_parts.as_slice() {
        [_http, _scheme, _domain, github_org_name, module_name, _pull, number] => (
            (*github_org_name).to_owned(),
            (*module_name).to_owned(),
            number.parse::<u64>().expect("Failed to parse PR number"),
        ),
        _ => {
            eprintln!("Failed to parse PR URL");
            exit(1);
        }
    };

    let github_token =
        std::env::var("GH_TOKEN").expect("GH_TOKEN wasn't set - must be set to a GitHub API token");
    let octocrab = octocrab_for_token(github_token).expect("Failed to get octocrab");

    let course_schedule = make_fake_course_schedule(module_name.clone());

    let course = CourseScheduleWithRegisterSheetId {
        name: "itp".to_owned(),
        register_sheet_id: "".to_owned(),
        course_schedule,
    };
    let result = validate_pr(&octocrab, course, &module_name, &github_org_name, pr_number)
        .await
        .expect("Failed to validate PR");
    match result {
        ValidationResult::Ok => {}
        ValidationResult::CouldNotMatch => {
            eprintln!("Validation error: Could not match PR against assignment");
            octocrab
                .issues(github_org_name, module_name.clone())
                .create_comment(pr_number, COULD_NOT_MATCH_COMMENT)
                .await
                .expect("Failed to create comment with validation error");
            exit(2);
        }
        ValidationResult::BadTitleFormat { reason } => {
            eprintln!("Validation error: Bad title: {}", reason);
            octocrab
                .issues(github_org_name, module_name.clone())
                .create_comment(pr_number, format!("{}{}", BAD_TITLE_COMMENT_PREFIX, reason))
                .await
                .expect("Failed to create comment with validation error");
            exit(2);
        }
    }
}

const COULD_NOT_MATCH_COMMENT: &'static str = r#"Your PR couldn't be matched to an assignment in this module.

Please check its title is in the correct format, and that you only have one PR per assignment.

If this PR is not coursework, please add the NotCoursework label (and message on Slack in #cyf-curriculum or it will probably not be noticed)."#;

const BAD_TITLE_COMMENT_PREFIX: &'static str = r#"Your PR's title isn't in the expected format.

Please check its title is in the correct format, and update it.

Reason: "#;

enum ValidationResult {
    Ok,
    CouldNotMatch,
    BadTitleFormat { reason: String },
}

async fn validate_pr(
    octocrab: &Octocrab,
    course_schedule: CourseScheduleWithRegisterSheetId,
    module_name: &str,
    github_org_name: &str,
    pr_number: u64,
) -> Result<ValidationResult, Error> {
    let course = course_schedule
        .with_assignments(&octocrab, github_org_name)
        .await
        .map_err(|err| err.context("Failed to get assignments"))?;

    let module_prs = get_prs(&octocrab, github_org_name, module_name, false)
        .await
        .map_err(|err| err.context("Failed to get PRs"))?;
    let pr_in_question = module_prs
        .iter()
        .find(|pr| pr.number == pr_number)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Failed to find PR {} in list of PRs for module {}",
                pr_number,
                module_name
            )
        })?
        .clone();
    let user_prs: Vec<_> = module_prs
        .into_iter()
        .filter(|pr| pr.author == pr_in_question.author)
        .collect();
    let matched = match_prs_to_assignments(
        &course.modules[module_name],
        user_prs,
        Vec::new(),
        &Region("London".to_owned()),
    )
    .map_err(|err| err.context("Failed to match PRs to assignments"))?;

    for pr in matched.unknown_prs {
        if pr.number == pr_number {
            return Ok(ValidationResult::CouldNotMatch);
        }
    }

    let title_sections: Vec<&str> = pr_in_question.title.split("|").collect();
    if title_sections.len() != 5 {
        return Ok(ValidationResult::BadTitleFormat {
            reason: "Wrong number of parts separated by |s".to_owned(),
        });
    }
    // TODO: Validate Regions when they're known (0)
    // TODO: Validate cohorts when they're known (1)
    let sprint_regex = Regex::new(r"^(S|s)print \d+$").unwrap();
    let sprint_section = title_sections[3].trim();
    if !sprint_regex.is_match(sprint_section) {
        return Ok(ValidationResult::BadTitleFormat { reason: format!("Sprint part ({}) doesn't match expected format (example: 'Sprint 2', without quotes)", sprint_section) });
    }

    if pr_in_question.title.to_ascii_uppercase() == pr_in_question.title {
        return Ok(ValidationResult::BadTitleFormat {
            reason: "PR title should not all be in uppercase".to_owned(),
        });
    }

    Ok(ValidationResult::Ok)
}

fn make_fake_course_schedule(module_name: String) -> CourseSchedule {
    let fixed_date = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
    let mut sprints = IndexMap::new();
    sprints.insert(
        module_name,
        std::iter::repeat_with(|| btreemap![ARBITRARY_REGION => fixed_date])
            .take(3)
            .collect(),
    );
    CourseSchedule {
        start: fixed_date,
        end: fixed_date,
        sprints,
    }
}
