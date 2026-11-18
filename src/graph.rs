//! The import graph of Python and JS/TS files. The raw imports of each file are cached
//! between runs and parsed again only when the file's mtime changes.
use crate::find::lang;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub struct Graph {
    /// File -> the files that import it.
    importers: HashMap<String, Vec<String>>,
}

/// `$XDG_CACHE_HOME/t/<hash of root>.tsv`, else `~/.cache/t/...`.
pub fn cache_path(root: &Path) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    root.hash(&mut h);
    Some(base.join("t").join(format!("{:016x}.tsv", h.finish())))
}

impl Graph {
    pub fn build(root: &Path, files: &[String], cache: Option<&Path>) -> Graph {
        let specs = specs(root, files, cache);
        let set: HashSet<&str> = files.iter().map(String::as_str).collect();
        let mods = py_modules(files, &set);
        let mut aliases = HashMap::new();
        let mut importers: HashMap<String, Vec<String>> = HashMap::new();
        for (f, list) in &specs {
            let mut to: Vec<String> = Vec::new();
            for s in list {
                if f.ends_with(".py") {
                    let m = py_absolute(&py_module(f, &set), f.ends_with("__init__.py"), s);
                    to.extend(mods.get(m.as_str()).into_iter().flatten().cloned());
                } else if let Some(t) = js_resolve(root, &set, &mut aliases, f, s) {
                    to.push(t);
                }
            }
            to.sort();
            to.dedup();
            for t in to.into_iter().filter(|t| t != f) {
                importers.entry(t).or_default().push(f.clone());
            }
        }
        for v in importers.values_mut() {
            v.sort();
        }
        Graph { importers }
    }

    pub fn importers(&self, file: &str) -> &[String] {
        self.importers.get(file).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Raw import specifiers of each Python and JS file: from the cache when the mtime matches.
fn specs(root: &Path, files: &[String], cache: Option<&Path>) -> HashMap<String, Vec<String>> {
    let text = cache.and_then(|c| std::fs::read_to_string(c).ok()).unwrap_or_default();
    let mut old: HashMap<&str, (&str, Vec<String>)> = HashMap::new();
    for l in text.lines() {
        let mut parts = l.split('\t');
        if let (Some(f), Some(stamp)) = (parts.next(), parts.next()) {
            old.insert(f, (stamp, parts.map(String::from).collect()));
        }
    }
    let (mut out, mut lines, mut dirty) = (HashMap::new(), String::new(), false);
    for f in files.iter().filter(|f| matches!(lang(f), Some("py" | "js"))) {
        let Ok(m) = std::fs::metadata(root.join(f)).and_then(|m| m.modified()) else { continue };
        let stamp = m.duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0).to_string();
        let list = match old.remove(f.as_str()) {
            Some((s, list)) if s == stamp => list,
            _ => {
                dirty = true;
                let src = std::fs::read_to_string(root.join(f)).unwrap_or_default();
                if f.ends_with(".py") { py_specs(&src) } else { js_specs(&src) }
            }
        };
        lines.push_str(&format!("{f}\t{stamp}\t{}\n", list.join("\t")));
        out.insert(f.clone(), list);
    }
    if let Some(c) = cache.filter(|_| dirty || !old.is_empty()) {
        let _ = c.parent().map(std::fs::create_dir_all);
        let _ = std::fs::write(c, lines);
    }
    out
}

/// `import a.b` gives `a.b`; `from a import b, c` gives `a`, `a.b` and `a.c`. Relative names keep their dots.
fn py_specs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        let l = l.split('#').next().unwrap_or("").trim();
        if let Some(r) = l.strip_prefix("import ") {
            out.extend(r.split(',').filter_map(|p| p.split_whitespace().next()).map(String::from));
        } else if let Some((m, names)) = l.strip_prefix("from ").and_then(|r| r.split_once(" import ")) {
            let mut names = names.to_string();
            if names.trim_start().starts_with('(') && !names.contains(')') {
                for n in lines.by_ref() {
                    names.push_str(n.split('#').next().unwrap_or(""));
                    if n.contains(')') {
                        break;
                    }
                }
            }
            let m = m.trim();
            out.push(m.to_string());
            for n in names.split(',').filter_map(|n| n.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace()).split_whitespace().next()) {
                if n != "*" {
                    out.push(if m.ends_with('.') { format!("{m}{n}") } else { format!("{m}.{n}") });
                }
            }
        }
    }
    out
}

