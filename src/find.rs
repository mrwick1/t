//! Map a changed file to the tests that cover it, from each language's own imports.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// pytest file, relative to the repo root.
    Py(String),
    /// cargo test in `ws` (workspace root): package, then `--test <name>` or a name filter.
    /// `lib`: the file belongs to the library, so `--lib` skips the doc tests and the `tests/` binaries.
    Rust { ws: PathBuf, pkg: String, test_bin: Option<String>, filter: String, lib: bool },
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
        && !file_name(path).ends_with("-lock.json") && !file_name(path).starts_with("tsconfig")
}

pub fn is_test(path: &str) -> bool {
    let n = file_name(path);
    match lang(path) {
        Some("py") => n.starts_with("test_") || n.ends_with("_test.py") || n == "tests.py" || n.ends_with("_tests.py"),
        Some("rs") => path.starts_with("tests/") || path.contains("/tests/") || n.ends_with("_tests.rs") || n == "tests.rs",
        Some("go") => n.ends_with("_test.go"),
        Some("js") => n.contains(".test.") || n.contains(".spec.") || path.contains("/__tests__/"),
        _ => false,
    }
}

/// Match a path against a glob: `**` is any number of folders, `*` and `?` stay inside one name.
pub fn glob(pat: &str, path: &str) -> bool {
    fn names(p: &[&str], s: &[&str]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(&"**") => (0..=s.len()).any(|i| names(&p[1..], &s[i..])),
            Some(x) => !s.is_empty() && name(x.as_bytes(), s[0].as_bytes()) && names(&p[1..], &s[1..]),
        }
    }
    fn name(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => (0..=s.len()).any(|i| name(&p[1..], &s[i..])),
            Some(b'?') => !s.is_empty() && name(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && name(&p[1..], &s[1..]),
        }
    }
    names(&pat.split('/').collect::<Vec<_>>(), &path.split('/').collect::<Vec<_>>())
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
    pub graph: &'a crate::graph::Graph,
}

