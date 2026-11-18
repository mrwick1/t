//! t: run only the tests that cover a change.
//!
//!   t                      tests for the files changed against the base branch
//!   t <file>...            a test file runs as is; a source file runs the tests that cover it
//!   t --for <file>...      list the covering tests, run nothing
//!   t --dry <file>...      print the commands, run nothing
//!   t <file> -- <args>     pass extra arguments to the test runner
mod config;
mod find;

use find::{Repo, Target};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// Print a line; stop quietly when the reader is gone (for example `| head`).
macro_rules! out {
    ($($a:tt)*) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($a)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const USAGE: &str = "usage: t [--for] [--dry] [--max N] [file...] [-- runner args]";

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Files changed against the base branch, plus untracked files.
fn changed(root: &Path) -> Vec<String> {
    let base = ["origin/HEAD", "origin/main", "origin/master", "main", "master"]
        .iter()
        .find_map(|b| git(root, &["merge-base", "HEAD", b]))
        .unwrap_or_else(|| "HEAD".into());
    let mut files: BTreeSet<String> = BTreeSet::new();
    for out in [git(root, &["diff", "--name-only", &base]), git(root, &["ls-files", "--others", "--exclude-standard"])].into_iter().flatten() {
        files.extend(out.lines().map(str::to_string));
    }
    files.into_iter().filter(|f| root.join(f).is_file() && (find::lang(f).is_some() || find::is_data(f))).collect()
}

fn relative(root: &Path, cwd: &Path, arg: &str) -> Option<String> {
    let p = std::fs::canonicalize(cwd.join(arg)).ok()?;
    Some(p.strip_prefix(root).ok()?.to_string_lossy().to_string())
}

struct Job {
    cwd: PathBuf,
    argv: Vec<String>,
}

fn js_exec(root: &Path, dir: &Path) -> Vec<String> {
    let lock = [("pnpm-lock.yaml", "pnpm exec"), ("bun.lock", "bunx"), ("bun.lockb", "bunx"), ("yarn.lock", "yarn")]
        .iter()
        .find(|(f, _)| find::up_to(root, dir, f).is_some())
        .map(|(_, c)| *c)
        .unwrap_or("npx");
    lock.split(' ').map(String::from).collect()
}

fn jobs(root: &Path, cfg: &config::Config, targets: &[(Target, String)], extra: &[String]) -> Vec<Job> {
    let mut py: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut go: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut js: BTreeMap<(PathBuf, &str), Vec<String>> = BTreeMap::new();
    let mut out = Vec::new();
    for (t, _) in targets {
        match t {
            Target::Py(f) => {
                // Each top folder can be its own pytest root, so run them apart.
                let top = f.split('/').next().unwrap_or("").to_string();
                py.entry(top.clone()).or_default().push(f.clone());
            }
            Target::Rust { ws, pkg, test_bin, filter } => {
                let mut argv: Vec<String> = ["cargo", "test", "-q", "-p", pkg].map(String::from).to_vec();
                if let Some(b) = test_bin {
                    argv.extend(["--test".into(), b.clone()]);
                }
                if !filter.is_empty() {
                    argv.push(filter.clone());
                }
                argv.extend(extra.iter().cloned());
                out.push(Job { cwd: ws.clone(), argv});
            }
            Target::Go { module, pkg } => {
                go.entry(module.clone()).or_default().push(pkg.clone());
            }
            Target::Js { dir, tool, file } => {
                js.entry((dir.clone(), tool)).or_default().push(file.clone());
            }
        }
    }
    for (_, files) in py {
        let runner = cfg.runner.get("python").cloned().unwrap_or_else(|| {
            if root.join("uv.lock").exists() {
                // uv syncs first (inexact, so it never removes packages): a fresh worktree just works.
                "uv run pytest".into()
            } else if root.join(".venv/bin/pytest").exists() {
                ".venv/bin/pytest".into()
            } else {
                "python3 -m pytest".into()
            }
        });
        let mut argv: Vec<String> = runner.split_whitespace().map(String::from).collect();
        argv.extend(["-q".into(), "--tb=short".into()]);
        argv.extend(files.into_iter().chain(extra.iter().cloned()));
        out.push(Job { cwd: root.to_path_buf(), argv });
    }
    for (module, pkgs) in go {
        let mut argv: Vec<String> = vec!["go".into(), "test".into()];
        argv.extend(pkgs.into_iter().collect::<BTreeSet<_>>());
        argv.extend(extra.iter().cloned());
        out.push(Job { cwd: module, argv});
    }
    for ((dir, tool), files) in js {
        let mut argv = js_exec(root, &dir);
        argv.extend(match tool {
            "vitest" => vec!["vitest".into(), "run".into()],
            "playwright" => vec!["playwright".into(), "test".into()],
            _ => vec![tool.to_string()],
        });
        argv.extend(files.into_iter().chain(extra.iter().cloned()));
        out.push(Job { cwd: dir, argv});
    }
    out
}

/// One hint for a failure that comes from the environment, not from the code.
fn diagnose(lines: &[String]) -> Option<String> {
    for l in lines {
        if let Some(i) = l.find("No module named '") {
            let m = l[i + 17..].split('\'').next().unwrap_or("");
            return Some(format!("Python package `{m}` is missing from this environment. Install the dependencies (for example `uv sync --all-packages`), then run t again."));
        }
        if l.contains("out of shared memory") || l.contains("max_locks_per_transaction") {
            return Some("Postgres ran out of lock slots, often while it creates the test database. Raise max_locks_per_transaction (for example to 256) in postgresql.conf, then restart Postgres.".into());
        }
        if l.contains("could not resize shared memory segment") {
            return Some("Postgres has too little shared memory (/dev/shm). In Docker, raise shm_size (for example to 1g).".into());
        }
        if l.contains("Cannot find module") || l.contains("ERR_MODULE_NOT_FOUND") || l.contains("command not found: vitest") {
            return Some("node_modules is missing or stale. Run the package install (for example `pnpm install`), then run t again.".into());
        }
        if l.contains("Connection refused") || l.contains("ECONNREFUSED") || l.contains("could not connect to server") {
            let port = l.split(|c: char| !c.is_ascii_digit()).filter(|w| w.len() >= 4 && w.len() <= 5).last().unwrap_or("?");
            return Some(format!("a service on port {port} refused the connection. Start it (see the repo's local setup doc), or check the database URL in .env."));
        }
    }
    None
}

/// Run a job, stream its output and keep the last lines for `diagnose`.
fn run(job: &Job, env: &BTreeMap<String, String>) -> std::io::Result<(i32, Vec<String>)> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    let mut child = Command::new(&job.argv[0]).args(&job.argv[1..]).current_dir(&job.cwd).envs(env).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let pump = |r: Box<dyn std::io::Read + Send>, to_err: bool, tail: Arc<Mutex<Vec<String>>>| {
        std::thread::spawn(move || {
            for line in BufReader::new(r).lines().map_while(Result::ok) {
                let _ = if to_err { writeln!(std::io::stderr(), "{line}") } else { writeln!(std::io::stdout(), "{line}") };
                let mut t = tail.lock().unwrap();
                t.push(line);
                if t.len() > 400 {
                    t.remove(0);
                }
            }
        })
    };
    let a = pump(Box::new(child.stdout.take().unwrap()), false, tail.clone());
    let b = pump(Box::new(child.stderr.take().unwrap()), true, tail.clone());
    let status = child.wait()?;
    let _ = (a.join(), b.join());
    let lines = std::mem::take(&mut *tail.lock().unwrap());
    Ok((status.code().unwrap_or(1), lines))
}

