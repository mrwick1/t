//! Django links that imports miss: tests that request a view's URL, and tests that use a signal's sender.
use crate::find::{is_test, word_count, Hit, Repo, Target};

impl Repo<'_> {
    pub(crate) fn add_django(&self, rel: &str, hits: &mut Vec<Hit>) {
        let tests: Vec<(&String, String)> = self.files.iter().filter(|f| f.ends_with(".py") && is_test(f)).map(|f| (f, self.read(f))).collect();
        // The view file itself, and the modules that import it (a serializer reaches URLs through its views).
        let mut views = vec![rel];
        views.extend(self.graph.importers(rel).iter().filter(|f| !is_test(f) && !f.ends_with("urls.py")).map(String::as_str));
        for v in views {
            let names = top_level_names(&self.read(v));
            let through = if v == rel { String::new() } else { format!(", through {v}") };
            for urls in self.graph.importers(v).iter().filter(|f| f.ends_with("urls.py")) {
                for (url_names, route) in url_entries(&self.read(urls), &names) {
                    for (f, text) in &tests {
                        let why = if let Some(n) = url_names.iter().find(|n| names_url(text, n)) {
                            format!("requests URL name \"{n}\" ({urls}{through})")
                        } else if let Some(r) = route.as_ref().filter(|r| text.contains(r.as_str())) {
                            format!("requests \"{r}\" ({urls}{through})")
                        } else {
                            continue;
                        };
                        push(hits, f, why, 5);
                    }
                }
            }
        }
        for model in signal_senders(&self.read(rel)) {
            for (f, text) in &tests {
                if word_count(text, &model) > 0 {
                    push(hits, f, format!("uses {model}, the sender of a signal here"), 3);
                }
            }
        }
    }
}

fn push(hits: &mut Vec<Hit>, f: &str, why: String, score: usize) {
    if !hits.iter().any(|h| h.show == f) {
        hits.push(Hit { target: Target::Py(f.into()), show: f.into(), why, score });
    }
}

/// Top-level `def` and `class` names.
fn top_level_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("class ").or_else(|| l.strip_prefix("def ")).or_else(|| l.strip_prefix("async def ")))
        .filter_map(|r| r.split(|c: char| !(c.is_alphanumeric() || c == '_')).next())
        .filter(|n| !n.is_empty())
        .map(String::from)
        .collect()
}

/// `name` as an identifier: `views.name` and `name.as_view()` count; `other_name` does not.
fn mentions(hay: &str, name: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    hay.match_indices(name).any(|(i, _)| !hay[..i].chars().next_back().is_some_and(ident) && !hay[i + name.len()..].chars().next().is_some_and(ident))
}

/// A URL name in quotes, alone or after a namespace: `"invoice-pdf"` or `'billing:invoice-pdf'`.
fn names_url(text: &str, name: &str) -> bool {
    ['"', '\''].iter().any(|q| text.contains(&format!("{q}{name}{q}")) || text.contains(&format!(":{name}{q}")))
}

fn quoted_after(text: &str, key: &str) -> Option<String> {
    let rest = text[text.find(key)? + key.len()..].trim_start();
    let q = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    Some(rest[1..1 + rest[1..].find(q)?].to_string())
}

/// For each `path(`, `re_path(`, `url(` and `router.register(` entry that names one of `views`:
/// its URL names and the fixed start of its route.
fn url_entries(text: &str, views: &[String]) -> Vec<(Vec<String>, Option<String>)> {
    let mut starts: Vec<(usize, bool)> = ["path(", "url(", "register("].iter().flat_map(|k| text.match_indices(k).map(move |(i, _)| (i, *k == "register("))).collect();
    starts.sort();
    let mut out = Vec::new();
    for (n, &(i, register)) in starts.iter().enumerate() {
        let chunk = &text[i..starts.get(n + 1).map(|s| s.0).unwrap_or(text.len())];
        if !views.iter().any(|v| mentions(chunk, v)) {
            continue;
        }
        let route = quoted_after(chunk, "(").map(|r| {
            let fixed = r.trim_start_matches('^').split(['<', '(', '?', '$', '[']).next().unwrap_or("").to_string();
            if register && !fixed.ends_with('/') { format!("{fixed}/") } else { fixed }
        });
        let route = route.filter(|r| r.len() >= 4 && r.chars().any(char::is_alphabetic));
        let mut names: Vec<String> = quoted_after(chunk, "name=").into_iter().collect();
        // DRF names router URLs `<basename>-list` and `-detail`. Without `basename=`, it takes the
        // model name, which is usually the viewset name without `ViewSet`.
        let base = quoted_after(chunk, "basename=").or_else(|| {
            register.then(|| views.iter().find(|v| mentions(chunk, v)).and_then(|v| v.strip_suffix("ViewSet")).map(str::to_lowercase)).flatten()
        });
        if let Some(b) = base.filter(|b| !b.is_empty()) {
            names = vec![format!("{b}-list"), format!("{b}-detail")];
        }
        out.push((names, route));
    }
    out
}

/// Models named in `@receiver(..., sender=X)` and `signal.connect(fn, sender=X)`.
fn signal_senders(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| l.contains("@receiver") || l.contains(".connect("))
        .filter_map(|l| l.split_once("sender=").map(|(_, r)| r))
        .filter_map(|r| r.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).next())
        .map(|m| m.rsplit('.').next().unwrap_or(m).to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_names_without_basename() {
        let urls = "router.register('provider-accounts', views.ProviderAccountViewSet)\n";
        assert_eq!(url_entries(urls, &["ProviderAccountViewSet".into()]), [(vec!["provideraccount-list".to_string(), "provideraccount-detail".to_string()], Some("provider-accounts/".to_string()))]);
    }
}
