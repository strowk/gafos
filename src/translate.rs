//! Translates parsed OpenAPI operations into HTTPRoute match rules.
//!
//! Pure functions only: no I/O, no config file access. `build_rules` is the
//! entry point downstream tasks call to turn `openapi::Operation`s plus a
//! resolved `config::Config` into `GeneratedRule`s ready for rendering.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};

use crate::config::{Config, PathMatch};
use crate::openapi::Operation;

/// Gateway API's per-rule match limit; matches are chunked into
/// `GeneratedRule`s of at most this many entries.
const MAX_MATCHES_PER_RULE: usize = 64;

/// How a `PathRule`'s value is matched against the request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathMatchKind {
    Exact,
    RegularExpression,
    PathPrefix,
}

/// A path match: how to compare, and the value to compare against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRule {
    pub kind: PathMatchKind,
    pub value: String,
}

/// A query or header parameter whose mere presence is required (value is
/// unconstrained; renders as the regex `.+`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamPresence {
    pub name: String,
}

/// One HTTPRoute match: a path rule plus optional method/query/header
/// constraints, per the enabled dimensions in `Config`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: PathRule,
    pub method: Option<String>,
    pub query: Vec<ParamPresence>,
    pub headers: Vec<ParamPresence>,
}

/// A group of at most `MAX_MATCHES_PER_RULE` matches.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeneratedRule {
    pub matches: Vec<Match>,
}

/// Translate one OpenAPI path into a `PathRule` under the given strategy.
///
/// A path with no `{param}` segments is always `Exact`. A templated path is
/// `RegularExpression` (anchored `^...$`, each `{param}` replaced by
/// `[^/]+`, literal runs regex-escaped) under `PathMatch::Regex`, or
/// `PathPrefix` (the literal prefix up to the first `{`) under
/// `PathMatch::Prefix`.
pub fn path_rule(path: &str, strategy: PathMatch) -> Result<PathRule> {
    if !path.contains('{') {
        return Ok(PathRule {
            kind: PathMatchKind::Exact,
            value: path.to_string(),
        });
    }

    match strategy {
        PathMatch::Prefix => {
            let idx = path.find('{').expect("templated path checked above");
            Ok(PathRule {
                kind: PathMatchKind::PathPrefix,
                value: path[..idx].to_string(),
            })
        }
        PathMatch::Regex => {
            let mut value = String::from("^");
            let mut rest = path;
            while let Some(start) = rest.find('{') {
                value.push_str(&escape_regex(&rest[..start]));
                value.push_str("[^/]+");
                let after_open = &rest[start..];
                let end = match after_open.find('}') {
                    Some(end) => end,
                    None => bail!("invalid path template {path:?}: unterminated '{{'"),
                };
                rest = &after_open[end + 1..];
            }
            value.push_str(&escape_regex(rest));
            value.push('$');
            Ok(PathRule {
                kind: PathMatchKind::RegularExpression,
                value,
            })
        }
    }
}

