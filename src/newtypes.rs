use std::{fmt::Display, str::FromStr};

use case_insensitive_string::CaseInsensitiveString;
use email_address::EmailAddress;
use serde::{Deserialize, Serialize};

pub fn new_case_insensitive_email_address(s: &str) -> Result<EmailAddress, email_address::Error> {
    EmailAddress::from_str(&s.to_ascii_lowercase())
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GithubLogin(CaseInsensitiveString);

impl Display for GithubLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<String> for GithubLogin {
    fn from(value: String) -> Self {
        GithubLogin(CaseInsensitiveString::from(value))
    }
}

impl From<CaseInsensitiveString> for GithubLogin {
    fn from(value: CaseInsensitiveString) -> Self {
        GithubLogin(value)
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Region(pub String);

impl Display for Region {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl Region {
    pub fn timezone(&self) -> chrono_tz::Tz {
        if self.0 == "South Africa" {
            chrono_tz::Africa::Johannesburg
        } else {
            chrono_tz::Europe::London
        }
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
