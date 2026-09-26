//! Renders generated route rules to YAML text.
//!
//! Pure text rendering only: no I/O. `render_rules_entry` turns
//! `translate::GeneratedRule`s into the exact `rules:` mapping entry text
//! that a later task splices into an HTTPRoute manifest.

use crate::translate::{GeneratedRule, Match, ParamPresence, PathMatchKind};

/// Render the full `rules:` mapping entry (key plus value) for `rules`,
/// indented so the `rules` key itself sits at `child_indent` spaces.
///
/// Each `GeneratedRule` becomes one list item under `rules:`, containing a
/// `matches:` list (path, then optional method/queryParams/headers) and,
/// when `backend_refs` is `Some`, a `backendRefs:` key attached after it.
///
/// `backend_refs` must be the YAML block value that would render under a
/// `backendRefs:` key starting at column 0 — e.g.
/// `"- name: my-svc\n  port: 8080"` — with no leading indentation of its
/// own. Every line of it (blank lines left bare) is re-indented by
/// `child_indent + 6` spaces, the same indent used for each rule's
/// `matches:` list items, so it lines up as a sibling of `matches:` inside
/// the rule mapping. Later tasks that capture `backendRefs` text must
/// produce it in this zero-indent form.
///
/// All scalar `value:`s (path values, and the fixed `'.+'` for param
/// presence) are single-quoted, since regex path values can contain
/// characters (`.`, `^`, `[`, `\`) that are unsafe as YAML plain scalars.
/// `method:`, `type:`, and `name:` are emitted unquoted.
pub fn render_rules_entry(
    rules: &[GeneratedRule],
    backend_refs: Option<&str>,
    child_indent: usize,
) -> String {
    let rule_item_indent = child_indent + 2; // "- matches:" dash
    let rule_key_indent = child_indent + 4; // "backendRefs:" (sibling of "matches:")
    let match_item_indent = child_indent + 6; // "- path:" dash, and backendRefs content
    let match_key_indent = child_indent + 8; // method:/queryParams:/headers: (siblings of "path:")
    let path_attr_indent = child_indent + 10; // type:/value: under path
    let param_item_indent = child_indent + 10; // "- name: ..." dash
    let param_attr_indent = child_indent + 12; // type:/value: under a param item

    let mut out = String::new();
    out.push_str(&format!("{}rules:\n", indent(child_indent)));

    for rule in rules {
        out.push_str(&format!("{}- matches:\n", indent(rule_item_indent)));
        for m in &rule.matches {
            render_match(&mut out, m, match_item_indent, match_key_indent, path_attr_indent, param_item_indent, param_attr_indent);
        }
        if let Some(refs) = backend_refs {
            out.push_str(&format!("{}backendRefs:\n", indent(rule_key_indent)));
            render_backend_refs_block(&mut out, refs, match_item_indent);
        }
    }

    out
}

fn render_match(
    out: &mut String,
    m: &Match,
    match_item_indent: usize,
    match_key_indent: usize,
    path_attr_indent: usize,
    param_item_indent: usize,
    param_attr_indent: usize,
) {
    out.push_str(&format!("{}- path:\n", indent(match_item_indent)));
    out.push_str(&format!(
        "{}type: {}\n",
        indent(path_attr_indent),
        path_kind_str(m.path.kind)
    ));
    out.push_str(&format!(
        "{}value: {}\n",
        indent(path_attr_indent),
        quote(&m.path.value)
    ));

    if let Some(method) = &m.method {
        out.push_str(&format!("{}method: {}\n", indent(match_key_indent), method));
    }
    if !m.query.is_empty() {
        out.push_str(&format!("{}queryParams:\n", indent(match_key_indent)));
        for p in &m.query {
            render_param(out, p, param_item_indent, param_attr_indent);
        }
    }
    if !m.headers.is_empty() {
        out.push_str(&format!("{}headers:\n", indent(match_key_indent)));
        for p in &m.headers {
            render_param(out, p, param_item_indent, param_attr_indent);
        }
    }
}

