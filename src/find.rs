//! Map a changed file to the tests that cover it, from each language's own imports.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// pytest file, relative to the repo root.
    Py(String),
    /// cargo test in `ws` (workspace root): package, then `--test <name>` or a name filter.
    Rust { ws: PathBuf, pkg: String, test_bin: Option<String>, filter: String },
    /// go test package path such as `./internal/auth`, run in the module root.
    Go { module: PathBuf, pkg: String },
    /// vitest, jest or playwright file, relative to the package dir that owns the runner.
    Js { dir: PathBuf, tool: &'static str, file: String },
}

/// The score of a test that must run: a changed test file, or a test that reads a changed data file.
pub const MUST: usize = usize::MAX;

pub struct Hit {
    pub target: Target,
    /// The path to show and pass back to `t`.
    pub show: String,
    pub why: String,
    pub score: usize,
}

const SKIP: &[&str] = &[".git", "node_modules", "target", ".venv", "venv", "__pycache__", "dist", "build", ".next", "vendor", ".turbo", "coverage"];

/// Every file under `dir`, relative to `root`, without dependency and build folders.
pub fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if !SKIP.contains(&name.as_str()) && !name.starts_with(".venv") {
                walk(root, &e.path(), out);
            }
        } else if ft.is_file() {
            if let Ok(rel) = e.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

pub fn lang(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1;
    Some(match ext {
        "py" => "py",
        "rs" => "rs",
        "go" => "go",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "svelte" => "js",
        _ => return None,
    })
}

fn file_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn stem(p: &str) -> &str {
    let n = file_name(p);
    n.split('.').next().unwrap_or(n)
}

/// A file that tests can read as data. Docs, lock files and images are not.
pub fn is_data(path: &str) -> bool {
    matches!(path.rsplit_once('.').map(|(_, e)| e), Some("json" | "jsonl" | "yaml" | "yml" | "toml" | "csv" | "tsv" | "txt" | "sql" | "html" | "xml" | "snap"))
        && !path.ends_with("Cargo.toml") && !path.ends_with("pyproject.toml") && !path.ends_with("package.json")
}

pub fn is_test(path: &str) -> bool {
    let n = file_name(path);
    match lang(path) {
        Some("py") => n.starts_with("test_") || n.ends_with("_test.py"),
        Some("rs") => path.starts_with("tests/") || path.contains("/tests/") || n.ends_with("_tests.rs") || n == "tests.rs",
        Some("go") => n.ends_with("_test.go"),
        Some("js") => n.contains(".test.") || n.contains(".spec.") || path.contains("/__tests__/"),
        _ => false,
    }
}

/// Count `needle` as a whole word: no identifier character on either side.
pub fn word_count(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(needle)
        .filter(|(i, _)| {
            let before = hay[..*i].chars().next_back();
            let after = hay[i + needle.len()..].chars().next();
            !before.is_some_and(|c| ident(c) || c == '.') && !after.is_some_and(ident)
        })
        .count()
}

pub struct Repo<'a> {
    pub root: &'a Path,
    pub files: &'a [String],
}

