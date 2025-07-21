use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use futures::future::join_all;
use http::Uri;
use slack_with_types::{
    client::RateLimiter, newtypes::UserGroupId, usergroups::UserGroup, users::UserInfo,
};
use tower_sessions::Session;
use uuid::Uuid;

use crate::{Error, ServerState};

pub(crate) const SLACK_ACCESS_TOKEN_SESSION_KEY: &str = "slack_access_token";

pub(crate) async fn slack_client(
    session: &Session,
    server_state: ServerState,
    original_uri: Uri,
) -> Result<slack_with_types::client::Client, Error> {
    let maybe_token: Option<String> = session
        .get(SLACK_ACCESS_TOKEN_SESSION_KEY)
        .await
        .context("Session load error")?;
    if let Some(access_token) = maybe_token {
        let rate_limiter = server_state
            .slack_rate_limiters
            .get_with(access_token.clone(), async { RateLimiter::new() })
            .await;
        Ok(slack_with_types::client::Client::new(
            reqwest::Client::new(),
            rate_limiter,
            access_token,
        ))
    } else {
        let state = Uuid::new_v4();
        server_state
            .slack_auth_state_cache
            .insert(state, original_uri)
            .await;
        let client_id = &server_state.config.slack_client_id;
        let redirect_uri = make_slack_redirect_uri(&server_state.config.public_base_url);
        // TODO: Generalise requesting scopes
        Err(Error::Redirect(
            format!("https://slack.com/oauth/v2/authorize?scope={},{},{}&client_id={}&redirect_uri={}&state={}", "usergroups:read", "users:read", "users:read.email", client_id, redirect_uri, state).parse().context("Statically known correct Slack auth Uri couldn't be constructed")?
        ))
    }
}

pub(crate) fn make_slack_redirect_uri(public_base_uri: &str) -> Uri {
    format!("{}/api/oauth-callbacks/slack", public_base_uri,)
        .parse()
        .expect("Statically known Slack redirect URI failed to parse")
}

#[derive(Clone, Debug)]
pub(crate) struct SlackUserGroup {
    pub(crate) name: String,
    pub(crate) handle: String,
    pub(crate) id: UserGroupId,
    pub(crate) members: Vec<UserInfo>,
}

pub(crate) async fn list_groups_with_members(
    client: slack_with_types::client::Client,
) -> Result<Vec<SlackUserGroup>, Error> {
    let list_groups_request = slack_with_types::usergroups::ListRequest {
        include_count: None,
        include_disabled: None,
        include_users: Some(true),
        team_id: None,
    };

    let groups_list: slack_with_types::usergroups::ListResponse = client
        .post("usergroups.list", &list_groups_request)
        .await
        .context("Failed to list Slack usergroups")?;

    let groups_and_users = join_all(groups_list.usergroups.into_iter().map(
        |usergroup: UserGroup| async {
            if usergroup.user_count == 0 {
                return Ok((usergroup, Vec::new()));
            }

            let list_users_request = slack_with_types::usergroups::ListUsersRequest {
                usergroup: usergroup.id.clone(),
            };
            let users_list: slack_with_types::usergroups::ListUsersResponse = client
                .post("usergroups.users.list", &list_users_request)
                .await
                .with_context(|| {
                    format!(
                        "Failed to list users in group {}",
                        list_users_request.usergroup
                    )
                })?;
            Ok((usergroup, users_list.users))
        },
    ))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, Error>>()?;

    let users: BTreeSet<_> = groups_and_users
        .iter()
        .flat_map(|(_user_group, users)| users.iter().cloned())
        .collect();

    let users_by_id = join_all(users.into_iter().map(|user_id| async {
        let get_user_request = slack_with_types::users::GetUserInfoRequest {
            user: user_id.clone(),
        };

        let user: slack_with_types::users::GetUserInfoResponse = client
            .post("users.info", &get_user_request)
            .await
            .with_context(|| format!("Failed to get user with ID {}", get_user_request.user))?;
        Ok((user_id, user.user))
    }))
    .await
    .into_iter()
    .collect::<Result<BTreeMap<_, _>, Error>>()?;

    let groups = groups_and_users
        .into_iter()
        .map(
            |(
                UserGroup {
                    name, handle, id, ..
                },
                users,
            )| {
                SlackUserGroup {
                    name,
                    handle,
                    id,
                    // UNWRAP: By construction above.
                    members: users
                        .iter()
                        .map(|user_id| users_by_id.get(user_id).unwrap().clone())
                        .collect(),
                }
            },
        )
        .collect();

    Ok(groups)
}
