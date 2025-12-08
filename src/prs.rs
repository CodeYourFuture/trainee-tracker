use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use chrono::{DateTime, TimeDelta};
use futures::future::join_all;
use octocrab::Octocrab;
use octocrab::models::pulls::{Comment, PullRequest, Review as OctoReview};
use octocrab::models::{Author, IssueState};
use octocrab::params::State;
use serde::Serialize;

use crate::Error;
use crate::newtypes::GithubLogin;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Pr {
    pub repo_name: String,
    pub number: u64,
    pub url: String,
    pub title: String,
    pub author: GithubLogin,
    pub body: String,
    pub state: PrState,
    pub updated_at: DateTime<chrono::Utc>,
    pub is_closed: bool,
    pub labels: BTreeSet<String>
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum PrState {
    NeedsReview,
    Reviewed,
    Complete,
    Unknown,
}

impl From<&BTreeSet<String>> for PrState {
    fn from(labels: &BTreeSet<String>) -> Self {
        if labels.contains("Needs Review") {
            PrState::NeedsReview
        } else if labels.contains("Complete") {
            PrState::Complete
        } else if labels.contains("Reviewed") {
            PrState::Reviewed
        } else {
            PrState::Unknown
        }
    }
}

#[derive(Serialize)]
pub struct PrWithReviews {
    pr: Pr,
    reviews: BTreeSet<Review>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Review {
    created_at: DateTime<chrono::Utc>,
    author: GithubLogin,
}

pub async fn get_prs(
    octocrab: &Octocrab,
    org_name: &str,
    module: &str,
    include_complete_closed: bool,
) -> Result<Vec<Pr>, Error> {
    let page = octocrab
        .pulls(org_name, module)
        .list()
        .state(if include_complete_closed {
            State::All
        } else {
            State::Open
        })
        .send()
        .await
        .context("Failed to get PRs")?;
    let pulls_list = octocrab
        .all_pages(page)
        .await
        .context("Failed to list PRs")?;
    let prs = pulls_list
        .into_iter()
        .filter_map(
            |PullRequest {
                 html_url,
                 number,
                 user,
                 labels,
                 updated_at,
                 title,
                 state,
                 body,
                 ..
             }| {
                // If a user is deleted from GitHub, their User will be None - ignore PRs from deleted users.
                let author = GithubLogin::from(user?.login);

                let labels = labels
                    .into_iter()
                    .flatten()
                    .map(|label| label.name)
                    .collect();

                let pr_state = PrState::from(&labels);

                let is_closed = state.unwrap_or(IssueState::Open) == IssueState::Closed;
                if is_closed && pr_state != PrState::Complete {
                    return None;
                }

                // For some reason repo is generally None, but we know it, so...
                let repo_name = module.to_owned();

                // Unclear when they API would return None for these, ignore them.
                let updated_at = updated_at?;
                let url = html_url?.to_string();
                let title = title?;
                let body = body.unwrap_or_default();

                Some(Pr {
                    number,
                    url,
                    author,
                    state: pr_state,
                    updated_at,
                    repo_name,
                    title,
                    body,
                    is_closed,
                    labels
                })
            },
        )
        .collect();
    Ok(prs)
}

pub(crate) async fn fill_in_reviewers(
    octocrab: Octocrab,
    github_org: String,
    prs: Vec<Pr>,
) -> Result<Vec<PrWithReviews>, Error> {
    let mut comment_and_review_futures = BTreeMap::new();

    let mut repo_to_pr_numbers_to_prs = BTreeMap::new();
    for pr in prs {
        comment_and_review_futures.insert(
            (pr.repo_name.clone(), pr.number),
            (
                tokio::spawn(get_full_page(
                    octocrab.clone(),
                    github_org.clone(),
                    pr.repo_name.clone(),
                    pr.number,
                    CommentsOrReviews::Comments,
                )),
                tokio::spawn(get_full_page(
                    octocrab.clone(),
                    github_org.clone(),
                    pr.repo_name.clone(),
                    pr.number,
                    CommentsOrReviews::Reviews,
                )),
            ),
        );

        let pr_numbers_to_prs = repo_to_pr_numbers_to_prs
            .entry(pr.repo_name.clone())
            .or_insert_with(BTreeMap::new);
        pr_numbers_to_prs.insert(
            pr.number,
            PrWithReviews {
                pr,
                reviews: BTreeSet::new(),
            },
        );
    }

    for ((module, number), (comments_future, reviews_future)) in comment_and_review_futures {
        let reviews = &mut repo_to_pr_numbers_to_prs
            .get_mut(&module)
            .unwrap()
            .get_mut(&number)
            .unwrap()
            .reviews;

        for comment in comments_future
            .await
            .context("tokio execution failed in unexpected way")??
        {
            reviews.insert(comment);
        }

        for review in reviews_future
            .await
            .context("tokio execution failed in unexpected way")??
        {
            reviews.insert(review);
        }
    }
    Ok(repo_to_pr_numbers_to_prs
        .into_values()
        .flat_map(|map| map.into_values())
        .collect())
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) enum CheckStatus {
    CheckedAndOk,
    CheckedAndCheckAgain,
    Unchecked,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ReviewerStaffOnlyDetails {
    pub(crate) name: String,
    pub(crate) attended_training: bool,
    pub(crate) checked: CheckStatus,
    pub(crate) quality: String,
    pub(crate) notes: String,
}

#[derive(PartialEq, Eq, Serialize)]
pub(crate) struct ReviewerInfo {
    pub last_review: chrono::DateTime<chrono::Utc>,
    pub prs: Vec<ReviewedPr>,
    pub login: GithubLogin,
    pub reviews_days_in_last_28_days: u8,
    pub staff_only_details: MaybeReviewerStaffOnlyDetails,
}

#[derive(PartialEq, Eq, Serialize)]
pub(crate) enum MaybeReviewerStaffOnlyDetails {
    Some(ReviewerStaffOnlyDetails),
    Unknown,
    NotAuthenticated,
}

impl PartialOrd for ReviewerInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReviewerInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.last_review.cmp(&other.last_review) {
            core::cmp::Ordering::Equal => {}
            core::cmp::Ordering::Greater => return core::cmp::Ordering::Less,
            core::cmp::Ordering::Less => return core::cmp::Ordering::Greater,
        }
        match self.prs.len().cmp(&other.prs.len()) {
            core::cmp::Ordering::Equal => {}
            core::cmp::Ordering::Greater => return core::cmp::Ordering::Less,
            core::cmp::Ordering::Less => return core::cmp::Ordering::Greater,
        }
        self.login.cmp(&other.login)
    }
}

#[derive(PartialEq, Eq, Serialize)]
pub(crate) struct ReviewedPr {
    pub latest_review_time: chrono::DateTime<chrono::Utc>,
    pub pr: Pr,
}

pub(crate) async fn get_reviewers(
    octocrab: Octocrab,
    github_org: &str,
    module_names: &[String],
) -> Result<BTreeSet<ReviewerInfo>, Error> {
    let mut futures = Vec::new();
    for module in module_names {
        let octocrab = octocrab.clone();
        let github_org = github_org.to_owned();
        futures.push(async move {
            let prs = get_prs(&octocrab, &github_org, module, true).await?;
            fill_in_reviewers(octocrab, github_org, prs).await
        });
    }

    let now = chrono::Utc::now();

    let mut reviewers = BTreeMap::new();

    let mut reviewers_to_recent_review_days: BTreeMap<GithubLogin, BTreeSet<_>> = BTreeMap::new();

    for future in join_all(futures).await {
        for pr_with_reviews in future? {
            let mut reviewers_to_latest_time = BTreeMap::new();
            for review in pr_with_reviews.reviews {
                if review.author == pr_with_reviews.pr.author {
                    continue;
                }

                if now - review.created_at <= TimeDelta::weeks(4) {
                    reviewers_to_recent_review_days
                        .entry(review.author.clone())
                        .or_default()
                        .insert(review.created_at.date_naive());
                }

                let reviewer_info =
                    reviewers
                        .entry(review.author.clone())
                        .or_insert_with(|| ReviewerInfo {
                            last_review: chrono::DateTime::UNIX_EPOCH,
                            prs: Vec::new(),
                            login: review.author.clone(),
                            reviews_days_in_last_28_days: 0,
                            staff_only_details: MaybeReviewerStaffOnlyDetails::NotAuthenticated,
                        });
                if review.created_at > reviewer_info.last_review {
                    reviewer_info.last_review = review.created_at;
                }
                if *reviewers_to_latest_time
                    .entry(review.author.clone())
                    .or_insert(review.created_at)
                    < review.created_at
                {
                    reviewers_to_latest_time.insert(review.author, review.created_at);
                }
            }
            for (reviewer, latest_review_time) in reviewers_to_latest_time {
                reviewers.get_mut(&reviewer).unwrap().prs.push(ReviewedPr {
                    latest_review_time,
                    pr: pr_with_reviews.pr.clone(),
                });
            }
        }
    }

    for (reviewer, days) in reviewers_to_recent_review_days {
        reviewers
            .get_mut(&reviewer)
            // UNWRAP: Guaranteed by construction above to be present.
            .unwrap()
            // UNWRAP: Guaranteed by construction above to be <= 28
            .reviews_days_in_last_28_days = u8::try_from(days.len()).unwrap();
    }

    Ok(reviewers
        .into_values()
        .map(|mut r| {
            r.prs.sort_by_key(|pr| pr.latest_review_time);
            r.prs.reverse();
            r
        })
        .collect())
}

enum CommentsOrReviews {
    Comments,
    Reviews,
}

// Ideally this would be a more general shared function, but async closures aren't super stable yet.
async fn get_full_page<S1: AsRef<str>, S2: AsRef<str>>(
    octocrab: Octocrab,
    github_org: S1,
    repo_name: S2,
    number: u64,
    comments_or_reviews: CommentsOrReviews,
) -> Result<Vec<Review>, anyhow::Error> {
    match comments_or_reviews {
        CommentsOrReviews::Comments => {
            let page = octocrab
                .pulls(github_org.as_ref(), repo_name.as_ref())
                .list_comments(Some(number))
                .send()
                .await
                .context("Failed to get PR comments")?;
            let comments = octocrab
                .all_pages(page)
                .await
                .context("Failed to list PR comments")?;
            Ok(comments
                .into_iter()
                .filter_map(
                    |Comment {
                         created_at, user, ..
                     }| {
                        user.map(|Author { login, .. }| Review {
                            created_at,
                            author: GithubLogin::from(login),
                        })
                    },
                )
                .collect())
        }
        CommentsOrReviews::Reviews => {
            let page = octocrab
                .pulls(github_org.as_ref(), repo_name.as_ref())
                .list_reviews(number)
                .send()
                .await
                .context("Failed to get PR reviews")?;
            let reviews = octocrab
                .all_pages(page)
                .await
                .context("Failed to list PR reviews")?;
            Ok(reviews
                .into_iter()
                .filter_map(
                    |OctoReview {
                         submitted_at, user, ..
                     }| {
                        match (submitted_at, user) {
                            (Some(created_at), Some(Author { login, .. })) => Some(Review {
                                created_at,
                                author: GithubLogin::from(login),
                            }),
                            _ => None,
                        }
                    },
                )
                .collect())
        }
    }
}