impl Repo<'_> {
    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    /// The tests to run for one changed file, best first. A hit with score `MUST` always runs.
    pub fn resolve(&self, rel: &str) -> Vec<Hit> {
        let Some(l) = lang(rel) else { return self.cover_data(rel) };
        if is_test(rel) {
            return self.target(rel).map(|t| vec![Hit { target: t, show: rel.into(), why: "the test file itself".into(), score: MUST }]).unwrap_or_default();
        }
        let mut hits = match l {
            "py" => self.cover_py(rel),
            "rs" => self.cover_rs(rel),
            "go" => self.cover_go(rel),
            _ => self.cover_js(rel),
        };
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.show.cmp(&b.show)));
        hits
    }

    /// How to run one test file (or a Rust file that holds tests).
    pub fn target(&self, rel: &str) -> Option<Target> {
        match lang(rel)? {
            "py" => Some(Target::Py(rel.into())),
            "rs" => self.rust_target(rel),
            "go" => {
                let module = up_to(self.root, &self.root.join(rel), "go.mod")?;
                let dir = self.root.join(rel).parent()?.strip_prefix(&module).ok()?.to_string_lossy().to_string();
                Some(Target::Go { module, pkg: if dir.is_empty() { ".".into() } else { format!("./{dir}") } })
            }
            _ => {
                let abs = self.root.join(rel);
                let (dir, tool) = js_runner(self.root, &abs)?;
                let file = abs.strip_prefix(&dir).ok()?.to_string_lossy().to_string();
                Some(Target::Js { dir, tool, file })
            }
        }
    }

    /// A data file (a golden table, a fixture): the tests that name it.
    fn cover_data(&self, rel: &str) -> Vec<Hit> {
        let name = file_name(rel);
        self.files
            .iter()
            .filter(|f| lang(f).is_some() && is_test(f) && self.read(f).contains(name))
            .filter_map(|f| Some(Hit { target: self.target(f)?, show: f.clone(), why: format!("reads {name}"), score: MUST }))
            .collect()
    }

    fn cover_py(&self, rel: &str) -> Vec<Hit> {
        let module = py_module(self.root, rel);
        let s = stem(rel);
        let py: Vec<(&String, String)> = self.files.iter().filter(|f| f.ends_with(".py") && !f.contains("/.venv")).map(|f| (f, self.read(f))).collect();
        let mut hits: Vec<Hit> = Vec::new();
        for (f, text) in py.iter().filter(|(f, _)| is_test(f)) {
            let n = py_refs(text, &module);
            if n == 0 {
                continue;
            }
            let named = word_count(&stem(f).replace('_', " "), s) > 0;
            hits.push(Hit {
                target: Target::Py((*f).clone()),
                show: (*f).clone(),
                why: format!("imports {module}{}", if named { ", named after it" } else { "" }),
                score: 10 * n + if named { 1000 } else { 0 },
            });
        }
        // One level up: tests of the modules that import this one.
        let importers: Vec<String> = py.iter()
            .filter(|(f, text)| !is_test(f) && f.as_str() != rel && py_refs(text, &module) > 0)
            .map(|(f, _)| py_module(self.root, f))
            .take(30)
            .collect();
        for (f, text) in py.iter().filter(|(f, _)| is_test(f)) {
            if hits.iter().any(|h| h.show == **f) {
                continue;
            }
            if let Some(m) = importers.iter().find(|m| py_refs(text, m) > 0) {
                hits.push(Hit { target: Target::Py((*f).clone()), show: (*f).clone(), why: format!("imports {m}, which imports {module}"), score: 2 });
            }
        }
        self.add_named(rel, &mut hits, |f| f.ends_with(".py"));
        hits
    }

    /// Test files whose name shares a distinctive word with the source file name.
    fn add_named(&self, rel: &str, hits: &mut Vec<Hit>, same_kind: impl Fn(&str) -> bool) {
        const COMMON: &[&str] = &["test", "tests", "util", "utils", "common", "main", "base", "core", "types", "service", "services", "index", "helpers", "models", "config", "handler", "handlers", "client", "server", "agent", "call", "calls"];
        let words: Vec<&str> = stem(rel).split(['_', '-']).filter(|w| w.len() >= 4 && !COMMON.contains(w)).collect();
        if words.is_empty() {
            return;
        }
        for f in self.files.iter().filter(|f| same_kind(f) && is_test(f) && f.as_str() != rel) {
            if hits.iter().any(|h| h.show == *f) {
                continue;
            }
            let fw: Vec<&str> = stem(f).split(['_', '-']).collect();
            if let Some(w) = words.iter().find(|w| fw.contains(w)) {
                if let Some(t) = self.target(f) {
                    hits.push(Hit { target: t, show: f.clone(), why: format!("name shares \"{w}\""), score: 1 });
                }
            }
        }
    }

    fn rust_target(&self, rel: &str) -> Option<Target> {
        let abs = self.root.join(rel);
        let (crate_dir, pkg) = rust_crate(self.root, &abs)?;
        let ws = workspace_root(self.root, &crate_dir);
        let in_crate = abs.strip_prefix(&crate_dir).ok()?.to_string_lossy().to_string();
        if let Some(rest) = in_crate.strip_prefix("tests/") {
            let bin = rest.split('/').next()?.trim_end_matches(".rs").to_string();
            return Some(Target::Rust { ws, pkg, test_bin: Some(bin), filter: String::new() });
        }
        let filter = self.rust_modpath(&crate_dir, &in_crate);
        Some(Target::Rust { ws, pkg, test_bin: None, filter: if filter.is_empty() { filter } else { format!("{filter}::") } })
    }

    /// The module path of a file under `src/`, with `#[path]` includes resolved.
    fn rust_modpath(&self, crate_dir: &Path, in_crate: &str) -> String {
        let name = file_name(in_crate);
        let crate_rel = crate_dir.strip_prefix(self.root).unwrap_or(Path::new("")).to_string_lossy().to_string();
        let prefix = if crate_rel.is_empty() { String::new() } else { format!("{crate_rel}/") };
        for f in self.files.iter().filter(|f| f.starts_with(&format!("{prefix}src/")) && f.ends_with(".rs")) {
            let text = self.read(f);
            let mut lines = text.lines();
            while let Some(l) = lines.next() {
                let l = l.trim();
                if l.starts_with("#[path") && l.contains(&format!("\"{name}\"")) || l.starts_with("#[path") && l.ends_with(&format!("/{name}\"]")) {
                    let decl = lines.by_ref().map(str::trim).find(|l| !l.starts_with("#["));
                    if let Some(m) = decl.and_then(|d| d.split_whitespace().skip_while(|w| *w != "mod").nth(1)) {
                        let parent = self.rust_modpath(crate_dir, f.strip_prefix(&prefix).unwrap_or(f));
                        let m = m.trim_end_matches(';').trim_end_matches('{').trim();
                        return if parent.is_empty() { m.into() } else { format!("{parent}::{m}") };
                    }
                }
            }
        }
        let p = in_crate.strip_prefix("src/").unwrap_or(in_crate).trim_end_matches(".rs");
        let mut parts: Vec<&str> = p.split('/').collect();
        if matches!(parts.last(), Some(&"mod") | Some(&"lib") | Some(&"main")) {
            parts.pop();
        }
        if parts.len() == 1 && parts[0].is_empty() {
            return String::new();
        }
        parts.join("::")
    }

    fn cover_rs(&self, rel: &str) -> Vec<Hit> {
        let abs = self.root.join(rel);
        let Some((crate_dir, _)) = rust_crate(self.root, &abs) else { return vec![] };
        let mut hits = Vec::new();
        let own = self.read(rel);
        let mut s = stem(rel).to_string();
        if s == "mod" {
            s = abs.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        }
        if own.contains("#[test]") || own.contains("::test]") || own.contains("#[cfg(test)]") {
            if let Some(t) = self.rust_target(rel) {
                hits.push(Hit { target: t, show: rel.into(), why: "holds its own tests".into(), score: 1000 });
            }
        }
        if matches!(s.as_str(), "lib" | "main") {
            return hits;
        }
        let crate_rel = crate_dir.strip_prefix(self.root).unwrap_or(Path::new("")).to_string_lossy().to_string();
        let prefix = if crate_rel.is_empty() { String::new() } else { format!("{crate_rel}/") };
        let dir = rel.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
        for f in self.files.iter().filter(|f| f.starts_with(&prefix) && f.ends_with(".rs") && is_test(f) && *f != rel) {
            let sibling = f.starts_with(&dir) && (file_name(f).starts_with(&format!("{s}_")) || *f == format!("{dir}{s}/tests.rs"));
            let n = word_count(&self.read(f), &s);
            if !sibling && n == 0 {
                continue;
            }
            if let Some(t) = self.rust_target(f) {
                hits.push(Hit {
                    target: t,
                    show: f.clone(),
                    why: if sibling { format!("test file for {s}") } else { format!("names {s}") },
                    score: n + if sibling { 500 } else { 0 },
                });
            }
        }
        let crate_files = |f: &str| f.starts_with(&prefix) && f.ends_with(".rs");
        self.add_named(rel, &mut hits, crate_files);
        hits
    }

    fn cover_go(&self, rel: &str) -> Vec<Hit> {
        let dir = rel.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
        let has_tests = self.files.iter().any(|f| f.starts_with(&dir) && !f[dir.len()..].contains('/') && f.ends_with("_test.go"));
        if !has_tests {
            return vec![];
        }
        self.target(rel)
            .map(|t| vec![Hit { target: t, show: if dir.is_empty() { ".".into() } else { dir.clone() }, why: "tests in the same package".into(), score: 1000 }])
            .unwrap_or_default()
    }

    fn cover_js(&self, rel: &str) -> Vec<Hit> {
        let abs = self.root.join(rel);
        let Some((pkg_dir, _)) = js_runner(self.root, &abs) else { return vec![] };
        let pkg_rel = pkg_dir.strip_prefix(self.root).unwrap_or(Path::new("")).to_string_lossy().to_string();
        let prefix = if pkg_rel.is_empty() { String::new() } else { format!("{pkg_rel}/") };
        let mut s = stem(rel).to_string();
        if s == "index" {
            s = abs.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        }
        let dir = rel.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
        let mut hits = Vec::new();
        for f in self.files.iter().filter(|f| f.starts_with(&prefix) && lang(f) == Some("js") && is_test(f)) {
            let colocated = stem(f) == s && (f.starts_with(&dir) && !f[dir.len()..].contains('/') || f.starts_with(&format!("{dir}__tests__/")));
            let text = self.read(f);
            let n = text.match_indices(&format!("/{s}")).filter(|(i, _)| {
                let rest = &text[i + s.len() + 1..];
                rest.starts_with('\'') || rest.starts_with('"') || rest.starts_with('.') && rest[1..].chars().take_while(|c| c.is_alphanumeric()).count() <= 4 && rest[1..].trim_start_matches(char::is_alphanumeric).starts_with(['\'', '"'])
            }).count();
            if !colocated && n == 0 {
                continue;
            }
            if let Some(t) = self.target(f) {
                hits.push(Hit { target: t, show: f.clone(), why: if colocated { "test file next to it".into() } else { format!("imports {s}") }, score: n + if colocated { 500 } else { 0 } });
            }
        }
        self.add_named(rel, &mut hits, |f| f.starts_with(&prefix) && lang(f) == Some("js"));
        hits
    }
}