impl Repo<'_> {
    pub(crate) fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    /// The tests to run for one changed file, best first. A hit with score `MUST` always runs.
    pub fn resolve(&self, rel: &str) -> Vec<Hit> {
        let Some(l) = lang(rel) else { return self.cover_data(rel) };
        if is_test(rel) {
            return self.target(rel).map(|t| vec![Hit { target: t, show: rel.into(), why: "the test file itself".into(), score: MUST }]).unwrap_or_default();
        }
        let mut hits = match l {
            "py" if file_name(rel) == "conftest.py" => self.cover_conftest(rel),
            "py" => self.cover_py(rel),
            "rs" => self.cover_rs(rel),
            "go" => self.cover_go(rel),
            _ => self.cover_js(rel),
        };
        if matches!(l, "py" | "js") {
            self.add_graph(rel, &mut hits);
        }
        if l == "py" && file_name(rel) != "conftest.py" {
            self.add_django(rel, &mut hits);
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.show.cmp(&b.show)));
        hits
    }

    /// `resolve`, plus the tests that `[map]` rules in `.agents/test.toml` link to `rel`.
    pub fn resolve_mapped(&self, rel: &str, map: &std::collections::BTreeMap<String, Vec<String>>) -> Vec<Hit> {
        let mut hits = self.resolve(rel);
        for (pat, tests) in map.iter().filter(|(p, _)| glob(p, rel)) {
            for f in self.files.iter().filter(|f| is_test(f) && f.as_str() != rel && tests.iter().any(|t| glob(t, f))) {
                let why = format!("mapped in .agents/test.toml by \"{pat}\"");
                match hits.iter_mut().find(|h| h.show == *f) {
                    Some(h) if h.score < 2000 => (h.score, h.why) = (2000, why),
                    Some(_) => {}
                    None => hits.extend(self.target(f).map(|t| Hit { target: t, show: f.clone(), why, score: 2000 })),
                }
            }
        }
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.show.cmp(&b.show)));
        hits
    }

    /// How to run one test file (or a Rust file that holds tests), or the tests in a folder.
    pub fn target(&self, rel: &str) -> Option<Target> {
        if self.root.join(rel).is_dir() {
            return self.folder_target(rel);
        }
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

    /// A folder: a Rust crate or module folder, or a folder of Python, Go or JS tests.
    fn folder_target(&self, rel: &str) -> Option<Target> {
        let abs = self.root.join(rel);
        if let Some((crate_dir, pkg)) = rust_crate(self.root, &abs.join("Cargo.toml")) {
            // A module folder runs like its module file; the crate root and src/ run the whole library.
            return match abs.strip_prefix(&crate_dir).ok()?.to_str()? {
                "" | "src" => Some(Target::Rust { ws: workspace_root(self.root, &crate_dir), pkg, test_bin: None, filter: String::new(), lib: crate_dir.join("src/lib.rs").exists() }),
                "tests" => Some(Target::Rust { ws: workspace_root(self.root, &crate_dir), pkg, test_bin: Some("*".into()), filter: String::new(), lib: false }),
                _ => self.rust_target(&format!("{rel}.rs")),
            };
        }
        let dir = format!("{rel}/");
        let first = self.files.iter().find(|f| f.starts_with(&dir) && is_test(f))?;
        match lang(first)? {
            "py" => Some(Target::Py(rel.into())),
            "go" => self.target(&format!("{rel}/_.go")),
            _ => {
                let (dir, tool) = js_runner(self.root, &abs.join("_"))?;
                Some(Target::Js { dir: dir.clone(), tool, file: abs.strip_prefix(&dir).ok()?.to_string_lossy().to_string() })
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

    /// Tests up to three imports away, through the import graph, with the chain as the reason.
    fn add_graph(&self, rel: &str, hits: &mut Vec<Hit>) {
        let mut next_hop: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        let mut seen: std::collections::HashSet<&str> = [rel].into();
        let mut frontier = vec![rel];
        for score in [10, 2, 1] {
            let mut next = Vec::new();
            for node in frontier {
                for f in self.graph.importers(node) {
                    if !seen.insert(f) {
                        continue;
                    }
                    next_hop.insert(f, node);
                    if !is_test(f) {
                        next.push(f.as_str());
                        continue;
                    }
                    if hits.iter().any(|h| h.show == *f) {
                        continue;
                    }
                    let mut chain = vec![node];
                    while let Some(n) = next_hop.get(chain[chain.len() - 1]) {
                        chain.push(n);
                    }
                    if let Some(t) = self.target(f) {
                        hits.push(Hit { target: t, show: f.clone(), why: format!("imports {}", chain.join(" → ")), score });
                    }
                }
            }
            frontier = next;
        }
    }

    /// A conftest.py: the tests at or below it that use its fixtures, or all of them for an autouse fixture.
    fn cover_conftest(&self, rel: &str) -> Vec<Hit> {
        let text = self.read(rel);
        let (mut names, mut autouse) = (Vec::new(), false);
        let mut lines = text.lines().map(str::trim);
        while let Some(l) = lines.next() {
            if !l.starts_with("@pytest.fixture") && !l.starts_with("@fixture") {
                continue;
            }
            autouse |= l.contains("autouse=True");
            for l in lines.by_ref() {
                autouse |= l.contains("autouse=True");
                if let Some(d) = l.trim_start_matches("async ").strip_prefix("def ") {
                    names.push(d.split('(').next().unwrap_or("").trim().to_string());
                    break;
                }
            }
        }
        let dir = rel.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
        self.files
            .iter()
            .filter(|f| f.starts_with(&dir) && f.ends_with(".py") && is_test(f))
            .filter_map(|f| {
                let t = self.read(f);
                let used: Vec<&String> = names.iter().filter(|n| word_count(&t, n) > 0).collect();
                if used.is_empty() && !autouse {
                    return None;
                }
                let why = used.first().map(|n| format!("uses fixture {n}")).unwrap_or_else(|| "under an autouse fixture".into());
                Some(Hit { target: Target::Py(f.clone()), show: f.clone(), why, score: 10 * used.len() + 1 })
            })
            .collect()
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
            return Some(Target::Rust { ws, pkg, test_bin: Some(bin), filter: String::new(), lib: false });
        }
        let filter = self.rust_modpath(&crate_dir, &in_crate);
        // A crate with lib.rs and main.rs: main.rs, src/bin/ and the modules main.rs declares are not in the library.
        let main = std::fs::read_to_string(crate_dir.join("src/main.rs")).unwrap_or_default();
        let lib = crate_dir.join("src/lib.rs").exists()
            && in_crate != "src/main.rs"
            && !in_crate.starts_with("src/bin/")
            && !main.lines().any(|l| l.trim_start().trim_start_matches("pub ").strip_prefix("mod ").is_some_and(|m| m.trim_end_matches([';', '{', ' ']) == stem(&in_crate)));
        Some(Target::Rust { ws, pkg, test_bin: None, filter: if filter.is_empty() { filter } else { format!("{filter}::") }, lib })
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
        // A crate root only declares the test modules of other files; count its own tests only.
        let root_file = matches!(s.as_str(), "lib" | "main");
        if own.contains("#[test]") || own.contains("::test]") || !root_file && own.contains("#[cfg(test)]") {
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
                    score: 100 + n + if sibling { 500 } else { 0 },
                });
            }
        }
        self.add_rust_hop(rel, &s, &prefix, &mut hits);
        if hits.is_empty() {
            let crate_files = |f: &str| f.starts_with(&prefix) && f.ends_with(".rs");
            self.add_named(rel, &mut hits, crate_files);
        }
        hits
    }

    /// Test files in the crate that name a module which names the changed module `s`.
    fn add_rust_hop(&self, rel: &str, s: &str, prefix: &str, hits: &mut Vec<Hit>) {
        let module = |f: &str| match stem(f) {
            "mod" => f.rsplit('/').nth(1).unwrap_or("").to_string(),
            n => n.to_string(),
        };
        let texts: Vec<(&String, String)> = self.files.iter().filter(|f| f.starts_with(prefix) && f.ends_with(".rs") && f.as_str() != rel).map(|f| (f, self.read(f))).collect();
        // A `mod x;` line only declares a module; it does not use it.
        let words = |t: &str| t.lines().filter(|l| !l.trim_start().trim_start_matches("pub ").trim_start_matches("pub(crate) ").starts_with("mod ")).flat_map(|l| l.split(|c: char| !c.is_alphanumeric() && c != '_').filter(|w| !w.is_empty()).map(str::to_string).collect::<Vec<_>>()).collect::<std::collections::HashSet<String>>();
        let sets: Vec<(&String, std::collections::HashSet<String>)> = texts.iter().map(|(f, t)| (*f, words(t))).collect();
        let near: Vec<String> = sets.iter().filter(|(f, w)| !is_test(f) && !matches!(stem(f), "lib" | "main") && w.contains(s)).map(|(f, _)| module(f)).filter(|m| m != s).collect();
        // A module that many tests name (a hub) says less than one that few tests name.
        let tests: Vec<&(&String, std::collections::HashSet<String>)> = sets.iter().filter(|(f, _)| is_test(f)).collect();
        let fan = |m: &String| tests.iter().filter(|(_, w)| w.contains(m)).count();
        let mut near: Vec<(String, usize)> = near.into_iter().map(|m| (fan(&m), m)).map(|(n, m)| (m, n)).collect();
        near.sort_by_key(|(_, n)| *n);
        for (f, w) in tests {
            if hits.iter().any(|h| h.show == **f) {
                continue;
            }
            if let Some((m, n)) = near.iter().find(|(m, _)| w.contains(m)) {
                if let Some(t) = self.target(f) {
                    hits.push(Hit { target: t, show: (*f).clone(), why: format!("names {m}, which names {s}"), score: 100 - n.min(&99) });
                }
            }
        }
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
pub(crate) mod tests {
    use super::*;

    #[test]
    fn data_files() {
        assert!(is_data("agent/tests/number_capture_golden.jsonl"));
        assert!(is_data("fixtures/call.yaml"));
        assert!(!is_data("README.md"));
        assert!(!is_data("uv.lock"));
        assert!(!is_data("crates/x/Cargo.toml"));
        assert!(!is_data("web/package.json"));
        assert!(!is_data("web/package-lock.json"));
        assert!(!is_data("bun.lock"));
        assert!(!is_data("deno.lock"));
        assert!(!is_data("web/tsconfig.json"));
        assert!(!is_data("tsconfig.base.json"));
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
        assert!(is_test("billing/tests.py"));
        assert!(is_test("billing/invoice_tests.py"));
        assert!(!is_test("billing/tests/factories.py"));
        assert!(!is_test("billing/conftest.py"));
    }

    /// A throwaway repo on disk: (relative path, contents) pairs.
    pub fn fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("t-fixture-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, text) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        root
    }

    pub fn shows(root: &Path, rel: &str) -> Vec<String> {
        let mut files = Vec::new();
        walk(root, root, &mut files);
        let graph = crate::graph::Graph::build(root, &files, None);
        Repo { root, files: &files, graph: &graph }.resolve(rel).into_iter().map(|h| h.show).collect()
    }

    #[test]
    fn conftest_selects_tests_that_use_its_fixtures() {
        let root = fixture("conftest", &[
            ("billing/conftest.py", "import pytest\n\n@pytest.fixture\ndef invoice():\n    return 1\n"),
            ("billing/test_a.py", "def test_a(invoice):\n    assert invoice\n"),
            ("billing/sub/test_b.py", "def test_b(invoice):\n    assert invoice\n"),
            ("billing/test_c.py", "def test_c():\n    assert True\n"),
            ("other/test_d.py", "def test_d(invoice):\n    assert invoice\n"),
        ]);
        assert_eq!(shows(&root, "billing/conftest.py"), ["billing/sub/test_b.py", "billing/test_a.py"]);
    }

    #[test]
    fn autouse_conftest_selects_every_test_below_it() {
        let root = fixture("autouse", &[
            ("billing/conftest.py", "import pytest\n\n@pytest.fixture(autouse=True)\ndef tenant():\n    yield\n"),
            ("billing/test_a.py", "def test_a():\n    assert True\n"),
            ("other/test_d.py", "def test_d():\n    assert True\n"),
        ]);
        assert_eq!(shows(&root, "billing/conftest.py"), ["billing/test_a.py"]);
    }

    pub fn whys(root: &Path, rel: &str) -> Vec<(String, String)> {
        let mut files = Vec::new();
        walk(root, root, &mut files);
        let graph = crate::graph::Graph::build(root, &files, None);
        Repo { root, files: &files, graph: &graph }.resolve(rel).into_iter().map(|h| (h.show, h.why)).collect()
    }

    #[test]
    fn python_tests_three_imports_away() {
        let root = fixture("py-deep", &[
            ("api/__init__.py", ""),
            ("api/urls.py", "from api import views\n"),
            ("api/views.py", "from billing.tax import rate\n"),
            ("billing/__init__.py", ""),
            ("billing/tax.py", "def rate():\n    return 1\n"),
            ("billing/invoice.py", "from .tax import rate\n"),
            ("tests/test_api.py", "from api.urls import urlpatterns\n"),
            ("tests/test_invoice.py", "from billing import invoice\n"),
        ]);
        let got = whys(&root, "billing/tax.py");
        assert!(got.contains(&("tests/test_invoice.py".into(), "imports billing/invoice.py → billing/tax.py".into())), "{got:?}");
        assert!(got.contains(&("tests/test_api.py".into(), "imports api/urls.py → api/views.py → billing/tax.py".into())), "{got:?}");
    }

    #[test]
    fn js_alias_through_a_barrel() {
        let root = fixture("js-barrel", &[
            ("package.json", "{\"devDependencies\": {\"jest\": \"29\"}}"),
            ("tsconfig.json", "{\n  // aliases\n  \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@/*\": [\"./src/*\"],},},\n}"),
            ("src/components/Button.tsx", "export const Button = () => null;\n"),
            ("src/components/index.ts", "export { Button } from './Button';\n"),
            ("src/pages/Home.test.tsx", "import { Button } from '@/components';\n"),
        ]);
        assert_eq!(whys(&root, "src/components/Button.tsx"), [("src/pages/Home.test.tsx".to_string(), "imports src/components/index.ts → src/components/Button.tsx".to_string())]);
    }

    #[test]
    fn jest_module_name_mapper_alias() {
        let root = fixture("js-mapper", &[
            ("package.json", "{\"devDependencies\": {\"jest\": \"29\"}}"),
            ("jest.config.js", "module.exports = {\n  moduleNameMapper: { '^~/(.*)$': '<rootDir>/src/$1' },\n};\n"),
            ("src/lib/money.ts", "export const fmt = 1;\n"),
            ("src/lib/price.ts", "import { fmt } from '~/lib/money';\n"),
            ("src/checkout.test.ts", "jest.mock('~/lib/price');\nimport { x } from './lib/price';\n"),
        ]);
        assert_eq!(shows(&root, "src/lib/money.ts"), ["src/checkout.test.ts"]);
    }

    fn django_fixture(name: &str) -> PathBuf {
        fixture(name, &[
            ("billing/__init__.py", ""),
            ("billing/models.py", "class Invoice:\n    pass\n"),
            ("billing/views.py", "class InvoiceViewSet:\n    pass\n\ndef invoice_pdf(request, pk):\n    pass\n"),
            ("billing/serializers.py", "class InvoiceSerializer:\n    pass\n"),
            ("billing/api.py", "from billing.serializers import InvoiceSerializer\n\nclass InvoiceExport:\n    pass\n"),
            ("billing/urls.py", "from django.urls import path\nfrom billing import views\nfrom billing.api import InvoiceExport\n\nrouter.register(r\"invoices\", views.InvoiceViewSet, basename=\"invoice\")\nurlpatterns = [\n    path(\n        \"invoices/<int:pk>/pdf/\",\n        views.invoice_pdf,\n        name=\"invoice-pdf\",\n    ),\n    path(\"export/\", InvoiceExport.as_view(), name=\"invoice-export\"),\n]\n"),
            ("billing/signals.py", "from billing.models import Invoice\n\n@receiver(post_save, sender=Invoice)\ndef notify(sender, instance, **kw):\n    pass\n"),
            ("tests/test_list.py", "def test_list(client):\n    client.get(reverse(\"invoice-list\"))\n"),
            ("tests/test_pdf.py", "def test_pdf(client):\n    client.get(\"/api/invoices/3/pdf/\")\n"),
            ("tests/test_ns.py", "def test_ns(client):\n    client.get(reverse('billing:invoice-pdf', args=[1]))\n"),
            ("tests/test_export.py", "def test_export(client):\n    client.get(reverse(\"invoice-export\"))\n"),
            ("tests/test_model.py", "from billing.models import Invoice\n"),
            ("tests/test_other.py", "def test_other(client):\n    client.get(reverse(\"customer-list\"))\n"),
        ])
    }

    #[test]
    fn django_view_selects_tests_that_request_its_urls() {
        let mut got = shows(&django_fixture("dj-views"), "billing/views.py");
        got.sort();
        assert_eq!(got, ["tests/test_list.py", "tests/test_ns.py", "tests/test_pdf.py"]);
    }

    #[test]
    fn django_url_link_through_an_imported_module() {
        let got = whys(&django_fixture("dj-ser"), "billing/serializers.py");
        assert_eq!(got, [("tests/test_export.py".to_string(), "requests URL name \"invoice-export\" (billing/urls.py, through billing/api.py)".to_string())]);
    }

    #[test]
    fn django_signal_selects_tests_that_use_its_sender() {
        assert_eq!(shows(&django_fixture("dj-signals"), "billing/signals.py"), ["tests/test_model.py"]);
    }

    #[test]
    fn globs() {
        assert!(glob("rbac/catalog/**", "rbac/catalog/x/y.py"));
        assert!(glob("rbac/catalog/**", "rbac/catalog/a.yaml"));
        assert!(!glob("rbac/*.py", "rbac/a/b.py"));
        assert!(glob("rbac/*.py", "rbac/b.py"));
        assert!(glob("**/filters.py", "tenancy/filters.py"));
        assert!(glob("**/filters.py", "filters.py"));
        assert!(glob("core/**/tenant_*.py", "core/tenant_scope.py"));
        assert!(!glob("core/**/tenant_*.py", "core/a/tenant/x.py"));
    }

    #[test]
    fn map_rules_select_tests_for_any_file() {
        let root = fixture("map", &[
            ("rbac/catalog.yaml", "roles: []\n"),
            ("tests/rbac/test_perms.py", "def test_p():\n    pass\n"),
            ("tests/rbac/helpers.py", "X = 1\n"),
            ("tests/test_other.py", "def test_o():\n    pass\n"),
        ]);
        let mut files = Vec::new();
        walk(&root, &root, &mut files);
        let graph = crate::graph::Graph::build(&root, &files, None);
        let map = std::collections::BTreeMap::from([("rbac/**".to_string(), vec!["tests/rbac/**".to_string()])]);
        let got: Vec<(String, String)> = Repo { root: &root, files: &files, graph: &graph }.resolve_mapped("rbac/catalog.yaml", &map).into_iter().map(|h| (h.show, h.why)).collect();
        assert_eq!(got, [("tests/rbac/test_perms.py".to_string(), "mapped in .agents/test.toml by \"rbac/**\"".to_string())]);
    }

    #[test]
    fn django_tests_py_imports_count() {
        let root = fixture("django", &[
            ("billing/__init__.py", ""),
            ("billing/invoice.py", "def total():\n    return 1\n"),
            ("billing/tests.py", "from billing.invoice import total\n"),
        ]);
        assert_eq!(shows(&root, "billing/invoice.py"), ["billing/tests.py"]);
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

    #[test]
    fn lib_rs_that_only_declares_test_modules_holds_no_tests() {
        let root = super::tests::fixture("rs-lib", &[
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("src/lib.rs", "pub mod dom;\n#[cfg(test)]\nmod dom_tests;\n"),
            ("src/dom.rs", "pub fn f() {}\n"),
            ("src/dom_tests.rs", "#[test]\nfn a() { crate::dom::f() }\n"),
        ]);
        assert!(!super::tests::shows(&root, "src/lib.rs").contains(&"src/lib.rs".to_string()));
    }

    #[test]
    fn rust_tests_two_modules_away_beat_shared_name_words() {
        let root = super::tests::fixture("rs-hop", &[
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("src/lib.rs", "pub mod browser_dom;\npub mod browser_query;\n"),
            ("src/browser_dom.rs", "pub struct Node;\n"),
            ("src/browser_query.rs", "use crate::browser_dom::Node;\n"),
            ("src/browser_query_tests.rs", "use crate::browser_query::find;\n#[test]\nfn a() {}\n"),
            ("src/browser_abort_tests.rs", "use crate::browser::x;\n#[test]\nfn a() {}\n"),
            ("src/browser.rs", "mod browser_dom;\npub fn x() {}\n"),
        ]);
        let got = super::tests::whys(&root, "src/browser_dom.rs");
        assert_eq!(got, [("src/browser_query_tests.rs".to_string(), "names browser_query, which names browser_dom".to_string())]);
    }

    #[test]
    fn folders_are_targets() {
        use super::*;
        let root = super::tests::fixture("dirs", &[
            ("Cargo.lock", ""),
            ("crates/x/Cargo.toml", "[package]\nname = \"x\"\n"),
            ("crates/x/src/lib.rs", "pub mod css;\n"),
            ("crates/x/src/css/mod.rs", "#[test]\nfn t() {}\n"),
            ("crates/x/tests/a.rs", "#[test]\nfn t() {}\n"),
            ("api/tests/test_a.py", "def test_a():\n    pass\n"),
        ]);
        let mut files = Vec::new();
        walk(&root, &root, &mut files);
        let graph = crate::graph::Graph::build(&root, &files, None);
        let repo = Repo { root: &root, files: &files, graph: &graph };
        let rust = |f: &str, want: &str| matches!(repo.target(f), Some(Target::Rust { filter, lib: true, .. }) if filter == want);
        assert!(rust("crates/x", ""));
        assert!(rust("crates/x/src", ""));
        assert!(rust("crates/x/src/css", "css::"));
        assert!(matches!(repo.target("crates/x/tests"), Some(Target::Rust { test_bin: Some(b), lib: false, .. }) if b == "*"));
        assert_eq!(repo.target("api/tests"), Some(Target::Py("api/tests".into())));
        assert_eq!(repo.target("crates"), None);
    }

    #[test]
    fn rust_src_files_run_only_the_library_tests() {
        use super::*;
        let root = super::tests::fixture("rs-lib-flag", &[
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("src/lib.rs", "pub mod a;\n"),
            ("src/main.rs", "mod cli;\nfn main() {}\n"),
            ("src/a.rs", "#[test]\nfn t() {}\n"),
            ("src/cli.rs", "#[test]\nfn t() {}\n"),
        ]);
        let mut files = Vec::new();
        walk(&root, &root, &mut files);
        let graph = crate::graph::Graph::build(&root, &files, None);
        let repo = Repo { root: &root, files: &files, graph: &graph };
        let lib = |f: &str| matches!(repo.target(f), Some(Target::Rust { lib, .. }) if lib);
        assert!(lib("src/a.rs"));
        assert!(!lib("src/cli.rs"));
        assert!(!lib("src/main.rs"));
    }
}
