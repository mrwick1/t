//! Optional per-repo settings in `<repo>/.agents/test.toml`.
//!
//! ```toml
//! [env]
//! PYTHONPATH = "{root}/shared:{root}/api"   # {root} is the repo root
//!
//! [runner]
//! python = "uv run --no-sync pytest"        # replaces the detected command
//!
//! [map]                                      # links that imports do not show
//! "rbac/catalog/**" = ["tests/rbac/**"]      # changed file glob = test file globs
//! ```
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize, Default, Debug)]
pub struct Config {
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub runner: BTreeMap<String, String>,
    #[serde(default)]
    pub map: BTreeMap<String, Vec<String>>,
}

impl Config {
    pub fn load(root: &Path) -> Result<Config, String> {
        let path = root.join(".agents/test.toml");
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(Config::default()) };
        let mut cfg: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let r = root.to_string_lossy();
        for v in cfg.env.values_mut() {
            *v = v.replace("{root}", &r);
        }
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_map() {
        let cfg: super::Config = toml::from_str("[map]\n\"rbac/**\" = [\"tests/rbac/**\"]\n").unwrap();
        assert_eq!(cfg.map["rbac/**"], ["tests/rbac/**"]);
    }
}
