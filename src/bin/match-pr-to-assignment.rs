use std::process::exit;

use chrono::NaiveDate;
use indexmap::IndexMap;
use trainee_tracker::{
    config::{CourseSchedule, CourseScheduleWithRegisterSheetId},
    course::match_prs_to_assignments,
    newtypes::Region,
    octocrab::octocrab_for_token,
    prs::get_prs,
};

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    let (github_token, pr_link) = match args.as_slice() {
        [_, github_token, pr_link] => (github_token.clone(), pr_link.clone()),
        _ => {
            eprintln!("Expected two args - github token and PR link");
            exit(1);
        }
    };

    let octocrab = octocrab_for_token(github_token.to_owned()).expect("Failed to get octocrab");

    let pr_link_parts = pr_link.split("/").collect::<Vec<_>>();
    let org_name = pr_link_parts[3].to_owned();
    let module_name = pr_link_parts[4].to_owned();
    let pr_number: u64 = pr_link_parts[6].parse().expect("PR number not a number");

    let regions = [
        "London",
        "West Midlands",
        "North West",
        "Sheffield",
        "Glasgow",
        "South Africa",
    ];

    let fixed_date = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
    let mut sprints = IndexMap::new();
    sprints.insert(
        module_name.clone(),
        std::iter::repeat_with(|| {
            regions
                .iter()
                .map(|region| (Region(region.to_string()), fixed_date))
                .collect()
        })
        .take(3)
        .collect(),
    );
    let course_schedule = CourseSchedule {
        start: fixed_date,
        end: fixed_date,
        sprints,
    };
    let course = CourseScheduleWithRegisterSheetId {
        name: "itp".to_owned(),
        register_sheet_id: "".to_owned(),
        course_schedule,
    }
    .with_assignments(&octocrab, org_name.clone())
    .await
    .expect("Failed to get assignments");
    let module_prs = get_prs(&octocrab, org_name, module_name.clone())
        .await
        .expect("Failed to get PRs");
    let pr_in_question = module_prs
        .iter()
        .find(|pr| pr.number == pr_number)
        .unwrap()
        .clone();
    let user_prs: Vec<_> = module_prs
        .into_iter()
        .filter(|pr| pr.author == pr_in_question.author)
        .collect();
    let matched = match_prs_to_assignments(
        &course.modules[&module_name],
        user_prs,
        Vec::new(),
        &Region("London".to_owned()),
    )
    .expect("Failed to match PRs to assignments");

    for (sprint_index, (sprint, sprint_with_submissions)) in course.modules[&module_name]
        .sprints
        .iter()
        .zip(matched.sprints.iter())
        .enumerate()
    {
        println!("Sprint {}", sprint_index + 1);
        for (assignment, submission) in sprint
            .assignments
            .iter()
            .zip(sprint_with_submissions.submissions.iter())
        {
            println!("{:?} - {:?}", assignment, submission);
        }
    }
    for unknown in matched.unknown_prs {
        println!("Unknown PR: {:?}", unknown);
    }
}