/// Regex-escape a literal run of a path template (never contains `{`/`}`,
/// those are consumed as template delimiters by the caller). `/` is left
/// unescaped: it has no special meaning in the produced regex.
fn escape_regex(literal: &str) -> String {
    let mut out = String::with_capacity(literal.len());
    for c in literal.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '^' | '$' | '|' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Build route rules from parsed operations under the resolved config.
///
/// `cfg.base_path` (if set) is prepended to every operation's path before
/// translation. One `Match` is built per operation, applying whichever
/// dimensions are enabled (`match_methods`, `match_query`,
/// `match_headers`). When `match_methods` is off, operations that
/// translate to the same path are collapsed into a single match (their
/// query/header presence lists are merged). Matches are sorted by (path
/// value, method) for deterministic output, then chunked into
/// `GeneratedRule`s of at most `MAX_MATCHES_PER_RULE` matches.
pub fn build_rules(ops: &[Operation], cfg: &Config) -> Result<Vec<GeneratedRule>> {
    let mut matches: Vec<Match> = if cfg.match_methods {
        ops.iter()
            .map(|op| {
                let full_path = prepend_base_path(cfg.base_path.as_deref(), &op.path);
                Ok(Match {
                    path: path_rule(&full_path, cfg.path_match)?,
                    method: Some(op.method.clone()),
                    query: if cfg.match_query {
                        dedupe_presence(&op.required_query)
                    } else {
                        Vec::new()
                    },
                    headers: if cfg.match_headers {
                        dedupe_presence(&op.required_header)
                    } else {
                        Vec::new()
                    },
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        // Keyed on the whole (kind, value) pair, not just the string value:
        // an `Exact` path and a `PathPrefix` path can render to the same
        // string (e.g. a static "/users/" op alongside a templated
        // "/users/{id}" op under prefix-match strategy) and must not merge
        // into one match.
        let mut by_path: BTreeMap<(PathMatchKind, String), Match> = BTreeMap::new();
        for op in ops {
            let full_path = prepend_base_path(cfg.base_path.as_deref(), &op.path);
            let path = path_rule(&full_path, cfg.path_match)?;
            let key = (path.kind, path.value.clone());
            let entry = by_path.entry(key).or_insert_with(|| Match {
                path,
                method: None,
                query: Vec::new(),
                headers: Vec::new(),
            });
            if cfg.match_query {
                merge_presence(&mut entry.query, &op.required_query);
            }
            if cfg.match_headers {
                merge_presence(&mut entry.headers, &op.required_header);
            }
        }
        by_path.into_values().collect()
    };

    matches.sort_by(|a, b| {
        a.path
            .value
            .cmp(&b.path.value)
            .then_with(|| a.method.cmp(&b.method))
    });

    Ok(matches
        .chunks(MAX_MATCHES_PER_RULE)
        .map(|chunk| GeneratedRule {
            matches: chunk.to_vec(),
        })
        .collect())
}

/// Prepend `base_path` to `path`, if set.
fn prepend_base_path(base_path: Option<&str>, path: &str) -> String {
    match base_path {
        Some(base) => format!("{base}{path}"),
        None => path.to_string(),
    }
}

/// Turn a parameter name list into deduped `ParamPresence`s, preserving
/// first-occurrence order.
fn dedupe_presence(names: &[String]) -> Vec<ParamPresence> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for name in names {
        if seen.insert(name.clone()) {
            out.push(ParamPresence { name: name.clone() });
        }
    }
    out
}

/// Merge `names` into `existing`, deduping by name and preserving the
/// order names are first seen in.
fn merge_presence(existing: &mut Vec<ParamPresence>, names: &[String]) {
    for name in names {
        if !existing.iter().any(|p| &p.name == name) {
            existing.push(ParamPresence { name: name.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn base_cfg() -> Config {
        Config {
            spec: PathBuf::from("openapi.yaml"),
            route: PathBuf::from("k8s/httproute.yaml"),
            name: None,
            match_methods: false,
            match_query: false,
            match_headers: false,
            path_match: PathMatch::Regex,
            base_path: None,
            backend: None,
            check: false,
        }
    }

    fn op(path: &str, method: &str) -> Operation {
        Operation {
            path: path.to_string(),
            method: method.to_string(),
            required_query: Vec::new(),
            required_header: Vec::new(),
        }
    }

    #[test]
    fn static_path_is_exact() {
        let rule = path_rule("/health", PathMatch::Regex).unwrap();

        assert_eq!(rule.kind, PathMatchKind::Exact);
        assert_eq!(rule.value, "/health");
    }

    #[test]
    fn templated_path_regex_escapes_literals() {
        let rule = path_rule("/v1.0/users/{id}", PathMatch::Regex).unwrap();

        assert_eq!(rule.kind, PathMatchKind::RegularExpression);
        assert_eq!(rule.value, r"^/v1\.0/users/[^/]+$");
    }

    #[test]
    fn multiple_params_regex() {
        let rule = path_rule("/a/{x}/b/{y}", PathMatch::Regex).unwrap();

        assert_eq!(rule.kind, PathMatchKind::RegularExpression);
        assert_eq!(rule.value, "^/a/[^/]+/b/[^/]+$");
    }

    #[test]
    fn prefix_strategy_truncates_at_param() {
        let rule = path_rule("/users/{id}", PathMatch::Prefix).unwrap();

        assert_eq!(rule.kind, PathMatchKind::PathPrefix);
        assert_eq!(rule.value, "/users/");
    }

    #[test]
    fn base_path_is_prepended() {
        let cfg = Config {
            base_path: Some("/api".to_string()),
            ..base_cfg()
        };
        let ops = vec![op("/health", "GET"), op("/users/{id}", "GET")];

        let rules = build_rules(&ops, &cfg).unwrap();

        assert_eq!(rules.len(), 1);
        let matches = &rules[0].matches;
        assert_eq!(matches.len(), 2);

        let health = matches
            .iter()
            .find(|m| m.path.value == "/api/health")
            .expect("health match present");
        assert_eq!(health.path.kind, PathMatchKind::Exact);

        let users = matches
            .iter()
            .find(|m| m.path.kind == PathMatchKind::RegularExpression)
            .expect("templated match present");
        assert_eq!(users.path.value, "^/api/users/[^/]+$");
    }

    #[test]
    fn methods_off_dedupes_path() {
        let cfg = base_cfg();
        let ops = vec![op("/users/{id}", "GET"), op("/users/{id}", "DELETE")];

        let rules = build_rules(&ops, &cfg).unwrap();

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].matches.len(), 1);
        assert_eq!(rules[0].matches[0].method, None);
    }

    #[test]
    fn methods_on_keeps_each_operation() {
        let cfg = Config {
            match_methods: true,
            ..base_cfg()
        };
        let ops = vec![op("/users/{id}", "GET"), op("/users/{id}", "DELETE")];

        let rules = build_rules(&ops, &cfg).unwrap();

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].matches.len(), 2);
        assert_eq!(rules[0].matches[0].method, Some("DELETE".to_string()));
        assert_eq!(rules[0].matches[1].method, Some("GET".to_string()));
    }

    #[test]
    fn required_query_and_header_emitted_when_enabled() {
        let cfg = Config {
            match_query: true,
            match_headers: true,
            ..base_cfg()
        };
        let ops = vec![Operation {
            path: "/items".to_string(),
            method: "GET".to_string(),
            required_query: vec!["q".to_string(), "q".to_string(), "page".to_string()],
            required_header: vec!["X-Key".to_string()],
        }];

        let rules = build_rules(&ops, &cfg).unwrap();

        let m = &rules[0].matches[0];
        assert_eq!(
            m.query,
            vec![
                ParamPresence {
                    name: "q".to_string()
                },
                ParamPresence {
                    name: "page".to_string()
                },
            ]
        );
        assert_eq!(
            m.headers,
            vec![ParamPresence {
                name: "X-Key".to_string()
            }]
        );
    }

    #[test]
    fn splits_rules_at_64_matches() {
        let cfg = Config {
            match_methods: true,
            ..base_cfg()
        };
        let ops: Vec<Operation> = (0..65).map(|i| op(&format!("/items/{i}"), "GET")).collect();

        let rules = build_rules(&ops, &cfg).unwrap();

        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].matches.len(), 64);
        assert_eq!(rules[1].matches.len(), 1);
    }

    #[test]
    fn unterminated_brace_path_is_error() {
        let result = path_rule("/files/{path", PathMatch::Regex);

        let err = result.expect_err("should error on unterminated brace");
        assert!(
            err.to_string().contains("/files/{path"),
            "error message should name the offending path: {err}"
        );
    }

    #[test]
    fn methods_off_dedup_distinguishes_kind() {
        let cfg = Config {
            path_match: PathMatch::Prefix,
            ..base_cfg()
        };
        let ops = vec![op("/users/", "GET"), op("/users/{id}", "DELETE")];

        let rules = build_rules(&ops, &cfg).unwrap();

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].matches.len(), 2);
        let kinds: BTreeSet<PathMatchKind> = rules[0].matches.iter().map(|m| m.path.kind).collect();
        assert!(kinds.contains(&PathMatchKind::Exact));
        assert!(kinds.contains(&PathMatchKind::PathPrefix));
    }

    #[test]
    fn output_order_is_deterministic() {
        let cfg = Config {
            match_methods: true,
            ..base_cfg()
        };
        let ops = vec![op("/b", "GET"), op("/a", "POST"), op("/a", "GET")];

        let rules = build_rules(&ops, &cfg).unwrap();

        let seen: Vec<(String, Option<String>)> = rules[0]
            .matches
            .iter()
            .map(|m| (m.path.value.clone(), m.method.clone()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("/a".to_string(), Some("GET".to_string())),
                ("/a".to_string(), Some("POST".to_string())),
                ("/b".to_string(), Some("GET".to_string())),
            ]
        );
    }
}