/// The quoted specifier after `from`, `import`, `import(`, `require(` and `mock(` (jest.mock, vi.mock).
fn js_specs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for key in ["from ", "import ", "import(", "require(", "mock("] {
        for (i, _) in text.match_indices(key) {
            let rest = text[i + key.len()..].trim_start();
            let Some(q) = rest.chars().next().filter(|c| matches!(c, '\'' | '"' | '`')) else { continue };
            if let Some(end) = rest[1..].find(q) {
                out.push(rest[1..1 + end].to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Module name of a Python file: the trailing folders that hold an `__init__.py`.
fn py_module(rel: &str, set: &HashSet<&str>) -> String {
    let parts: Vec<&str> = rel.trim_end_matches(".py").split('/').collect();
    let mut start = parts.len() - 1;
    while start > 0 && set.contains(format!("{}/__init__.py", parts[..start].join("/")).as_str()) {
        start -= 1;
    }
    let mut m = parts[start..].to_vec();
    if m.len() > 1 && m.last() == Some(&"__init__") {
        m.pop();
    }
    m.join(".")
}

fn py_modules(files: &[String], set: &HashSet<&str>) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for f in files.iter().filter(|f| f.ends_with(".py")) {
        out.entry(py_module(f, set)).or_default().push(f.clone());
    }
    out
}

/// Resolve a relative import (`.tax`, `..core.x`) against the importing module.
fn py_absolute(from: &str, is_init: bool, spec: &str) -> String {
    let dots = spec.chars().take_while(|c| *c == '.').count();
    if dots == 0 {
        return spec.into();
    }
    let mut parts: Vec<&str> = from.split('.').collect();
    if !is_init {
        parts.pop();
    }
    for _ in 1..dots {
        parts.pop();
    }
    if dots < spec.len() {
        parts.push(&spec[dots..]);
    }
    parts.join(".")
}

const JS_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "svelte"];

fn normalize(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            _ => out.push(p),
        }
    }
    out.join("/")
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn js_file(set: &HashSet<&str>, base: &str) -> Option<String> {
    if set.contains(base) && lang(base) == Some("js") {
        return Some(base.into());
    }
    let found = JS_EXTS.iter().map(|e| format!("{base}.{e}")).chain(JS_EXTS.iter().map(|e| format!("{base}/index.{e}"))).find(|c| set.contains(c.as_str()));
    // ESM source often imports `./x.js` for `x.ts`.
    found.or_else(|| base.strip_suffix(".js").and_then(|b| js_file(set, b)))
}

fn js_resolve(root: &Path, set: &HashSet<&str>, aliases: &mut HashMap<String, Vec<(String, String)>>, from: &str, spec: &str) -> Option<String> {
    if spec.starts_with('.') {
        return js_file(set, &normalize(&format!("{}/{spec}", dir_of(from))));
    }
    for (pat, target) in alias_rules(root, set, aliases, dir_of(from)) {
        let mapped = match pat.split_once('*') {
            Some((pre, post)) if spec.starts_with(pre) && spec.ends_with(post) && spec.len() >= pre.len() + post.len() => target.replacen('*', &spec[pre.len()..spec.len() - post.len()], 1),
            None if spec == pat => target,
            _ => continue,
        };
        if let Some(f) = js_file(set, &mapped) {
            return Some(f);
        }
    }
    None
}

const ALIAS_FILES: &[&str] = &["tsconfig.json", "tsconfig.app.json", "tsconfig.base.json", "jsconfig.json", "jest.config.js", "jest.config.ts", "jest.config.cjs", "jest.config.mjs", "package.json"];

/// Path aliases that apply in `dir`: tsconfig `paths` and jest `moduleNameMapper`, here and in every parent.
fn alias_rules(root: &Path, set: &HashSet<&str>, memo: &mut HashMap<String, Vec<(String, String)>>, dir: &str) -> Vec<(String, String)> {
    if let Some(r) = memo.get(dir) {
        return r.clone();
    }
    let mut rules = Vec::new();
    for name in ALIAS_FILES {
        let rel = if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
        if set.contains(rel.as_str()) {
            rules.extend(parse_aliases(dir, &std::fs::read_to_string(root.join(&rel)).unwrap_or_default()));
        }
    }
    if !dir.is_empty() {
        rules.extend(alias_rules(root, set, memo, dir_of(dir)));
    }
    memo.insert(dir.to_string(), rules.clone());
    rules
}

/// (pattern, repo-relative target) pairs. `*` in both stands for the matched rest.
fn parse_aliases(dir: &str, text: &str) -> Vec<(String, String)> {
    let text = strip_comments(text);
    let mut out = Vec::new();
    if let Some(block) = block_after(&text, "\"paths\"") {
        let base = string_after(&text, "\"baseUrl\"").unwrap_or_else(|| ".".into());
        for (k, v) in pairs(block) {
            out.push((k, normalize(&format!("{dir}/{base}/{v}"))));
        }
    }
    if let Some(block) = block_after(&text, "moduleNameMapper") {
        for (k, v) in pairs(block) {
            if let Some(p) = k.strip_prefix('^').and_then(|k| k.strip_suffix('$')).filter(|p| p.matches("(.*)").count() <= 1) {
                let v = v.replace("<rootDir>", "").replace("$1", "*");
                out.push((p.replace("(.*)", "*").replace('\\', ""), normalize(&format!("{dir}/{v}"))));
            }
        }
    }
    out
}

fn strip_comments(text: &str) -> String {
    let (mut out, mut quote) = (String::new(), None);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == '\\' {
                    out.extend(chars.next());
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '/' && chars.peek() == Some(&'/') => {
                chars.by_ref().find(|c| *c == '\n');
                out.push('\n');
            }
            None if c == '/' && chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
    }
    out
}

/// The inside of the `{...}` that follows `key:`.
fn block_after<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let rest = &text[text.find(key)? + key.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('{')?;
    let (mut depth, mut quote) = (1, None);
    let mut it = rest.char_indices();
    while let Some((i, c)) = it.next() {
        match (quote, c) {
            (Some(_), '\\') => {
                it.next();
            }
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{' | '[') => depth += 1,
            (None, '}' | ']') => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn string_after(text: &str, key: &str) -> Option<String> {
    let rest = &text[text.find(key)? + key.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let q = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    Some(rest[1..1 + rest[1..].find(q)?].to_string())
}

/// `"key": "value"` and `"key": ["value", ...]` pairs, first value only.
fn pairs(block: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut key: Option<String> = None;
    let mut it = block.char_indices();
    while let Some((i, c)) = it.next() {
        if c != '"' && c != '\'' {
            continue;
        }
        let mut s = String::new();
        let mut end = i;
        while let Some((j, d)) = it.next() {
            end = j;
            if d == '\\' {
                s.extend(it.next().map(|(_, e)| e));
            } else if d == c {
                break;
            } else {
                s.push(d);
            }
        }
        if block[end + 1..].trim_start().starts_with(':') {
            key = Some(s);
        } else if let Some(k) = key.take() {
            out.push((k, s));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_imports() {
        assert_eq!(py_specs("from a.b import (x,\n  y)  # c\nimport c.d as e, f\n"), ["a.b", "a.b.x", "a.b.y", "c.d", "f"]);
        assert_eq!(py_specs("from . import views\nfrom ..core import x\n"), [".", ".views", "..core", "..core.x"]);
        assert_eq!(py_absolute("billing.invoice", false, ".tax"), "billing.tax");
        assert_eq!(py_absolute("billing", true, ".tax"), "billing.tax");
        assert_eq!(py_absolute("a.b.c", false, "..x"), "a.x");
    }

    #[test]
    fn js_imports() {
        assert_eq!(js_specs("import a from './a';\nexport { b } from \"@/b\";\nconst c = require('c');\njest.mock('./d');\nawait import('./e');\nimport './f.css';\n"), ["./a", "./d", "./e", "./f.css", "@/b", "c"]);
    }

    #[test]
    fn cache_reparses_an_edited_file() {
        let root = crate::find::tests::fixture("cache", &[("a.py", ""), ("b.py", "")]);
        let cache = root.join("cache.tsv");
        let files: Vec<String> = vec!["a.py".into(), "b.py".into()];
        assert!(Graph::build(&root, &files, Some(&cache)).importers("a.py").is_empty());
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(root.join("b.py"), "import a\n").unwrap();
        assert_eq!(Graph::build(&root, &files, Some(&cache)).importers("a.py"), ["b.py"]);
        assert!(std::fs::read_to_string(&cache).unwrap().contains("b.py\t"));
    }

    #[test]
    fn aliases() {
        let ts = "{\n// x\n\"compilerOptions\": {\"baseUrl\": \"src\", \"paths\": {\"@/*\": [\"*\"], \"~cfg\": [\"./config/index.ts\"],}},}";
        assert_eq!(parse_aliases("web", ts), [("@/*".to_string(), "web/src/*".to_string()), ("~cfg".to_string(), "web/src/config/index.ts".to_string())]);
        let jest = "moduleNameMapper: {\n  '^@/(.*)$': '<rootDir>/src/$1',\n  '\\\\.(css)$': 'identity-obj-proxy',\n}";
        assert_eq!(parse_aliases("", jest), [("@/*".to_string(), "src/*".to_string())]);
    }
}
