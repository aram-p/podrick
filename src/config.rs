//! Global config. A deliberately tiny TOML subset (`key = "value"`, `#` comments) so
//! there is no TOML dependency and the file is still hand-editable.

use std::fs;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::store::config_dir;

/// How siblings are ordered. A closed set, so `render::cmp_rows` can match it without a
/// catch-all arm — adding a fifth ordering becomes a compile error in the one place that
/// has to handle it, rather than a silent no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    Priority,
    Due,
    Created,
    Alpha,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Priority, Sort::Due, Sort::Created, Sort::Alpha];

    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Priority => "priority",
            Sort::Due => "due",
            Sort::Created => "created",
            Sort::Alpha => "alpha",
        }
    }

    pub fn keys() -> String {
        Sort::ALL
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl std::fmt::Display for Sort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Sort {
    type Err = AppError;

    fn from_str(s: &str) -> std::result::Result<Sort, AppError> {
        Sort::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| {
                AppError::usage(format!(
                    "unknown sort key {s:?}; use one of {}",
                    Sort::keys()
                ))
            })
    }
}

#[derive(Debug, Default, Clone)]
pub struct Config {
    pub sort: Option<Sort>,
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = config_dir()?.join("config.toml");
        let Ok(text) = fs::read_to_string(path) else {
            return Ok(Config::default());
        };
        let mut cfg = Config::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if v.is_empty() {
                continue;
            }
            if k.trim() == "sort" {
                // Rejected, not ignored. A key that is wrong in the file behaves exactly
                // as it does on the command line — silently falling back to insertion
                // order gives two behaviours for one bad value.
                cfg.sort =
                    Some(Sort::from_str(&v).map_err(|e| {
                        e.with_hint("fix or remove the `sort` line in config.toml")
                    })?);
            }
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_dir()?.join("config.toml");
        let mut out = String::from("# podrick config\n");
        if let Some(s) = &self.sort {
            out.push_str(&format!("sort = \"{s}\"\n"));
        }
        fs::write(path, out)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_keys_are_the_documented_set() {
        assert_eq!(Sort::keys(), "priority, due, created, alpha");
    }

    #[test]
    fn sort_round_trips_through_its_string_form() {
        for k in Sort::ALL {
            assert_eq!(Sort::from_str(k.as_str()).unwrap(), k);
        }
        let e = Sort::from_str("banana").unwrap_err();
        assert_eq!(e.code, crate::error::Code::Usage);
        assert!(
            e.msg.contains("banana"),
            "the bad value is named: {}",
            e.msg
        );
    }

    /// The serde form is what lands in `registry.jsonl`, so it has to stay lowercase.
    #[test]
    fn sort_serialises_as_its_key() {
        assert_eq!(serde_json::to_string(&Sort::Alpha).unwrap(), "\"alpha\"");
    }
}
