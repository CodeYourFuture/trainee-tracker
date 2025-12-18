/// This binary exists to be a lightweight teaching version of the pr-metadata-validator.
/// Its purpose is to train trainees in the idea that bots will comment on their PRs, and they should follow their advice.
/// It is installed in https://github.com/CodeYourFuture/github_issues_prs_practice as a GitHub Action.
use std::process::exit;

use const_format::concatcp;
use trainee_tracker::{
    octocrab::octocrab_for_token,
    pr_comments::{PullRequest, close_existing_comments, leave_tagged_comment},
};

#[tokio::main]
async fn main() {
    let Ok([_argv0, pr_url]) = <[_; _]>::try_from(std::env::args().collect::<Vec<_>>()) else {
        eprintln!("Expected one arg - PR URL");
        exit(1);
    };
    let pr_metadata = PullRequest::from_html_url(&pr_url).expect("Failed to parse PR URL");
    let github_token =
        std::env::var("GH_TOKEN").expect("GH_TOKEN wasn't set - must be set to a GitHub API token");
    let octocrab = octocrab_for_token(github_token.to_owned()).expect("Failed to get octocrab");

    let pr_from_rest = octocrab
        .pulls(&pr_metadata.org, &pr_metadata.repo)
        .get(pr_metadata.number)
        .await
        .expect("Failed to get PR");
    if pr_from_rest
        .body
        .unwrap_or_default()
        .ends_with(EXPECTED_SUFFIX)
    {
        let result = close_existing_comments(&octocrab, &pr_metadata, TAG).await;
        if let Err(err) = result {
            eprintln!("Failed to close existing comments: {:?}", err);
        }
    } else {
        leave_tagged_comment(&octocrab, &pr_metadata, &[TAG], COMMENT_TO_LEAVE.to_owned())
            .await
            .expect("Failed to leave comment");
    }
}

const EXPECTED_SUFFIX: &str = "I agree to follow the code of conduct for this organisation.";

const TAG: &str = "dummy-code-of-conduct-validator";

const COMMENT_TO_LEAVE: &str = concatcp!(
    COMMENT_TO_LEAVE_PREFIX,
    EXPECTED_SUFFIX,
    COMMENT_TO_LEAVE_SUFFIX
);

const COMMENT_TO_LEAVE_PREFIX: &str = r#"This is a comment from a bot.

You should read it, make sure you understand it, and take the action it suggests.

If you don't understand the action it suggests, ask a volunteer or another trainee for help.

## ⚠️ Problem detected

In this repository, all pull request descriptions must end with the sentence:

> "#;

const COMMENT_TO_LEAVE_SUFFIX: &str = r#"

Your pull request description does not currently end with this sentence.

Please edit your pull request description to add this sentence at the end.

If you are successful in doing this, this comment will get automatically hidden within about a minute.
"#;
