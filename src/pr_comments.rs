use anyhow::Context;
use futures::future::join_all;
use octocrab::Octocrab;

use crate::{Error, octocrab::all_pages};

pub async fn leave_tagged_comment<S: AsRef<str>>(
    octocrab: &Octocrab,
    pull_request: &PullRequest,
    tags: &[S],
    body: String,
) -> Result<(), Error> {
    let mut body = body;
    for tag in tags {
        body.push_str("\n");
        body.push_str(TAG_PREFIX);
        body.push_str(tag.as_ref());
        body.push_str(TAG_SUFFIX);
    }
    octocrab
        .issues(&pull_request.org, &pull_request.repo)
        .create_comment(pull_request.number, body)
        .await
        .with_context(|| format!("Failed to leave common on PR {}", pull_request.html_url()))?;
    Ok(())
}

pub async fn close_existing_comments(
    octocrab: &Octocrab,
    pull_request: &PullRequest,
    tag: &str,
) -> Result<(), Error> {
    let comments = all_pages("getting PR comments", octocrab, async || {
        octocrab
            .issues(&pull_request.org, &pull_request.repo)
            .list_comments(pull_request.number)
            .send()
            .await
    })
    .await
    .map_err(|err| {
        err.with_context(|| format!("Getting comments for PR {}", pull_request.html_url()))
    })?;
    let futures: Vec<_> = comments
        .into_iter()
        .filter(|comment| comment.body.as_deref().unwrap_or("").contains(&format!("{TAG_PREFIX}{tag}{TAG_SUFFIX}")))
        .map(|comment| comment.node_id)
        .map(|id| async move { octocrab.graphql(&serde_json::json!({"query": graphql::HIDE_COMMENT_MUTATION, "variables": {"commentId": id}})).await })
        .collect();
    let results: Vec<Result<graphql::Result<graphql::MinimiseCommentResponse>, octocrab::Error>> =
        join_all(futures).await;
    for result in results {
        match result {
            Ok(graphql_result) => {
                graphql_result.into_result()?;
            }
            Err(err) => {
                Err(err).with_context(|| {
                    format!("Closing comments for PR {}", pull_request.html_url())
                })?;
            }
        }
    }
    Ok(())
}

pub struct PullRequest {
    pub org: String,
    pub repo: String,
    pub number: u64,
}

impl PullRequest {
    pub fn from_html_url(url: &str) -> Result<PullRequest, Error> {
        let pr_parts: Vec<_> = url.split("/").collect();
        let (org, repo, number) = match pr_parts.as_slice() {
            [
                _http,
                _scheme,
                _domain,
                github_org_name,
                repo,
                _pull,
                number,
            ] => (
                (*github_org_name).to_owned(),
                (*repo).to_owned(),
                number.parse::<u64>().context("Failed to parse PR number")?,
            ),
            _ => {
                return Err(Error::UserFacing(format!(
                    "Failed to parse PR URL {} - wrong number of components",
                    url
                )));
            }
        };
        Ok(PullRequest { org, repo, number })
    }

    pub fn html_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.org, self.repo, self.number
        )
    }
}

const TAG_PREFIX: &str = "<!--CYFTT tag: ";
const TAG_SUFFIX: &str = "-->";

// Unfortunately there's no "hide comment" REST API so we need a whole GraphQL thing...
#[allow(unused)]
mod graphql {
    use serde::Deserialize;

    pub const HIDE_COMMENT_MUTATION: &str = r#"
mutation($commentId: ID!) {
  minimizeComment(input: {
    subjectId: $commentId
    classifier: OUTDATED
  }) {
    minimizedComment {
      isMinimized
      minimizedReason
    }
  }
}
"#;

    #[derive(Debug, Deserialize)]
    pub struct Result<T> {
        pub data: Option<T>,
        pub errors: Option<Vec<Error>>,
    }

    impl<T> Result<T> {
        pub fn into_result(self) -> std::result::Result<Option<T>, anyhow::Error> {
            if let Some(errors) = self.errors {
                if errors.is_empty() {
                    Ok(self.data)
                } else {
                    Err(anyhow::anyhow!("GraphQL errors: {:?}", errors))
                }
            } else {
                Ok(self.data)
            }
        }
    }

    #[derive(Debug, Deserialize)]
    pub struct Error {
        pub r#type: Option<String>,
        pub path: Option<Vec<String>>,
        pub locations: Option<Vec<ErrorLocation>>,
        pub message: String,
    }

    #[derive(Debug, Deserialize)]
    pub struct ErrorLocation {
        pub line: usize,
        pub column: usize,
    }

    #[derive(Debug, Deserialize)]
    pub struct MinimiseCommentResponse {
        #[serde(rename = "minimizeComment")]
        pub minimise_comment: OuterMinimizedComment,
    }

    #[derive(Debug, Deserialize)]
    pub struct OuterMinimizedComment {
        #[serde(rename = "minimizedComment")]
        pub minimized_comment: MinimizedComment,
    }

    #[derive(Debug, Deserialize)]
    pub struct MinimizedComment {
        #[serde(rename = "isMinimized")]
        pub is_minimized: bool,
        #[serde(rename = "minimizedReason")]
        pub minimized_reason: String,
    }
}
