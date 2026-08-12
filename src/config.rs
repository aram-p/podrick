//! Global config. A deliberately tiny TOML subset (`key = "value"`, `#` comments) so
//! there is no TOML dependency and the file is still hand-editable.

use std::fs;

use crate::error::Result;
use crate::store::config_dir;

#[derive(Debug, Default, Clone)]
pub struct Config {
    pub sort: Option<String>,
}

pub const SORT_KEYS: [&str; 4] = ["priority", "due", "created", "alpha"];

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
                cfg.sort = Some(v);
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
        assert_eq!(SORT_KEYS, ["priority", "due", "created", "alpha"]);
    }
}