/// How often a Python file refers to `module`: dotted name or `from parent import name`.
fn py_refs(text: &str, module: &str) -> usize {
    let (parent, last) = module.rsplit_once('.').unwrap_or(("", module));
    let mut n = if module.contains('.') { word_count(text, module) } else { 0 };
    for l in text.lines().map(str::trim_start) {
        let hit = if parent.is_empty() {
            (l.starts_with("import ") || l.starts_with("from ")) && word_count(l, last) > 0
        } else {
            l.starts_with(&format!("from {parent} import")) && word_count(l, last) > 0
        };
        n += hit as usize;
    }
    n
}

/// Python module name: walk up while the folder is a package.
fn py_module(root: &Path, rel: &str) -> String {
    let abs = root.join(rel);
    let mut parts = vec![stem(rel).to_string()];
    let mut dir = abs.parent();
    while let Some(d) = dir {
        if d == root || !d.join("__init__.py").exists() {
            break;
        }
        parts.push(d.file_name().unwrap().to_string_lossy().to_string());
        dir = d.parent();
    }
    parts.reverse();
    if parts.last().map(String::as_str) == Some("__init__") {
        parts.pop();
    }
    parts.join(".")
}

/// The nearest folder at or above `from` (up to `root`) that holds `marker`.
pub fn up_to(root: &Path, from: &Path, marker: &str) -> Option<PathBuf> {
    let mut d = if from.is_dir() { Some(from) } else { from.parent() };
    while let Some(dir) = d {
        if dir.join(marker).exists() {
            return Some(dir.to_path_buf());
        }
        if dir == root {
            break;
        }
        d = dir.parent();
    }
    None
}

