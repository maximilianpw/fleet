//! `--where` host filters shared by `status`, `pick`, `doctor`, and `agents`.
//!
//! An expression is a comma-separated AND of terms: `key=value`,
//! `key!=value`, a bare boolean `key`, or a negated boolean `!key`.

use thiserror::Error;

use crate::config::{FleetConfig, HostConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    Name,
    Os,
    Role,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagField {
    Gui,
    LongRunningAgents,
    ClientEnrolled,
    Local,
    Online,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    Text {
        field: TextField,
        value: String,
        negate: bool,
    },
    Flag {
        field: FlagField,
        expected: bool,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFilter {
    terms: Vec<Term>,
}

#[derive(Debug, Error)]
pub enum FilterError {
    #[error("fleet: unknown --where key '{0}' (use name, os, role, user, gui, long_running_agents, client_enrolled, local, online)")]
    UnknownKey(String),
    #[error("fleet: --where flag '{key}' expects true or false, got '{value}'")]
    NotBoolean { key: String, value: String },
    #[error("fleet: empty --where term")]
    EmptyTerm,
    #[error("fleet: --where online needs Tailscale status, which is unavailable")]
    OnlineUnknown,
}

impl FilterError {
    pub fn exit_code(&self) -> i32 {
        2
    }
}

impl HostFilter {
    pub fn parse(expression: &str) -> Result<Self, FilterError> {
        let mut terms = Vec::new();
        for raw in expression.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                return Err(FilterError::EmptyTerm);
            }
            terms.push(parse_term(raw)?);
        }
        Ok(Self { terms })
    }

    pub fn needs_online(&self) -> bool {
        self.terms.iter().any(|term| {
            matches!(
                term,
                Term::Flag {
                    field: FlagField::Online,
                    ..
                }
            )
        })
    }

    /// `online` is `None` when Tailscale presence is unknown. A filter that
    /// asks for `online` then fails rather than guessing.
    pub fn matches(
        &self,
        config: &FleetConfig,
        name: &str,
        host: &HostConfig,
        online: Option<bool>,
    ) -> Result<bool, FilterError> {
        for term in &self.terms {
            let ok = match term {
                Term::Text {
                    field,
                    value,
                    negate,
                } => {
                    let hit = match field {
                        TextField::Name => {
                            name == value || host.aliases.iter().any(|alias| alias == value)
                        }
                        TextField::Os => host.os.eq_ignore_ascii_case(value),
                        TextField::Role => host.role.eq_ignore_ascii_case(value),
                        TextField::User => host.user == *value,
                    };
                    hit != *negate
                }
                Term::Flag { field, expected } => {
                    let actual = match field {
                        FlagField::Gui => host.gui,
                        FlagField::LongRunningAgents => host.long_running_agents,
                        FlagField::ClientEnrolled => host.client_enrolled,
                        FlagField::Local => name == config.current_host,
                        FlagField::Online => online.ok_or(FilterError::OnlineUnknown)?,
                    };
                    actual == *expected
                }
            };
            if !ok {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn parse_term(raw: &str) -> Result<Term, FilterError> {
    if let Some((key, value)) = raw.split_once("!=") {
        return text_or_flag(key.trim(), value.trim(), true);
    }
    if let Some((key, value)) = raw.split_once('=') {
        return text_or_flag(key.trim(), value.trim(), false);
    }
    let (key, expected) = match raw.strip_prefix('!') {
        Some(key) => (key.trim(), false),
        None => (raw, true),
    };
    let field = flag_field(key).ok_or_else(|| FilterError::UnknownKey(key.to_string()))?;
    Ok(Term::Flag { field, expected })
}

fn text_or_flag(key: &str, value: &str, negate: bool) -> Result<Term, FilterError> {
    if let Some(field) = flag_field(key) {
        let expected = match value {
            "true" | "yes" | "1" => true,
            "false" | "no" | "0" => false,
            _ => {
                return Err(FilterError::NotBoolean {
                    key: key.to_string(),
                    value: value.to_string(),
                })
            }
        };
        return Ok(Term::Flag {
            field,
            expected: expected != negate,
        });
    }
    let field = match key {
        "name" | "host" => TextField::Name,
        "os" => TextField::Os,
        "role" => TextField::Role,
        "user" => TextField::User,
        _ => return Err(FilterError::UnknownKey(key.to_string())),
    };
    Ok(Term::Text {
        field,
        value: value.to_string(),
        negate,
    })
}

fn flag_field(key: &str) -> Option<FlagField> {
    Some(match key.replace('-', "_").as_str() {
        "gui" => FlagField::Gui,
        "long_running_agents" | "agents" => FlagField::LongRunningAgents,
        "client_enrolled" | "enrolled" => FlagField::ClientEnrolled,
        "local" => FlagField::Local,
        "online" => FlagField::Online,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;

    const TOML: &str = r#"
schema_version = 1
current_host = "laptop"

[hosts.laptop]
ssh_target = "laptop"
aliases = []
os = "darwin"
role = "interface"
user = "developer"
client_enrolled = true
gui = true
long_running_agents = false

[hosts.workbox]
ssh_target = "workbox"
aliases = ["dev"]
os = "linux"
role = "compute"
user = "developer"
client_enrolled = false
gui = false
long_running_agents = true
"#;

    fn names(filter: &str, online: Option<bool>) -> Result<Vec<String>, FilterError> {
        let config = parse_config(TOML).unwrap();
        let filter = HostFilter::parse(filter)?;
        let mut out = Vec::new();
        for (name, host) in &config.hosts {
            if filter.matches(&config, name, host, online)? {
                out.push(name.clone());
            }
        }
        Ok(out)
    }

    #[test]
    fn combines_terms_with_and() {
        assert_eq!(names("long_running_agents", None).unwrap(), ["workbox"]);
        assert_eq!(names("os=darwin,gui", None).unwrap(), ["laptop"]);
        assert_eq!(names("!local", None).unwrap(), ["workbox"]);
        assert_eq!(names("os!=linux", None).unwrap(), ["laptop"]);
        assert_eq!(names("name=dev", None).unwrap(), ["workbox"]);
        assert_eq!(
            names("long-running-agents=false", None).unwrap(),
            ["laptop"]
        );
        assert!(names("gui,!gui", None).unwrap().is_empty());
    }

    #[test]
    fn online_requires_presence() {
        assert!(matches!(
            names("online", None),
            Err(FilterError::OnlineUnknown)
        ));
        assert_eq!(names("online", Some(true)).unwrap().len(), 2);
    }

    #[test]
    fn rejects_unknown_keys_and_bad_booleans() {
        assert!(matches!(
            HostFilter::parse("color=blue"),
            Err(FilterError::UnknownKey(_))
        ));
        assert!(matches!(
            HostFilter::parse("gui=maybe"),
            Err(FilterError::NotBoolean { .. })
        ));
        assert!(matches!(
            HostFilter::parse("gui,,os=linux"),
            Err(FilterError::EmptyTerm)
        ));
    }
}
