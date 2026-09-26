//! Renders generated route rules to YAML text, and locates/splices the
//! `rules:` region of an existing HTTPRoute manifest.
//!
//! Pure text rendering only: no I/O. `render_rules_entry` turns
//! `translate::GeneratedRule`s into the exact `rules:` mapping entry text
//! that `splice` writes into an HTTPRoute manifest, at the byte range found
//! by `locate_rules`.

use std::ops::Range;

use anyhow::{Context, Result, anyhow, bail};
use saphyr::{LoadableYamlNode, Marker, MarkedYaml};

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

/// Where the `rules:` entry lives (or should be inserted) within an
/// HTTPRoute manifest's `spec:` mapping, plus any `backendRefs` value
/// carried over from the first existing rule.
pub struct RulesLocation {
    /// Byte range of the existing `rules:` entry (from the start of the
    /// `rules` key's line through the end of its value block), when a
    /// `rules:` key exists under `spec:`.
    pub replace: Option<Range<usize>>,
    /// Byte offset to insert a new `rules:` entry at the end of the
    /// `spec:` mapping, when there is no `rules:` key. Meaningless when
    /// `replace` is `Some`.
    pub insert_at: usize,
    /// Indent (in spaces) of `spec:`'s child keys, i.e. where the `rules`
    /// key sits (or should be placed).
    pub child_indent: usize,
    /// The first rule's `backendRefs` value, if any, in zero-indent form
    /// (see `render_rules_entry`'s doc comment for the exact contract).
    pub existing_backend_refs: Option<String>,
}

/// Locate the `rules:` entry (or the insertion point for one) within the
/// single HTTPRoute document in `manifest`.
///
/// `manifest` may contain multiple `---`-separated YAML documents; all
/// documents whose top-level `kind` is not `HTTPRoute` are ignored. Errors
/// if there is not exactly one `HTTPRoute` document, if it has no top-level
/// `spec:` mapping, or if `spec` is not a mapping.
pub fn locate_rules(manifest: &str) -> Result<RulesLocation> {
    let docs =
        MarkedYaml::load_from_str(manifest).map_err(|e| anyhow!("parsing manifest YAML: {e}"))?;

    let mut http_route: Option<&MarkedYaml> = None;
    for doc in &docs {
        let is_http_route = doc
            .data
            .as_mapping_get("kind")
            .and_then(|k| k.data.as_str())
            == Some("HTTPRoute");
        if is_http_route {
            if http_route.is_some() {
                bail!("manifest contains more than one HTTPRoute document");
            }
            http_route = Some(doc);
        }
    }
    let http_route =
        http_route.ok_or_else(|| anyhow!("manifest contains no HTTPRoute document"))?;

    let spec_mapping = http_route
        .data
        .as_mapping()
        .context("HTTPRoute document is not a mapping")?;
    let spec_entry = spec_mapping
        .iter()
        .find(|(k, _)| k.data.as_str() == Some("spec"))
        .ok_or_else(|| anyhow!("HTTPRoute document has no top-level 'spec' key"))?;
    let (spec_key, spec_value) = spec_entry;
    let spec_map = spec_value
        .data
        .as_mapping()
        .context("HTTPRoute 'spec' is not a mapping")?;

    let entries: Vec<(&MarkedYaml, &MarkedYaml)> = spec_map.iter().collect();
    let rules_entry = entries
        .iter()
        .find(|(k, _)| k.data.as_str() == Some("rules"));

    let child_indent = if let Some((key, _)) = rules_entry {
        key.span.start.col()
    } else if let Some((key, _)) = entries.first() {
        key.span.start.col()
    } else {
        // An empty `spec:` mapping: fall back to a conventional 2-space
        // nesting under `spec`'s own key.
        spec_key.span.start.col() + 2
    };

    let (replace, existing_backend_refs) = match rules_entry {
        Some((key, value)) => {
            let key_start = marker_byte(manifest, &key.span.start);
            let start = line_start_byte(manifest, key_start);
            let raw_end = marker_byte(manifest, &value.span.end);
            let end = snap_to_content_end(manifest, start, raw_end);
            let refs = capture_first_backend_refs(manifest, value);
            (Some(start..end), refs)
        }
        None => (None, None),
    };

    let spec_start = marker_byte(manifest, &spec_value.span.start);
    let raw_insert_at = marker_byte(manifest, &spec_value.span.end);
    let insert_at = snap_to_content_end(manifest, spec_start, raw_insert_at);

    Ok(RulesLocation {
        replace,
        insert_at,
        child_indent,
        existing_backend_refs,
    })
}