fn rust_crate(root: &Path, file: &Path) -> Option<(PathBuf, String)> {
    let mut d = file.parent();
    while let Some(dir) = d {
        if let Ok(text) = std::fs::read_to_string(dir.join("Cargo.toml")) {
            if let Ok(v) = text.parse::<toml::Table>() {
                if let Some(name) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str()) {
                    return Some((dir.to_path_buf(), name.to_string()));
                }
            }
        }
        if dir == root {
            break;
        }
        d = dir.parent();
    }
    None
}

fn workspace_root(root: &Path, crate_dir: &Path) -> PathBuf {
    up_to(root, crate_dir, "Cargo.lock").unwrap_or_else(|| crate_dir.to_path_buf())
}

/// The nearest package.json that lists a test runner, and which runner fits the file.
fn js_runner(root: &Path, file: &Path) -> Option<(PathBuf, &'static str)> {
    let name = file.file_name()?.to_string_lossy().to_string();
    let mut d = file.parent();
    while let Some(dir) = d {
        if let Ok(text) = std::fs::read_to_string(dir.join("package.json")) {
            let has = |dep: &str| text.contains(&format!("\"{dep}\""));
            let pw = has("@playwright/test");
            let tool = if pw && (name.contains(".spec.") && !has("vitest") || dir.to_string_lossy().contains("e2e")) {
                Some("playwright")
            } else if has("vitest") {
                Some("vitest")
            } else if has("jest") {
                Some("jest")
            } else if pw {
                Some("playwright")
            } else {
                None
            };
            if let Some(t) = tool {
                return Some((dir.to_path_buf(), t));
            }
        }
        if dir == root {
            break;
        }
        d = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_files() {
        assert!(is_data("agent/tests/number_capture_golden.jsonl"));
        assert!(is_data("fixtures/call.yaml"));
        assert!(!is_data("README.md"));
        assert!(!is_data("uv.lock"));
        assert!(!is_data("crates/x/Cargo.toml"));
        assert!(!is_data("web/package.json"));
    }

    #[test]
    fn counts_whole_words_only() {
        assert_eq!(word_count("from agent.pipeline import x; agent.pipeline_x", "agent.pipeline"), 1);
        assert_eq!(word_count("my.agent.pipeline", "agent.pipeline"), 0);
        assert_eq!(word_count("use crate::session;", "session"), 1);
    }

    #[test]
    fn spots_test_files() {
        assert!(is_test("agent/tests/test_ack_lane.py"));
        assert!(!is_test("agent/agent/pipeline.py"));
        assert!(is_test("crates/bare/src/browser_editing_tests.rs"));
        assert!(is_test("crates/drive/tests/fill.rs"));
        assert!(is_test("internal/auth/login_test.go"));
        assert!(is_test("src/lib/x.test.ts"));
        assert!(!is_test("src/lib/x.ts"));
    }
}

#[cfg(test)]
mod refs {
    use super::py_refs;

    #[test]
    fn finds_python_imports() {
        assert_eq!(py_refs("from agent.pipeline import build\n", "agent.pipeline"), 1);
        assert_eq!(py_refs("from agent import pipeline, flow\n", "agent.pipeline"), 1);
        assert_eq!(py_refs("monkeypatch.setattr('agent.pipeline.x', 1)\n", "agent.pipeline"), 1);
        assert_eq!(py_refs("from agent import pipeline_handlers\n", "agent.pipeline"), 0);
    }
}
