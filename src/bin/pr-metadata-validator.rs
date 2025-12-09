use std::{collections::BTreeMap, process::exit};

use anyhow::{anyhow, Context};
use chrono::NaiveDate;
use indexmap::IndexMap;
use maplit::btreemap;
use octocrab::Octocrab;
use regex::Regex;
use trainee_tracker::{
    Error,
    config::{CourseSchedule, CourseScheduleWithRegisterSheetId},
    course::match_prs_to_assignments,
    newtypes::Region,
    octocrab::octocrab_for_token,
    prs::get_prs,
};

const ARBITRARY_REGION: Region = Region(String::new());

#[tokio::main]
async fn main() {
    let Ok([_argv0, pr_url]) = <[_; 2]>::try_from(std::env::args().collect::<Vec<_>>()) else {
        eprintln!("Expected one arg - PR URL");
        exit(1);
    };
    let pr_parts: Vec<_> = pr_url.split("/").collect();
    let (github_org_name, module_name, pr_number) = match pr_parts.as_slice() {
        [
            _http,
            _scheme,
            _domain,
            github_org_name,
            module_name,
            _pull,
            number,
        ] => (
            (*github_org_name).to_owned(),
            (*module_name).to_owned(),
            number.parse::<u64>().expect("Failed to parse PR number"),
        ),
        _ => {
            eprintln!("Failed to parse PR URL");
            exit(1);
        }
    };

    // TODO: Fetch this from classplanner or somewhere when we have access to a useful API.
    let known_region_aliases = KnownRegions(btreemap! {
        "Cape Town" => vec!["South Africa", "SouthAfrica", "ZA", "ZA Cape Town"],
        "Glasgow" => vec!["Scotland"],
        "London" => vec![],
        "North West" => vec!["NW", "Manchester"],
        "Sheffield" => vec![],
        "West Midlands" => vec!["WM", "WestMidlands", "West-Midlands", "Birmingham"],
    });

    let github_token =
        std::env::var("GH_TOKEN").expect("GH_TOKEN wasn't set - must be set to a GitHub API token");
    let octocrab = octocrab_for_token(github_token).expect("Failed to get octocrab");

    let course_schedule = make_fake_course_schedule(module_name.clone());

    let course = CourseScheduleWithRegisterSheetId {
        name: "itp".to_owned(),
        register_sheet_id: "".to_owned(),
        course_schedule,
    };
    let result = validate_pr(
        &octocrab,
        course,
        &module_name,
        &github_org_name,
        pr_number,
        &known_region_aliases,
    )
    .await
    .expect("Failed to validate PR");
    let message = match result {
        ValidationResult::Ok => {
            exit(0);
        }
        ValidationResult::CouldNotMatch => COULD_NOT_MATCH_COMMENT,
        ValidationResult::BodyTemplateNotFilledOut => BODY_TEMPLATE_NOT_FILLED_IN_COMMENT,
        ValidationResult::BadTitleFormat { reason } => {
            &format!("{}{}", BAD_TITLE_COMMENT_PREFIX, reason)
        }
        ValidationResult::UnknownRegion => UNKNOWN_REGION_COMMENT,
        ValidationResult::WrongFiles => WRONG_FILES,
    };

    let full_message = format!(
        "{message}\n\nIf this PR is not coursework, please add the NotCoursework label (and message on Slack in #cyf-curriculum or it will probably not be noticed).\n\nIf this PR needs reviewed, please add the 'Needs Review' label to this PR after you have resolved the issues listed above."
    );
    eprintln!("{}", full_message);
    octocrab
        .issues(&github_org_name, &module_name)
        .create_comment(pr_number, full_message)
        .await
        .expect("Failed to create comment with validation error");
    let remove_label_response = octocrab
        .issues(&github_org_name, &module_name)
        .remove_label(pr_number, "Needs Review")
        .await;
    match remove_label_response {
        Ok(_) => {
            println!(
                "Found issues for PR #{}, notified and removed label",
                pr_number
            );
        }
        Err(octocrab::Error::GitHub { source, .. }) if source.status_code == 404 => {
            println!(
                "Found issues for PR #{}, notified and label already removed",
                pr_number
            );
            // The only time this API 404s is if the label is already removed. Continue without error.
        }
        err => {
            eprintln!("Error removing label: {:?}", err);
        }
    };
    exit(2);
}

const COULD_NOT_MATCH_COMMENT: &str = r#"Your PR couldn't be matched to an assignment in this module.

Please check its title is in the correct format, and that you only have one PR per assignment."#;

const BODY_TEMPLATE_NOT_FILLED_IN_COMMENT: &str = r#"Your PR description contained template fields which weren't filled in.

Check you've ticked everything in the self checklist, and that any sections which prompt you to fill in an answer are either filled in or removed."#;

const BAD_TITLE_COMMENT_PREFIX: &str = r#"Your PR's title isn't in the expected format.

Please check the expected title format, and update yours to match.

Reason: "#;

const UNKNOWN_REGION_COMMENT: &str = r#"Your PR's title didn't contain a known region.

Please check the expected title format, and make sure your region is in the correct place and spelled correctly."#;

const WRONG_FILES: &str = r#"The changed files in this PR don't match what is expected for this task.

Please check that you committed the right files for the task, and that there are no accidentally committed files from other sprints."#;

enum ValidationResult {
    Ok,
    BodyTemplateNotFilledOut,
    CouldNotMatch,
    BadTitleFormat { reason: String },
    UnknownRegion,
    WrongFiles
}

