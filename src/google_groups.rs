use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use anyhow::Context;
use email_address::EmailAddress;
use futures::future::join_all;
use gsuite_api::{
    types::{Group, Member},
    Client, Response,
};
use http::Uri;
use tower_sessions::Session;

use crate::{
    google_auth::{make_redirect_uri, redirect_endpoint, GoogleScope},
    Error, ServerState,
};

pub async fn groups_client(
    session: &Session,
    server_state: ServerState,
    original_uri: Uri,
) -> Result<Client, Error> {
    let maybe_token: Option<String> = session
        .get(GoogleScope::Groups.token_session_key())
        .await
        .context("Session load error")?;

    let redirect_endpoint = redirect_endpoint(&server_state);

    if let Some(token) = maybe_token {
        let client = Client::new(
            server_state.config.google_apis_client_id.clone(),
            server_state.config.google_apis_client_secret.to_string(),
            &redirect_endpoint,
            token,
            "",
        );
        Ok(client)
    } else {
        Err(Error::Redirect(
            make_redirect_uri(
                &server_state,
                original_uri,
                &redirect_endpoint,
                GoogleScope::Groups,
            )
            .await?,
        ))
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GoogleGroup {
    pub email: EmailAddress,
    pub members: BTreeSet<EmailAddress>,
}

impl GoogleGroup {
    pub(crate) fn link(&self) -> String {
        let user = self.email.local_part();
        let domain = self.email.domain();
        format!("https://groups.google.com/a/{domain}/g/{user}")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct GoogleGroups {
    pub groups: BTreeSet<GoogleGroup>,
}

pub(crate) async fn get_groups(client: &Client) -> Result<GoogleGroups, Error> {
    let groups_response = client
        .groups()
        .list_all(
            "my_customer",
            "codeyourfuture.io",
            gsuite_api::types::DirectoryGroupsListOrderBy::Email,
            "",
            gsuite_api::types::SortOrder::Ascending,
            "",
        )
        .await
        .context("Failed to list Google groups")?;
    let groups = error_for_status(groups_response)?;
    let group_member_futures = groups
        .iter()
        .map(|Group { id, .. }| async { client.members().list_all(id, false, "").await })
        .collect::<Vec<_>>();
    let group_members = join_all(group_member_futures).await;

    let groups = groups
        .into_iter()
        .zip(group_members.into_iter())
        .map(|(group, members)| {
            let members =
                error_for_status(members.context("Failed to list Google group members")?)?;
            Ok(GoogleGroup {
                email: EmailAddress::from_str(&group.email).with_context(|| {
                    format!("Failed to parse group email address {}", group.email)
                })?,
                members: members
                    .into_iter()
                    .map(|Member { email, .. }| {
                        EmailAddress::from_str(&email).with_context(|| {
                            format!(
                                "Failed to parse group member email address {} (member of {})",
                                email, group.email
                            )
                        })
                    })
                    .collect::<Result<_, anyhow::Error>>()?,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(GoogleGroups { groups })
}

impl GoogleGroups {
    pub(crate) fn expand_recursively(&mut self) -> Result<(), anyhow::Error> {
        let mut index = BTreeMap::new();
        let groups = self
            .groups
            .iter()
            .map(|GoogleGroup { email, .. }| email.clone())
            .collect::<BTreeSet<_>>();
        for group in &self.groups {
            index.insert(group.email.clone(), group.members.clone());
        }
        let mut iteration = 0;
        loop {
            let mut changed = false;
            if iteration > 15 {
                return Err(anyhow::anyhow!("Reached recursion limit expanding groups"));
            }
            let mut to_replace: BTreeMap<
                EmailAddress,
                BTreeMap<EmailAddress, BTreeSet<EmailAddress>>,
            > = BTreeMap::new();
            for (group, members) in index.iter() {
                for member in members.iter() {
                    if groups.contains(member) {
                        to_replace
                            .entry(group.clone())
                            .or_default()
                            .insert(member.clone(), index.get(member).unwrap().clone());
                    }
                }
            }
            for (group, replacements) in to_replace {
                for (to_replace, replacements) in replacements {
                    index.get_mut(&group).unwrap().remove(&to_replace);
                    index.get_mut(&group).unwrap().extend(replacements);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
            iteration += 1;
        }
        self.groups = index
            .into_iter()
            .map(|(email, members)| GoogleGroup { email, members })
            .collect();
        Ok(())
    }
}

fn error_for_status<T: std::fmt::Debug>(response: Response<T>) -> Result<T, Error> {
    if !response.status.is_success() {
        Err(Error::Fatal(anyhow::anyhow!(
            "Got bad response from Google Groups API: {:?}",
            response
        )))
    } else {
        Ok(response.body)
    }
}

#[cfg(test)]
mod test {
    use email_address::EmailAddress;
    use maplit::btreeset;

    use crate::google_groups::{GoogleGroup, GoogleGroups};

    #[test]
    fn test_expand_recursively() {
        let outer_group = EmailAddress::new_unchecked("container@example.com");
        let inner_group = EmailAddress::new_unchecked("inner@example.com");
        let inner_members = btreeset![
            EmailAddress::new_unchecked("someone@example.com"),
            EmailAddress::new_unchecked("someone-else@example.com")
        ];
        let other_member = EmailAddress::new_unchecked("external@example.com");
        let all_members = btreeset![
            other_member.clone(),
            EmailAddress::new_unchecked("someone@example.com"),
            EmailAddress::new_unchecked("someone-else@example.com")
        ];

        let mut input = GoogleGroups {
            groups: btreeset![
                GoogleGroup {
                    email: outer_group.clone(),
                    members: btreeset![inner_group.clone(), other_member.clone()],
                },
                GoogleGroup {
                    email: inner_group.clone(),
                    members: inner_members.clone(),
                }
            ],
        };

        let want = GoogleGroups {
            groups: btreeset![
                GoogleGroup {
                    email: outer_group,
                    members: all_members,
                },
                GoogleGroup {
                    email: inner_group,
                    members: inner_members,
                }
            ],
        };

        input.expand_recursively().unwrap();
        assert_eq!(input, want);
    }
}
