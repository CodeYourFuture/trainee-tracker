use std::{collections::BTreeMap, process::exit};

use chrono::NaiveDate;
use clap::Parser;
use indexmap::IndexMap;
use maplit::btreemap;
use octocrab::Octocrab;
use regex::Regex;
use trainee_tracker::{
    Error,
    config::{CourseSchedule, CourseScheduleWithRegisterSheetId},
    course::{get_descriptor_id_for_pr, match_prs_to_assignments},
    newtypes::Region,
    octocrab::{all_pages, octocrab_for_token},
    pr_comments::{PullRequest, close_existing_comments, leave_tagged_comment},
    prs::get_prs,
};

const ARBITRARY_REGION: Region = Region(String::new());

#[derive(Parser)]
struct Args {
    pr_url: String,

    #[arg(long)]
    give_more_specific_comment_for_earlier_learners: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let pr = PullRequest::from_html_url(&args.pr_url).expect("Failed to parse PR URL");

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

    let course_schedule = make_fake_course_schedule(pr.repo.clone());

    let course = CourseScheduleWithRegisterSheetId {
        name: "itp".to_owned(),
        register_sheet_id: "".to_owned(),
        course_schedule,
    };
    let result = validate_pr(
        &octocrab,
        course,
        &pr.repo,
        &pr.org,
        pr.number,
        &known_region_aliases,
    )
    .await
    .expect("Failed to validate PR");

    const PR_METADATA_VALIDATOR_LABEL: &str = "pr-metadata-validator";

    let message = match &result {
        ValidationResult::Ok => {
            if let Err(err) =
                close_existing_comments(&octocrab, &pr, PR_METADATA_VALIDATOR_LABEL).await
            {
                eprintln!("Failed to close existing comments: {:?}", err);
            }
            exit(0);
        }
        ValidationResult::CouldNotMatch => COULD_NOT_MATCH_COMMENT,
        ValidationResult::BodyTemplateNotFilledOut => {
            if args.give_more_specific_comment_for_earlier_learners {
                BODY_TEMPLATE_NOT_FILLED_IN_SPECIFIC_COMMENT
            } else {
                BODY_TEMPLATE_NOT_FILLED_IN_VAGUE_COMMENT
            }
        }
        ValidationResult::BadTitleFormat { reason } => {
            &format!("{}{}", BAD_TITLE_COMMENT_PREFIX, reason)
        }
        ValidationResult::UnknownRegion => UNKNOWN_REGION_COMMENT,
        ValidationResult::WrongFiles {
            expected_files_pattern,
        } => &format!("{}`{}`", WRONG_FILES, expected_files_pattern),
        ValidationResult::NoFiles => NO_FILES,
        ValidationResult::TooManyFiles => TOO_MANY_FILES,
    };