/// In a linked worktree, name the env files that the primary checkout has and this one lacks.
fn missing_env_files(root: &Path) -> Vec<String> {
    let Some(common) = git(root, &["rev-parse", "--path-format=absolute", "--git-common-dir"]) else { return vec![] };
    let Some(primary) = Path::new(&common).parent() else { return vec![] };
    if primary == root {
        return vec![];
    }
    [".env", ".env.local", ".env.test"].iter().filter(|f| primary.join(f).exists() && !root.join(f).exists()).map(|f| primary.join(f).display().to_string()).collect()
}

fn shell_line(job: &Job, root: &Path) -> String {
    let cd = match job.cwd.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => String::new(),
        Ok(r) => format!("(in {}) ", r.display()),
        Err(_) => format!("(in {}) ", job.cwd.display()),
    };
    format!("{cd}{}", job.argv.join(" "))
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let extra: Vec<String> = match args.iter().position(|a| a == "--") {
        Some(i) => {
            let e = args[i + 1..].to_vec();
            args.truncate(i);
            e
        }
        None => vec![],
    };
    let (mut list, mut dry, mut max) = (false, false, 5usize);
    let mut inputs = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--for" => list = true,
            "--dry" => dry = true,
            "--max" => max = it.next().and_then(|n| n.parse().ok()).unwrap_or(max),
            "-h" | "--help" => {
                out!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => inputs.push(a),
        }
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    // Resolve symlinks so the root matches the canonical paths of the inputs.
    let Some(root) = git(&cwd, &["rev-parse", "--show-toplevel"]).and_then(|r| std::fs::canonicalize(r).ok()) else {
        eprintln!("t: not inside a git repository");
        return ExitCode::from(2);
    };
    let cfg = match config::Config::load(&root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("t: {e}");
            return ExitCode::from(2);
        }
    };
    let rels: Vec<String> = if inputs.is_empty() {
        let c = changed(&root);
        if c.is_empty() {
            out!("t: no changed code or data files against the base branch");
            return ExitCode::SUCCESS;
        }
        c
    } else {
        let mut v = Vec::new();
        for a in &inputs {
            match relative(&root, &cwd, a) {
                Some(r) => v.push(r),
                None => eprintln!("t: skip {a}: not a file in this repository"),
            }
        }
        v
    };

    let mut files = Vec::new();
    find::walk(&root, &root, &mut files);
    let repo = Repo { root: &root, files: &files };

    // Changed tests and tests that read changed data always run. Then fill up to the limit:
    // the best hit of each changed file first, then the next best.
    let per: Vec<(String, Vec<find::Hit>)> = rels.iter().map(|r| (r.clone(), repo.resolve(r))).collect();
    let mut chosen: Vec<(Target, String)> = Vec::new();
    let mut seen = BTreeSet::new();
    for h in per.iter().flat_map(|(_, h)| h).filter(|h| h.score == find::MUST) {
        if seen.insert(h.target.clone()) {
            chosen.push((h.target.clone(), h.show.clone()));
        }
    }
    if chosen.len() > max {
        out!("t: {} changed or required tests exceed --max {max}; all of them run.", chosen.len());
    }
    let deepest = per.iter().map(|(_, h)| h.len()).max().unwrap_or(0);
    for rank in 0..deepest {
        for (_, hits) in &per {
            if let Some(h) = hits.get(rank) {
                if chosen.len() < max && seen.insert(h.target.clone()) {
                    chosen.push((h.target.clone(), h.show.clone()));
                }
            }
        }
    }
    for (rel, hits) in &per {
        if hits.is_empty() {
            out!("{rel}: no covering test found. Write one, or leave it to CI.");
            continue;
        }
        out!("{rel}:");
        let shown = if list { max.max(15) } else { usize::MAX };
        for h in hits.iter().take(shown) {
            let mark = if chosen.iter().any(|(t, _)| *t == h.target) { "  run " } else { "  skip" };
            if mark == "  run " || list {
                out!("{mark} {}  ({})", h.show, h.why);
            }
        }
        if list && hits.len() > shown {
            out!("  ... {} more, weaker matches (raise --max to list them)", hits.len() - shown);
        }
        let skipped = hits.iter().filter(|h| !chosen.iter().any(|(t, _)| *t == h.target)).count();
        if skipped > 0 && !list {
            out!("  ... {skipped} more covering tests; CI runs them (or pass --max)");
        }
    }
    if list {
        return ExitCode::SUCCESS;
    }
    if chosen.is_empty() {
        // Not a pass: nothing ran.
        out!("t: no test selected; nothing ran.");
        return ExitCode::from(4);
    }

    if !dry {
        for f in missing_env_files(&root) {
            eprintln!("t: this worktree has no {}, but the primary checkout has {f}. Tests that read it may fail. Link or copy it only if the repo allows that.", Path::new(&f).file_name().unwrap().to_string_lossy());
        }
    }
    let mut code = 0u8;
    for job in jobs(&root, &cfg, &chosen, &extra) {
        out!("$ {}", shell_line(&job, &root));
        if dry {
            continue;
        }
        match run(&job, &cfg.env) {
            Ok((0, _)) => {}
            Ok((c, lines)) => {
                code = code.max(c.clamp(1, 255) as u8);
                if let Some(h) = diagnose(&lines) {
                    eprintln!("t: hint: {h}");
                }
            }
            Err(e) => {
                eprintln!("t: cannot run {}: {e}", job.argv[0]);
                code = code.max(2);
            }
        }
    }
    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::diagnose;

    fn hint(lines: &[&str]) -> Option<String> {
        diagnose(&lines.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn names_environment_failures() {
        assert!(hint(&["E   ModuleNotFoundError: No module named 'fastapi'"]).unwrap().contains("`fastapi`"));
        assert!(hint(&["psycopg2.OperationalError: connection to server at \"localhost\" (127.0.0.1), port 5432 failed: Connection refused"]).unwrap().contains("port 5432"));
        assert!(hint(&["Error: Cannot find module 'vitest/config'"]).unwrap().contains("node_modules"));
        assert!(hint(&["django.db.utils.OperationalError: out of shared memory", "HINT:  You might need to increase max_locks_per_transaction."]).unwrap().contains("max_locks_per_transaction"));
        assert!(hint(&["ERROR:  could not resize shared memory segment \"/PostgreSQL.1\" to 1048576 bytes: No space left on device"]).unwrap().contains("shm"));
        assert_eq!(hint(&["AssertionError: assert 1 == 2"]), None);
    }
}