/// Replace the byte range in `location.replace` with `entry`, or insert
/// `entry` at `location.insert_at` if there is no existing `rules:` entry
/// to replace. All other bytes of `manifest` (including a trailing newline,
/// or its absence) are preserved exactly.
pub fn splice(manifest: &str, location: &RulesLocation, entry: &str) -> String {
    match &location.replace {
        Some(range) => {
            let mut out = String::with_capacity(manifest.len() + entry.len());
            out.push_str(&manifest[..range.start]);
            out.push_str(entry);
            out.push_str(&manifest[range.end..]);
            out
        }
        None => {
            let mut out = String::with_capacity(manifest.len() + entry.len() + 1);
            out.push_str(&manifest[..location.insert_at]);
            // The insertion point sits right after the last existing
            // sibling's value. If that value's text doesn't already end
            // in a newline (e.g. the manifest has no trailing newline and
            // `spec:` has no other children after `rules`), start a new
            // line ourselves so `entry`'s first key doesn't get glued
            // onto the previous line.
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(entry);
            out.push_str(&manifest[location.insert_at..]);
            out
        }
    }
}

/// Convert a `saphyr` char-index marker into a byte offset into `s`.
///
/// `saphyr`'s `Marker::index` counts chars, not bytes (despite its doc
/// comment), so this must go through `char_indices` rather than assume the
/// two coincide.
fn marker_byte(s: &str, marker: &Marker) -> usize {
    match s.char_indices().nth(marker.index()) {
        Some((byte, _)) => byte,
        None => s.len(),
    }
}

/// Byte offset of the start of the line containing `byte_idx`.
fn line_start_byte(s: &str, byte_idx: usize) -> usize {
    s[..byte_idx].rfind('\n').map_or(0, |i| i + 1)
}

/// Snap a `saphyr`-reported "end of value" byte offset back to the end of
/// the value's own last real line of content.
///
/// `saphyr`'s end-of-value markers land at the start of whatever real
/// token comes next in the document (the next sibling key, an enclosing
/// sequence's next item, or the document's end), silently skipping over
/// any trailing blank lines and comments in between. Left alone, that
/// would (a) fold trailing comments meant to stand on their own (or to
/// document the *next* key) into the region being replaced, discarding
/// them, and (b) when the next sibling is indented less than the current
/// one, `raw_end` overshoots partway into that sibling's own line, and
/// slicing there would eat its leading indentation.
///
/// This repeatedly inspects the last line within `[region_start, boundary)`
/// (a trailing `\n` terminates rather than starts a line, so it never
/// counts as its own empty line) and, while that line is blank or
/// comment-only, drops it and retries with the shrunk boundary. It stops
/// at the end of the nearest preceding line with real content, never
/// crossing back before `region_start`.
fn snap_to_content_end(manifest: &str, region_start: usize, raw_end: usize) -> usize {
    let region = &manifest[region_start..raw_end];
    let mut boundary = region.len();
    loop {
        let scan_end = if boundary > 0 && region.as_bytes()[boundary - 1] == b'\n' {
            boundary - 1
        } else {
            boundary
        };
        let line_start = region[..scan_end].rfind('\n').map_or(0, |i| i + 1);
        let content = region[line_start..scan_end].trim();
        if content.is_empty() || content.starts_with('#') {
            if line_start == 0 {
                boundary = 0;
                break;
            }
            boundary = line_start;
        } else {
            break;
        }
    }
    region_start + boundary
}

/// Capture the first rule's `backendRefs` value (if any) from a `rules:`
/// sequence node, in zero-indent form. Warns to stderr (and keeps the
/// first) if rules carry differing `backendRefs`.
fn capture_first_backend_refs(manifest: &str, rules_value: &MarkedYaml) -> Option<String> {
    let items = rules_value.data.as_sequence()?;

    let refs_per_rule: Vec<Option<String>> = items
        .iter()
        .map(|item| {
            item.data
                .as_mapping()
                .and_then(|m| m.iter().find(|(k, _)| k.data.as_str() == Some("backendRefs")))
                .map(|(_, v)| extract_zero_indent(manifest, v))
        })
        .collect();

    let first = refs_per_rule.first().cloned().flatten();
    if refs_per_rule.iter().any(|r| r != &first) {
        eprintln!(
            "warning: rules have differing backendRefs; keeping the first rule's backendRefs"
        );
    }
    first
}