fn render_param(out: &mut String, p: &ParamPresence, item_indent: usize, attr_indent: usize) {
    out.push_str(&format!("{}- name: {}\n", indent(item_indent), p.name));
    out.push_str(&format!("{}type: RegularExpression\n", indent(attr_indent)));
    out.push_str(&format!("{}value: '.+'\n", indent(attr_indent)));
}

/// Re-indent a zero-indent `backendRefs` value block by `block_indent`
/// spaces per line, leaving blank lines bare.
fn render_backend_refs_block(out: &mut String, refs: &str, block_indent: usize) {
    for line in refs.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&indent(block_indent));
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn path_kind_str(kind: PathMatchKind) -> &'static str {
    match kind {
        PathMatchKind::Exact => "Exact",
        PathMatchKind::RegularExpression => "RegularExpression",
        PathMatchKind::PathPrefix => "PathPrefix",
    }
}

/// Single-quote a YAML scalar, doubling any internal single quotes.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn indent(n: usize) -> String {
    " ".repeat(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::translate::PathRule;

    fn exact_match(path: &str) -> Match {
        Match {
            path: PathRule {
                kind: PathMatchKind::Exact,
                value: path.to_string(),
            },
            method: None,
            query: Vec::new(),
            headers: Vec::new(),
        }
    }

    #[test]
    fn renders_exact_path_only() {
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/health")],
        }];

        let out = render_rules_entry(&rules, None, 2);

        assert_eq!(
            out,
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: '/health'\n"
        );
    }

    #[test]
    fn renders_method_and_regex_path() {
        let rules = vec![GeneratedRule {
            matches: vec![Match {
                path: PathRule {
                    kind: PathMatchKind::RegularExpression,
                    value: r"^/v1\.0/users/[^/]+$".to_string(),
                },
                method: Some("GET".to_string()),
                query: Vec::new(),
                headers: Vec::new(),
            }],
        }];

        let out = render_rules_entry(&rules, None, 2);

        assert_eq!(
            out,
            "  rules:\n    - matches:\n        - path:\n            type: RegularExpression\n            value: '^/v1\\.0/users/[^/]+$'\n          method: GET\n"
        );
    }

    #[test]
    fn renders_query_and_header_presence() {
        let rules = vec![GeneratedRule {
            matches: vec![Match {
                path: PathRule {
                    kind: PathMatchKind::Exact,
                    value: "/items".to_string(),
                },
                method: None,
                query: vec![
                    ParamPresence {
                        name: "q".to_string(),
                    },
                    ParamPresence {
                        name: "page".to_string(),
                    },
                ],
                headers: vec![ParamPresence {
                    name: "X-Key".to_string(),
                }],
            }],
        }];

        let out = render_rules_entry(&rules, None, 2);

        assert_eq!(
            out,
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: '/items'\n          queryParams:\n            - name: q\n              type: RegularExpression\n              value: '.+'\n            - name: page\n              type: RegularExpression\n              value: '.+'\n          headers:\n            - name: X-Key\n              type: RegularExpression\n              value: '.+'\n"
        );
    }

    #[test]
    fn renders_backend_refs_when_present() {
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/health")],
        }];

        let out = render_rules_entry(&rules, Some("- name: my-svc\n  port: 8080"), 2);

        assert_eq!(
            out,
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: '/health'\n      backendRefs:\n        - name: my-svc\n          port: 8080\n"
        );
    }

    #[test]
    fn respects_child_indent() {
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/health")],
        }];

        let out2 = render_rules_entry(&rules, None, 2);
        let out4 = render_rules_entry(&rules, None, 4);

        assert_eq!(
            out2,
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: '/health'\n"
        );
        assert_eq!(
            out4,
            "    rules:\n      - matches:\n          - path:\n              type: Exact\n              value: '/health'\n"
        );
    }
}
