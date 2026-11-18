//! Optional per-repo settings in `<repo>/.agents/test.toml`.
//!
//! ```toml
//! [env]
//! PYTHONPATH = "{root}/shared:{root}/api"   # {root} is the repo root
//!
//! [runner]
//! python = "uv run --no-sync pytest"        # replaces the detected command
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