async fn validate_pr(
    octocrab: &Octocrab,
    course_schedule: CourseScheduleWithRegisterSheetId,
    module_name: &str,
    github_org_name: &str,
    pr_number: u64,
    known_region_aliases: &KnownRegions,
) -> Result<ValidationResult, Error> {
    let course = course_schedule
        .with_assignments(octocrab, github_org_name)
        .await
        .map_err(|err| err.context("Failed to get assignments"))?;

    let module_prs = get_prs(octocrab, github_org_name, module_name, false)
        .await
        .map_err(|err| err.context("Failed to get PRs"))?;
    let pr_in_question = module_prs
        .iter()
        .find(|pr| pr.number == pr_number)
        .ok_or_else(|| {
            anyhow!(
                "Failed to find PR {} in list of PRs for module {}",
                pr_number,
                module_name
            )
        })?
        .clone();

    if pr_in_question.labels.contains("NotCoursework") {
        return Ok(ValidationResult::Ok);
    }

    let user_prs: Vec<_> = module_prs
        .into_iter()
        .filter(|pr| pr.author == pr_in_question.author)
        .collect();
    let matched = match_prs_to_assignments(
        &course.modules[module_name],
        user_prs,
        Vec::new(),
        &ARBITRARY_REGION,
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

    if !known_region_aliases.is_known_ignoring_case(title_sections[0].trim()) {
        return Ok(ValidationResult::UnknownRegion);
    }

    // TODO: Validate cohorts when they're known (1)
    let sprint_regex = Regex::new(r"^(S|s)print \d+$").unwrap();
    let sprint_section = title_sections[3].trim();
    if !sprint_regex.is_match(sprint_section) {
        return Ok(ValidationResult::BadTitleFormat {
            reason: format!(
                "Sprint part ({}) doesn't match expected format (example: 'Sprint 2', without quotes)",
                sprint_section
            ),
        });
    }

    if pr_in_question.title.to_ascii_uppercase() == pr_in_question.title {
        return Ok(ValidationResult::BadTitleFormat {
            reason: "PR title should not all be in uppercase".to_owned(),
        });
    }

    if pr_in_question.body.contains("Briefly explain your PR.")
        || pr_in_question
            .body
            .contains("Ask any questions you have for your reviewer.")
        || pr_in_question.body.contains("- [ ]")
    {
        return Ok(ValidationResult::BodyTemplateNotFilledOut);
    }

    match check_pr_file_changes(octocrab, github_org_name, module_name, pr_number, 35) // TODO get the correct one,just default to this for testing now
        .await {
            Ok(Some(problem)) => return Ok(problem),
            Ok(None) => (),
            Err(e) => { let _ = anyhow!(e); }
        }

    Ok(ValidationResult::Ok)
}

// Check the changed files in a pull request match what is expected for that sprint task
async fn check_pr_file_changes(
    octocrab: &Octocrab,
    org_name: &str,
    module_name: &str,
    pr_number: u64,
    task_issue_number: u64,
) -> Result<Option<ValidationResult>, Error> {
    // Get the Sprint Task's description of expected changes
    let task_issue = match octocrab
        .issues(org_name, module_name)
        .get(task_issue_number)
        .await {
            Ok(iss) => iss,
            Err(_) => return Ok(Some(ValidationResult::CouldNotMatch)) // Failed to find the right task
        };
    let task_issue_body = match task_issue.body {
        Some(body) => body,
        None => return Ok(None) // Task is empty, nothing left to check
    };
    let directory_description = Regex::new("CHANGE_DIR=(.+)\\n").unwrap();
    let directory_description_regex = match directory_description.captures(&task_issue_body) {
        Some(capts) => capts.get(1).unwrap().as_str(), // Only allows a single directory for now
        None => return Ok(None) // There is no match defined for this task, don't do any more checks
    };
    let directory_matcher = Regex::new(directory_description_regex)
        .context("Invalid regex for task directory match")?;
    // Get all of the changed files
    let pr_files_pages = octocrab
        .pulls(org_name, module_name)
        .list_files(pr_number)
        .await
        .context("Failed to get changed files")?;
    if pr_files_pages.items.len() == 0 {
        return Ok(Some(ValidationResult::WrongFiles)); // no files committed
    }
    let pr_files_all = octocrab
        .all_pages(pr_files_pages)
        .await
        .context("Failed to list all changed files")?;
    let mut pr_files = pr_files_all
        .into_iter();
    let mut i = 0;
    // check each file and error if one is in unexpected place
    println!("{}", directory_description_regex);
    while let Some(pr_file) = pr_files.next() {
        i += 1;
        println!("{}{}", i, pr_file.filename);
        if !directory_matcher.is_match(&pr_file.filename) {
            println!("Found bad match");
            return Ok(Some(ValidationResult::WrongFiles))
        }
    }
    return Ok(None);
}

struct KnownRegions(BTreeMap<&'static str, Vec<&'static str>>);

impl KnownRegions {
    fn is_known_ignoring_case(&self, possible_region: &str) -> bool {
        let possible_region_lower = possible_region.to_ascii_lowercase();
        for (known_region, known_region_aliases) in &self.0 {
            if known_region.to_ascii_lowercase() == possible_region_lower {
                return true;
            }
            for known_region_alias in known_region_aliases {
                if known_region_alias.to_ascii_lowercase() == possible_region_lower {
                    return true;
                }
            }
        }
        false
    }
}

fn make_fake_course_schedule(module_name: String) -> CourseSchedule {
    let fixed_date = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
    let mut sprints = IndexMap::new();
    sprints.insert(
        module_name,
        std::iter::repeat_with(|| btreemap![ARBITRARY_REGION => fixed_date])
            // 5 is the max number of sprints a module (currently) contains.
            .take(5)
            .collect(),
    );
    CourseSchedule {
        start: fixed_date,
        end: fixed_date,
        sprints,
    }
}
