use std::fmt::Display;

use case_insensitive_string::CaseInsensitiveString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Email(pub String);

impl Display for Email {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
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
}