    let full_message = format!(
        "{message}\n\nIf this PR is not coursework, please add the NotCoursework label (and message on Slack in #cyf-curriculum or it will probably not be noticed).\n\nIf this PR needs reviewed, please add the 'Needs Review' label to this PR after you have resolved the issues listed above."
    );
    eprintln!("{}", full_message);
    leave_tagged_comment(
        &octocrab,
        &pr,
        &[PR_METADATA_VALIDATOR_LABEL, &result.to_string()],
        full_message,
    )
    .await
    .expect("Failed to create comment with validation error");
    let remove_label_response = octocrab
        .issues(&pr.org, &pr.repo)
        .remove_label(pr.number, "Needs Review")
        .await;
    match remove_label_response {
        Ok(_) => {
            println!(
                "Found issues for PR #{}, notified and removed label",
                pr.number
            );
        }
        Err(octocrab::Error::GitHub { source, .. }) if source.status_code == 404 => {
            println!(
                "Found issues for PR #{}, notified and label already removed",
                pr.number
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

const BODY_TEMPLATE_NOT_FILLED_IN_VAGUE_COMMENT: &str = r#"Your PR description contained template fields which weren't filled in.

Check you've ticked everything in the self checklist, and that any sections which prompt you to fill in an answer are either filled in or removed."#;

const BODY_TEMPLATE_NOT_FILLED_IN_SPECIFIC_COMMENT: &str = r#"Your PR description is incomplete.

You filled out a template (that starts "Learners, PR Template") when you created this PR - you can see it at the top of this page.

Make sure to fill in all fields in the template.

Please ensure:
- [ ] All self checklist items are ticked (with a `[x]`)
- [ ] The "Changelist" section is filled with details of what your PR does.
- [ ] The "Questions" section is either filled in (if you have questions) or is removed (if you don't)."#;

const BAD_TITLE_COMMENT_PREFIX: &str = r#"Your PR's title isn't in the expected format.

Please check the expected title format, and update yours to match.

Reason: "#;

const UNKNOWN_REGION_COMMENT: &str = r#"Your PR's title didn't contain a known region.

Please check the expected title format, and make sure your region is in the correct place and spelled correctly."#;

const WRONG_FILES: &str = r#"The changed files in this PR don't match what is expected for this task.

Please check that you committed the right files for the task, and that there are no accidentally committed files from other sprints.

Please review the changed files tab at the top of the page, we are only expecting changes in this directory: "#;

const NO_FILES: &str = r#"This PR is missing any submitted files.

Please check that you committed the right files and pushed to the repository"#;

const TOO_MANY_FILES: &str = r#"There are too many files committed in this pull request.

Please check and make sure you have not accidentally committed a cache, virtual environment, or npm package directory."#;

#[derive(strum_macros::Display)]
enum ValidationResult {
    Ok,
    BodyTemplateNotFilledOut,
    CouldNotMatch,
    BadTitleFormat { reason: String },
    UnknownRegion,
    WrongFiles { expected_files_pattern: String },
    NoFiles,
    TooManyFiles,
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
            anyhow::anyhow!(
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

    let pr_assignment_descriptor_id =
        get_descriptor_id_for_pr(&matched.sprints, pr_number).expect("This PR does not exist");
    // This should never error, as a PR by this point in code must have been matched
    // with an assignment, and PR assignments must have an associated issue descriptor

    check_pr_file_changes(
        octocrab,
        github_org_name,
        module_name,
        pr_number,
        pr_assignment_descriptor_id,
    )
    .await
}

// Check the changed files in a pull request match what is expected for that sprint task
async fn check_pr_file_changes(
    octocrab: &Octocrab,
    org_name: &str,
    module_name: &str,
    pr_number: u64,
    task_issue_number: u64,
) -> Result<ValidationResult, Error> {
    // Get the Sprint Task's description of expected changes
    let Ok(task_issue) = octocrab
        .issues(org_name, module_name)
        .get(task_issue_number)
        .await
    else {
        return Ok(ValidationResult::CouldNotMatch); // Failed to find the right task
    };

    let task_issue_body = task_issue.body.unwrap_or_default();

    let directory_description = Regex::new("CHANGE_DIR=(.+)\\n")
        .map_err(|err| Error::UserFacing(format!("Known good regex failed to compile: {}", err)))?;
    let Some(directory_regex_captures) = directory_description.captures(&task_issue_body) else {
        return Ok(ValidationResult::Ok); // There is no match defined for this task, don't do any more checks
    };
    let directory_description_regex = directory_regex_captures
        .get(1)
        .expect("Regex capture failed to return string match")
        .as_str(); // Only allows a single directory for now

    let directory_matcher = Regex::new(directory_description_regex).map_err(|err| {
        Error::UserFacing(format!(
            "Failed to compile regex from {}, check the CHANGE_DIR declaration: {}",
            task_issue.html_url, err
        ))
    })?;

    // Get all of the changed files
    let pr_files = all_pages("changed files in pull request", octocrab, async || {
        octocrab
            .pulls(org_name, module_name)
            .list_files(pr_number)
            .await
    })
    .await?;
    if pr_files.is_empty() {
        return Ok(ValidationResult::NoFiles); // no files committed
    }

    if pr_files.len() > 100 {
        return Ok(ValidationResult::TooManyFiles); // too many files probably a venv or npm cache
    }

    // check each file and error if one is in unexpected place
    for pr_file in pr_files {
        if pr_file.filename == ".gitignore" {
            continue; // always allow top-level gitignore changes
        }
        if !directory_matcher.is_match(&pr_file.filename) {
            return Ok(ValidationResult::WrongFiles {
                expected_files_pattern: directory_description_regex.to_string(),
            });
        }
    }

    Ok(ValidationResult::Ok)
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