/// Extract `value`'s raw source text and re-indent it to zero-indent form:
/// the first line flush at column 0, every other line dedented by the
/// value's own starting column. Blank lines are left bare. See
/// `render_rules_entry`'s doc comment for the exact contract this must
/// satisfy.
fn extract_zero_indent(manifest: &str, value: &MarkedYaml) -> String {
    let start = marker_byte(manifest, &value.span.start);
    let end = marker_byte(manifest, &value.span.end).max(start);
    // `value`'s end marker lands at the start of whatever comes next in
    // the document (the next sibling key, or an enclosing list's next
    // item), which may be partway through a line of pure indentation
    // belonging to that next token. Trimming trailing whitespace drops
    // that fragment along with any real trailing newline, matching the
    // no-trailing-newline zero-indent contract.
    let raw = manifest[start..end].trim_end();

    let strip_col = value.span.start.col();
    raw.split('\n')
        .enumerate()
        .map(|(i, line)| {
            if i == 0 || line.trim().is_empty() {
                if line.trim().is_empty() {
                    String::new()
                } else {
                    line.to_string()
                }
            } else {
                let strip = strip_col.min(line.len());
                if line.as_bytes()[..strip].iter().all(|&b| b == b' ') {
                    line[strip..].to_string()
                } else {
                    line.trim_start().to_string()
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod locate_and_splice_tests {
    use super::*;

    const HTTP_ROUTE_WITH_RULES: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
";

    const HTTP_ROUTE_NO_RULES: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
";

    #[test]
    fn locates_existing_rules_span() {
        let location = locate_rules(HTTP_ROUTE_WITH_RULES).expect("should locate rules");

        let range = location.replace.clone().expect("rules key exists");
        assert_eq!(location.child_indent, 2);
        assert_eq!(
            &HTTP_ROUTE_WITH_RULES[range],
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /health\n"
        );
    }

    #[test]
    fn captures_first_rule_backend_refs() {
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
      backendRefs:
        - name: svc
          port: 8080
    - matches:
        - path:
            type: Exact
            value: /other
";

        let location = locate_rules(manifest).expect("should locate rules");

        assert_eq!(
            location.existing_backend_refs,
            Some("- name: svc\n  port: 8080".to_string())
        );
    }

    #[test]
    fn differing_backend_refs_keeps_first_and_warns() {
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
      backendRefs:
        - name: svc-a
          port: 8080
    - matches:
        - path:
            type: Exact
            value: /other
      backendRefs:
        - name: svc-b
          port: 9090
";

        let location = locate_rules(manifest).expect("should locate rules");

        assert_eq!(
            location.existing_backend_refs,
            Some("- name: svc-a\n  port: 8080".to_string())
        );
    }

    #[test]
    fn no_backend_refs_is_none() {
        let location = locate_rules(HTTP_ROUTE_WITH_RULES).expect("should locate rules");

        assert_eq!(location.existing_backend_refs, None);
    }

    #[test]
    fn insert_point_when_no_rules() {
        let location = locate_rules(HTTP_ROUTE_NO_RULES).expect("should locate");

        assert!(location.replace.is_none());
        assert_eq!(location.child_indent, 2);

        let entry = "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /health\n";
        let spliced = splice(HTTP_ROUTE_NO_RULES, &location, entry);

        assert_eq!(
            spliced,
            "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
"
        );
    }

    #[test]
    fn errors_on_non_httproute() {
        let manifest = "\
apiVersion: v1
kind: Service
metadata:
  name: svc
";

        let result = locate_rules(manifest);

        assert!(result.is_err());
    }

    #[test]
    fn errors_on_multiple_httproutes() {
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: one
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /one
---
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: two
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /two
";

        let result = locate_rules(manifest);

        assert!(result.is_err());
    }

    #[test]
    fn ignores_other_kinds_in_multidoc() {
        let manifest = "\
apiVersion: v1
kind: Service
metadata:
  name: svc
---
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
";

        let location = locate_rules(manifest).expect("should target the HTTPRoute doc");

        let range = location.replace.expect("rules key exists");
        assert_eq!(
            &manifest[range],
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /health\n"
        );
    }

    #[test]
    fn splice_preserves_comments_around_rules() {
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
  # keep this list sorted by path
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
# trailing top-level comment
status: {}
";

        let location = locate_rules(manifest).expect("should locate rules");
        let entry = "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /new\n";

        let spliced = splice(manifest, &location, entry);

        assert_eq!(
            spliced,
            "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
    - name: gw
  # keep this list sorted by path
  rules:
    - matches:
        - path:
            type: Exact
            value: /new
# trailing top-level comment
status: {}
"
        );
    }

    #[test]
    fn splice_preserves_shallower_sibling_indentation_and_comment() {
        // Regression test: `rules` is followed, within `spec:`, by a
        // sibling key (`timeouts`) at the same indent, with a comment
        // line in between. The old rules value's end marker lands
        // partway into `timeouts`'s own indentation; splicing must not
        // eat that indentation or the comment.
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /health
  # comment between rules and next sibling
  timeouts:
    request: 5s
";

        let location = locate_rules(manifest).expect("should locate rules");
        let entry =
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /new\n";

        let spliced = splice(manifest, &location, entry);

        assert_eq!(
            spliced,
            "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /new
  # comment between rules and next sibling
  timeouts:
    request: 5s
"
        );
    }

    #[test]
    fn splice_preserves_trailing_newline_absence() {
        let manifest = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /health";
        assert!(!manifest.ends_with('\n'));

        let location = locate_rules(manifest).expect("should locate rules");
        let entry = "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: /new";

        let spliced = splice(manifest, &location, entry);

        assert!(!spliced.ends_with('\n'));
        assert_eq!(
            spliced,
            "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /new"
        );
    }
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
