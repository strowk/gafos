//! Renders generated route rules to YAML text, and locates/splices the
//! owned region of an existing HTTPRoute manifest.
//!
//! Pure text rendering only: no I/O. `apply` (the entry point used by
//! `main`) picks one of two modes, both implemented in terms of
//! `locate_rules` and `splice`:
//!
//! - **Mode 1, in-place splice**: when the manifest has exactly one
//!   existing rule with a `matches:` key *and* the generated matches fit in
//!   a single rule, `render_matches_entry` renders a bare `matches:` entry
//!   and only that key's value is replaced, leaving `backendRefs:`,
//!   `timeouts:`, `filters:`, comments, and key order on the rule
//!   untouched.
//! - **Mode 2, full regeneration**: otherwise (no existing `rules:`, more
//!   than one existing rule, or the spec needs more than 64 matches and so
//!   splits into multiple generated rules). `render_rules_entry` renders
//!   the whole `rules:` mapping entry, which `splice` writes into the
//!   manifest at the byte range found by `locate_rules` (or inserts fresh,
//!   when `rules:` didn't exist).
//!
//! Both renderers indent their output by `seq_indent`, the block-sequence
//! dash offset `detect_seq_indent` reads from the manifest itself — there
//! is no fixed or configured indentation width.

use std::io::Write;
use std::ops::Range;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use saphyr::{LoadableYamlNode, MarkedYaml, Marker};

use crate::config::{Backend, Config};
use crate::translate::{GeneratedRule, Match, ParamPresence, PathMatchKind};

/// Render the full `rules:` mapping entry (key plus value) for `rules`,
/// indented so the `rules` key itself sits at `child_indent` spaces. Used
/// by `apply`'s full-regeneration mode; see `render_matches_entry` for the
/// in-place `matches:` splice used when there's exactly one existing rule.
///
/// Each `GeneratedRule` becomes one list item under `rules:`, containing a
/// `matches:` list (path, then optional method/queryParams/headers) and,
/// when `backend_refs` is `Some`, a `backendRefs:` key attached after it.
/// Per-match emission is delegated to `render_match_items`.
///
/// `seq_indent` is the gap, in spaces, between a block-sequence key's own
/// column and the `- ` dash of its first item (see `detect_seq_indent`).
/// Mapping nesting past a dash is always `+2` regardless of `seq_indent`;
/// only the dash offset itself varies. The rule dash sits at
/// `child_indent + seq_indent`, and `matches:`/`backendRefs:` (siblings
/// inside the rule mapping) at `child_indent + seq_indent + 2`.
///
/// `backend_refs` must be the YAML block value that would render under a
/// `backendRefs:` key starting at column 0 — e.g.
/// `"- name: my-svc\n  port: 8080"` — with no leading indentation of its
/// own. Every line of it (blank lines left bare) is re-indented by the same
/// indent used for each rule's `matches:` list items (its first dash), so
/// it lines up as a sibling of `matches:` inside the rule mapping. Later
/// tasks that capture `backendRefs` text must produce it in this
/// zero-indent form.
///
/// All scalar `value:`s (path values, and the fixed `'.+'` for param
/// presence) are single-quoted, since regex path values can contain
/// characters (`.`, `^`, `[`, `\`) that are unsafe as YAML plain scalars.
/// `method:`, `type:`, and `name:` are emitted unquoted.
pub fn render_rules_entry(
    rules: &[GeneratedRule],
    backend_refs: Option<&str>,
    child_indent: usize,
    seq_indent: usize,
) -> String {
    let rule_item_indent = child_indent + seq_indent; // "- matches:" dash
    let rule_key_indent = child_indent + seq_indent + 2; // "backendRefs:" (sibling of "matches:")
    let m_indent = rule_key_indent; // "matches:" key column, passed to render_match_items
    let match_item_indent = m_indent + seq_indent; // "- path:" dash, and backendRefs content

    let mut out = String::new();
    out.push_str(&format!("{}rules:\n", indent(child_indent)));

    for rule in rules {
        out.push_str(&format!("{}- matches:\n", indent(rule_item_indent)));
        render_match_items(&mut out, &rule.matches, m_indent, seq_indent);
        if let Some(refs) = backend_refs {
            out.push_str(&format!("{}backendRefs:\n", indent(rule_key_indent)));
            render_backend_refs_block(&mut out, refs, match_item_indent);
        }
    }

    out
}

/// Render a bare `matches:` mapping entry (key plus value) for `matches`,
/// with the `matches` key itself unindented (flush at column 0) — the form
/// a Mode-1 splice inserts directly at an existing `matches:` key's column.
///
/// `m_indent` is the column the `matches` key sits at (or would sit at) in
/// the surrounding manifest; it governs only the indentation of the
/// `matches:` value's contents (via `render_match_items`), not the leading
/// `"matches:\n"` line itself. See `render_match_items` for how `m_indent`
/// and `seq_indent` combine to produce each item's indent.
pub fn render_matches_entry(matches: &[Match], m_indent: usize, seq_indent: usize) -> String {
    let mut out = String::from("matches:\n");
    render_match_items(&mut out, matches, m_indent, seq_indent);
    out
}

/// Emit the `- path:` items of a matches list (no `matches:` key line of
/// its own) into `out`, shared by `render_matches_entry` and
/// `render_rules_entry` so both code paths render matches identically.
///
/// `m_indent` is the column of the `matches` key that owns this list.
/// `seq_indent` is the dash offset (see `detect_seq_indent`); mapping
/// nesting past a dash is always `+2`. Indents: match item dash
/// `m_indent + seq_indent`; match keys (`method:`/`queryParams:`/
/// `headers:`, siblings of `path:`) `m_indent + seq_indent + 2`;
/// `type:`/`value:` under `path:` `m_indent + seq_indent + 4`; param item
/// dash `m_indent + 2*seq_indent + 2`; param keys
/// `m_indent + 2*seq_indent + 4`.
fn render_match_items(out: &mut String, matches: &[Match], m_indent: usize, seq_indent: usize) {
    let match_item_indent = m_indent + seq_indent;
    let match_key_indent = m_indent + seq_indent + 2;
    let path_attr_indent = m_indent + seq_indent + 4;
    let param_item_indent = m_indent + 2 * seq_indent + 2;
    let param_attr_indent = m_indent + 2 * seq_indent + 4;

    for m in matches {
        render_match(
            out,
            m,
            match_item_indent,
            match_key_indent,
            path_attr_indent,
            param_item_indent,
            param_attr_indent,
        );
    }
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
    /// Number of items in the `spec.rules` sequence: `0` when `rules:` is
    /// absent, null, or not a sequence.
    pub existing_rule_count: usize,
    /// The sole rule's `matches:` entry span, when `existing_rule_count` is
    /// exactly `1` and that rule mapping has a `matches:` key. `None`
    /// otherwise, including when there is one rule but it has no `matches:`.
    pub sole_rule_matches: Option<MatchesSplice>,
}

/// Byte range of a single rule's existing `matches:` entry, plus the column
/// its `matches` key sits at, for an in-place `matches:` splice.
pub struct MatchesSplice {
    /// Byte range from the `matches` key's own byte (not the start of its
    /// line, so a preceding `- ` dash is preserved) through the end of its
    /// value block.
    pub range: Range<usize>,
    /// Column the `matches` key sits at.
    pub key_indent: usize,
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

    // `spec:` with nothing (or only comments) after it parses not as an
    // empty mapping but as an empty string scalar (saphyr has no distinct
    // "absent value" representation once a comment follows on its own
    // indented line). Treat that the same as an empty `spec:` mapping so a
    // freshly `scaffold`ed manifest — whose `spec:` has only a commented
    // placeholder line — can still have `rules:` inserted into it.
    let (entries, null_insert_at): (Vec<(&MarkedYaml, &MarkedYaml)>, Option<usize>) =
        match spec_value.data.as_mapping() {
            Some(spec_map) => (spec_map.iter().collect(), None),
            None if spec_value.data.as_str() == Some("") => {
                let key_col = spec_key.span.start.col();
                let key_end = marker_byte(manifest, &spec_key.span.end);
                let next_line_start = manifest[key_end..]
                    .find('\n')
                    .map(|i| key_end + i + 1)
                    .unwrap_or(manifest.len());
                (
                    Vec::new(),
                    Some(scan_block_end(manifest, key_col, next_line_start)),
                )
            }
            None => bail!("HTTPRoute 'spec' is not a mapping"),
        };
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

    let insert_at = match null_insert_at {
        Some(v) => v,
        None => {
            let spec_start = marker_byte(manifest, &spec_value.span.start);
            let raw_insert_at = marker_byte(manifest, &spec_value.span.end);
            snap_to_content_end(manifest, spec_start, raw_insert_at)
        }
    };

    let (existing_rule_count, sole_rule_matches) =
        rule_count_and_sole_matches(manifest, rules_entry);

    Ok(RulesLocation {
        replace,
        insert_at,
        child_indent,
        existing_backend_refs,
        existing_rule_count,
        sole_rule_matches,
    })
}

/// Compute `RulesLocation::existing_rule_count` and
/// `RulesLocation::sole_rule_matches` from the `rules:` entry (if any) found
/// under `spec:`.
fn rule_count_and_sole_matches(
    manifest: &str,
    rules_entry: Option<&(&MarkedYaml, &MarkedYaml)>,
) -> (usize, Option<MatchesSplice>) {
    let Some((_, rules_value)) = rules_entry else {
        return (0, None);
    };
    let Some(items) = rules_value.data.as_sequence() else {
        return (0, None);
    };

    let count = items.len();
    if count != 1 {
        return (count, None);
    }

    let sole_rule_matches = items[0].data.as_mapping().and_then(|rule_map| {
        rule_map
            .iter()
            .find(|(k, _)| k.data.as_str() == Some("matches"))
            .map(|(matches_key, matches_value)| {
                let start = marker_byte(manifest, &matches_key.span.start);
                let raw_end = marker_byte(manifest, &matches_value.span.end);
                let end = snap_to_content_end(manifest, start, raw_end);
                MatchesSplice {
                    range: start..end,
                    key_indent: matches_key.span.start.col(),
                }
            })
    });

    (count, sole_rule_matches)
}

/// Replace the byte range `range` within `manifest` with `entry`. All other
/// bytes of `manifest` are preserved exactly.
fn replace_range(manifest: &str, range: &Range<usize>, entry: &str) -> String {
    let mut out = String::with_capacity(manifest.len() + entry.len());
    out.push_str(&manifest[..range.start]);
    out.push_str(entry);
    out.push_str(&manifest[range.end..]);
    out
}

/// Replace the byte range in `location.replace` with `entry`, or insert
/// `entry` at `location.insert_at` if there is no existing `rules:` entry
/// to replace. All other bytes of `manifest` (including a trailing newline,
/// or its absence) are preserved exactly.
pub fn splice(manifest: &str, location: &RulesLocation, entry: &str) -> String {
    match &location.replace {
        Some(range) => replace_range(manifest, range, entry),
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

/// Produce a minimal HTTPRoute manifest document for `name`.
///
/// Enough structure for `locate_rules` to find an insertion point under
/// `spec:` (at `child_indent` 2) for a `rules:` entry: `spec:`'s only
/// content is a commented `parentRefs:` placeholder, reminding the user to
/// attach the route to their Gateway themselves.
pub fn scaffold(name: &str) -> String {
    format!(
        "apiVersion: gateway.networking.k8s.io/v1\n\
         kind: HTTPRoute\n\
         metadata:\n\
         \u{20}\u{20}name: {name}\n\
         spec:\n\
         \u{20}\u{20}# parentRefs:   (commented placeholder line — the user attaches this to their Gateway)\n"
    )
}

/// Choose which `backendRefs` value new/updated rules should carry.
///
/// `existing` (the first rule's current `backendRefs`, if any, as captured
/// by `locate_rules`) always wins, and is passed through unchanged — it is
/// already in the zero-indent form `render_rules_entry` expects. Otherwise,
/// `fallback` (typically `cfg.backend`) is rendered as a zero-indent
/// `backendRefs` list value (dash at column 0). With neither, `None`.
pub fn resolve_backend_refs(existing: Option<&str>, fallback: Option<&Backend>) -> Option<String> {
    if let Some(existing) = existing {
        return Some(existing.to_string());
    }
    fallback.map(|backend| format!("- name: {}\n  port: {}", backend.name, backend.port))
}

/// Render `rules` into `manifest` (scaffolding a fresh HTTPRoute document
/// first if `manifest` is `None`) and return the resulting manifest text.
///
/// Two modes, chosen by how `rules` and the existing manifest line up:
///
/// **Mode 1** (in-place `matches:` splice): when there is exactly one
/// generated rule, exactly one existing rule, and that existing rule has a
/// `matches:` key. Only the existing rule's `matches:` value is replaced
/// (via `replace_range`); everything else about that rule — `backendRefs`,
/// hand-added fields like `timeouts` or `filters`, comments, key order — is
/// left untouched.
///
/// **Mode 2** (regenerate): otherwise. The whole `rules:` entry is
/// re-rendered from `rules` via `render_rules_entry` and spliced in,
/// carrying over the first existing rule's `backendRefs` (or falling back
/// to `cfg.backend`) the same way it always has. If the manifest already
/// had one or more rules, this discards any hand-added per-rule fields, so
/// a warning is printed to stderr first.
///
/// Both modes render at `seq_indent`, the block-sequence dash offset
/// `detect_seq_indent` reads from `manifest_text` itself, so generated
/// output matches the file's own indentation style rather than a fixed
/// one. In both modes, `manifest` and all other content around `spec:`
/// (comments, sibling keys, trailing-newline presence) is preserved
/// exactly.
pub fn apply(manifest: Option<&str>, rules: &[GeneratedRule], cfg: &Config) -> Result<String> {
    let owned_scaffold;
    let manifest_text: &str = match manifest {
        Some(text) => text,
        None => {
            let name = cfg.name.as_deref().unwrap_or("httproute");
            owned_scaffold = scaffold(name);
            &owned_scaffold
        }
    };

    let location = locate_rules(manifest_text)?;
    let seq_indent = detect_seq_indent(manifest_text);

    if rules.len() == 1
        && location.existing_rule_count == 1
        && let Some(ms) = &location.sole_rule_matches
    {
        let entry = render_matches_entry(&rules[0].matches, ms.key_indent, seq_indent);
        return Ok(replace_range(manifest_text, &ms.range, &entry));
    }

    if location.existing_rule_count >= 1 {
        eprintln!(
            "warning: route spans multiple rules; hand-added per-rule fields (timeouts, filters) are not preserved"
        );
    }

    let backend_refs = resolve_backend_refs(
        location.existing_backend_refs.as_deref(),
        cfg.backend.as_ref(),
    );
    let entry = render_rules_entry(
        rules,
        backend_refs.as_deref(),
        location.child_indent,
        seq_indent,
    );
    Ok(splice(manifest_text, &location, &entry))
}

/// Write `contents` to `path`, replacing it atomically: a temp file is
/// written in `path`'s parent directory and then renamed over `path`, so a
/// crash mid-write never leaves a half-written manifest on disk.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temp file in {}", parent.display()))?;
    tmp.write_all(contents.as_bytes())
        .with_context(|| format!("writing temp file for {}", path.display()))?;
    tmp.persist(path)
        .with_context(|| format!("persisting temp file to {}", path.display()))?;
    Ok(())
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

/// Scan forward from `from_byte` (the start of a line) through lines that
/// are blank or indented more than `key_col`, treating them as still
/// belonging to the block that started at that column (this is how an
/// empty `key:` value's commented-out placeholder content is kept intact
/// rather than treated as a sibling). Returns the byte offset of the first
/// line that dedents to `key_col` or less, or `s.len()` if none is found.
fn scan_block_end(s: &str, key_col: usize, from_byte: usize) -> usize {
    let mut pos = from_byte;
    loop {
        if pos >= s.len() {
            return s.len();
        }
        let line_end = s[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(s.len());
        let line = s[pos..line_end].trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            pos = line_end;
            continue;
        }
        let indent = line.len() - trimmed.len();
        if indent > key_col {
            pos = line_end;
        } else {
            return pos;
        }
    }
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

/// The gap, in spaces, between `key`'s column and the `- ` dash of
/// `value`'s first item — e.g. `2` when items dash two spaces past their
/// key, `0` when the dash lines up with the key itself. `None` when `value`
/// isn't a non-empty block sequence, or the computed offset would be
/// negative (a dash shallower than its key, which callers should treat as
/// not usable).
fn seq_offset(key: &MarkedYaml, value: &MarkedYaml) -> Option<usize> {
    let items = value.data.as_sequence()?;
    let first_item = items.first()?;
    (first_item.span.start.col() - 2).checked_sub(key.span.start.col())
}

/// Detect how many spaces past its own key a manifest's existing block
/// sequences dash their items, so later renders can match `manifest`'s
/// style instead of always using `gafos`'s own default.
///
/// Tries, in order: the HTTPRoute's `spec.rules` sequence itself; failing
/// that, the first rule's `matches:` sequence; failing that, any other
/// `spec` child whose value is a non-empty block sequence (e.g.
/// `parentRefs:`). Returns `2` when none of those qualify, or when
/// `manifest` doesn't parse as a single HTTPRoute document with a `spec`
/// mapping.
pub fn detect_seq_indent(manifest: &str) -> usize {
    const DEFAULT: usize = 2;

    let Ok(docs) = MarkedYaml::load_from_str(manifest) else {
        return DEFAULT;
    };

    let mut http_route: Option<&MarkedYaml> = None;
    for doc in &docs {
        let is_http_route = doc
            .data
            .as_mapping_get("kind")
            .and_then(|k| k.data.as_str())
            == Some("HTTPRoute");
        if is_http_route {
            if http_route.is_some() {
                return DEFAULT;
            }
            http_route = Some(doc);
        }
    }
    let Some(http_route) = http_route else {
        return DEFAULT;
    };

    let Some(doc_mapping) = http_route.data.as_mapping() else {
        return DEFAULT;
    };
    let Some((_, spec_value)) = doc_mapping
        .iter()
        .find(|(k, _)| k.data.as_str() == Some("spec"))
    else {
        return DEFAULT;
    };
    let Some(entries) = spec_value.data.as_mapping() else {
        return DEFAULT;
    };

    let rules_entry = entries
        .iter()
        .find(|(k, _)| k.data.as_str() == Some("rules"));

    if let Some((rules_key, rules_value)) = rules_entry {
        if let Some(offset) = seq_offset(rules_key, rules_value) {
            return offset;
        }
        let first_rule_matches = rules_value
            .data
            .as_sequence()
            .and_then(|items| items.first())
            .and_then(|rule| rule.data.as_mapping())
            .and_then(|rule_map| {
                rule_map
                    .iter()
                    .find(|(k, _)| k.data.as_str() == Some("matches"))
            });
        if let Some((matches_key, matches_value)) = first_rule_matches
            && let Some(offset) = seq_offset(matches_key, matches_value)
        {
            return offset;
        }
    }

    for (key, value) in entries.iter() {
        if key.data.as_str() == Some("rules") {
            continue;
        }
        if let Some(offset) = seq_offset(key, value) {
            return offset;
        }
    }

    DEFAULT
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
                .and_then(|m| {
                    m.iter()
                        .find(|(k, _)| k.data.as_str() == Some("backendRefs"))
                })
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

    // Same `rules:` shape as `HTTP_ROUTE_WITH_RULES`, whose items dash two
    // columns past `rules:` (`rules:` at col 2, `- matches:` dash at col 4).
    const INDENTED_MANIFEST: &str = HTTP_ROUTE_WITH_RULES;

    // `rules:` items dashed in the same column as `rules:` itself.
    const DASH_ALIGNED_MANIFEST: &str = "\
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

    // Same shape as `HTTP_ROUTE_WITH_RULES`'s single-doc `rules:` list, but
    // with two rule items (reusing the per-doc shape from
    // `errors_on_multiple_httproutes`, folded into one HTTPRoute).
    const TWO_RULE_MANIFEST: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /one
    - matches:
        - path:
            type: Exact
            value: /two
";

    // No `rules:` at all; `parentRefs:` items dashed in the same column as
    // `parentRefs:` itself.
    const NO_RULES_DASH_ALIGNED: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  parentRefs:
  - name: gw
";

    // `spec` has only scalar/mapping children, no block sequences anywhere.
    const NO_SEQUENCES: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  timeouts:
    request: 5s
";

    #[test]
    fn seq_indent_detects_dash_aligned() {
        // rules items dashed in the same column as `rules:` -> 0
        assert_eq!(detect_seq_indent(DASH_ALIGNED_MANIFEST), 0);
    }
    #[test]
    fn seq_indent_detects_indented() {
        // rules items dashed two past `rules:` -> 2
        assert_eq!(detect_seq_indent(INDENTED_MANIFEST), 2);
    }
    #[test]
    fn seq_indent_falls_back_to_parent_refs_when_no_rules() {
        // no rules:, parentRefs items dash-aligned -> 0
        assert_eq!(detect_seq_indent(NO_RULES_DASH_ALIGNED), 0);
    }
    #[test]
    fn seq_indent_defaults_to_two_without_sequences() {
        // spec has only scalar/mapping children -> 2
        assert_eq!(detect_seq_indent(NO_SEQUENCES), 2);
    }

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
    fn locate_reports_single_rule_matches_span() {
        let loc = locate_rules(HTTP_ROUTE_WITH_RULES).unwrap();
        assert_eq!(loc.existing_rule_count, 1);
        let ms = loc.sole_rule_matches.expect("single rule with matches");
        assert_eq!(ms.key_indent, 6);
        assert_eq!(
            &HTTP_ROUTE_WITH_RULES[ms.range],
            "matches:\n        - path:\n            type: Exact\n            value: /health\n"
        );
    }

    #[test]
    fn locate_reports_multiple_rules_no_splice() {
        let loc = locate_rules(TWO_RULE_MANIFEST).unwrap();
        assert_eq!(loc.existing_rule_count, 2);
        assert!(loc.sole_rule_matches.is_none());
    }

    #[test]
    fn locate_reports_zero_rules() {
        let loc = locate_rules(HTTP_ROUTE_NO_RULES).unwrap();
        assert_eq!(loc.existing_rule_count, 0);
        assert!(loc.sole_rule_matches.is_none());
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

        let out = render_rules_entry(&rules, None, 2, 2);

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

        let out = render_rules_entry(&rules, None, 2, 2);

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

        let out = render_rules_entry(&rules, None, 2, 2);

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

        let out = render_rules_entry(&rules, Some("- name: my-svc\n  port: 8080"), 2, 2);

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

        let out2 = render_rules_entry(&rules, None, 2, 2);
        let out4 = render_rules_entry(&rules, None, 4, 2);

        assert_eq!(
            out2,
            "  rules:\n    - matches:\n        - path:\n            type: Exact\n            value: '/health'\n"
        );
        assert_eq!(
            out4,
            "    rules:\n      - matches:\n          - path:\n              type: Exact\n              value: '/health'\n"
        );
    }

    #[test]
    fn renders_matches_entry_bare_for_splice() {
        let rules = [GeneratedRule {
            matches: vec![exact_match("/health")],
        }];
        let out = render_matches_entry(&rules[0].matches, 6, 2);
        assert_eq!(
            out,
            "matches:\n        - path:\n            type: Exact\n            value: '/health'\n"
        );
    }

    #[test]
    fn renders_rules_entry_dash_aligned_when_seq_indent_zero() {
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/health")],
        }];
        let out = render_rules_entry(&rules, None, 2, 0);
        assert_eq!(
            out,
            "  rules:\n  - matches:\n    - path:\n        type: Exact\n        value: '/health'\n"
        );
    }
}

#[cfg(test)]
mod apply_tests {
    use super::*;
    use crate::config::PathMatch;
    use crate::translate::PathRule;
    use std::path::PathBuf;

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

    fn test_cfg(name: Option<&str>, backend: Option<Backend>) -> Config {
        Config {
            spec: PathBuf::from("openapi.yaml"),
            route: PathBuf::from("httproute.yaml"),
            name: name.map(str::to_string),
            match_methods: false,
            match_query: false,
            match_headers: false,
            path_match: PathMatch::Regex,
            base_path: None,
            backend,
            check: false,
        }
    }

    fn one_rule(path: &str) -> GeneratedRule {
        GeneratedRule {
            matches: vec![exact_match(path)],
        }
    }

    // Single rule: matches:, then backendRefs:, then timeouts: (all +2
    // style, matching HTTP_ROUTE_WITH_RULES's layout). `timeouts:`'s
    // "child" is deliberately dashed at the same column as `timeouts:`
    // itself (col 6) rather than +2 (col 8) — Mode 1 never re-parses or
    // re-indents it, so this only needs to be valid YAML and preserved
    // byte-for-byte, not semantically nested.
    const MANIFEST_MATCHES_BACKEND_TIMEOUTS: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /old
      backendRefs:
        - name: my-svc
          port: 8080
      timeouts:
      request: 60s
";

    // Single rule, dash-aligned (S=0): `rules:` items dash at the same
    // column as `rules:` itself, and `matches:` items dash at the same
    // column as `matches:` itself.
    const DASH_ALIGNED_ONE_RULE: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
  - matches:
    - path:
        type: Exact
        value: /old
";

    // Single rule with `backendRefs:` before `matches:` in the rule
    // mapping; Mode 1 must find `matches:` regardless of key order and
    // leave `backendRefs:` (and the key order) untouched.
    const BACKEND_BEFORE_MATCHES: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - backendRefs:
        - name: kept-svc
          port: 8080
      matches:
        - path:
            type: Exact
            value: /old
";

    // Single rule whose `matches:` is its only (and thus last) key; a
    // `timeouts:` sibling of `rules:` follows at the `spec:` level, and the
    // manifest as a whole has no trailing newline. `matches:`'s value span
    // must skip past the end of the rule mapping and the `rules:` sequence
    // to stop right before `timeouts:`, leaving the untouched, newline-less
    // tail exactly as-is.
    const MANIFEST_NO_TRAILING_NL: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /old
  timeouts:
    request: 5s";

    // A comment sits between the rule's `matches:` value and its
    // `backendRefs:` sibling; Mode 1 must not swallow it into the
    // replaced `matches:` range.
    const MANIFEST_COMMENT_IN_RULE: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /old
      # keep me
      backendRefs:
        - name: svc
          port: 8080
";

    // Two existing rules: Mode 1's single-rule preconditions don't hold,
    // so `apply` must fall back to Mode 2 (full regeneration).
    const TWO_RULE_MANIFEST: &str = "\
apiVersion: gateway.networking.k8s.io/v1
kind: HTTPRoute
metadata:
  name: demo
spec:
  rules:
    - matches:
        - path:
            type: Exact
            value: /one
    - matches:
        - path:
            type: Exact
            value: /two
";

    #[test]
    fn apply_mode1_rewrites_matches_preserving_timeouts() {
        // single rule: matches:, then backendRefs:, then timeouts: (all +2 style)
        let out = apply(
            Some(MANIFEST_MATCHES_BACKEND_TIMEOUTS),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(out.contains("value: '/new'"));
        assert!(out.contains("timeouts:\n      request: 60s")); // preserved byte-for-byte
        assert!(out.contains("backendRefs:\n        - name:")); // preserved
        assert!(!out.contains("/old")); // matches replaced
    }

    #[test]
    fn apply_mode1_preserves_zero_seq_indent() {
        // dash-aligned single-rule manifest; output keeps S=0 and rewrites matches
        let out = apply(
            Some(DASH_ALIGNED_ONE_RULE),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(out.contains("  - matches:\n    - path:"));
    }

    #[test]
    fn apply_mode1_matches_not_first_key() {
        // rule with backendRefs: before matches:; matches rewritten, backendRefs untouched, order kept
        let out = apply(
            Some(BACKEND_BEFORE_MATCHES),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(out.contains("value: '/new'"));
        assert!(out.contains("- name: kept-svc"));
    }

    #[test]
    fn apply_mode1_matches_last_key_no_trailing_newline() {
        assert!(!MANIFEST_NO_TRAILING_NL.ends_with('\n'));
        let out = apply(
            Some(MANIFEST_NO_TRAILING_NL),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(!out.ends_with('\n'));
        assert!(out.contains("value: '/new'"));
    }

    #[test]
    fn apply_mode1_preserves_comment_between_matches_and_sibling() {
        let out = apply(
            Some(MANIFEST_COMMENT_IN_RULE),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(out.contains("# keep me"));
    }

    #[test]
    fn apply_mode2_regenerates_when_multiple_rules() {
        // two existing rules -> full regeneration, single generated rule replaces the block
        let out = apply(
            Some(TWO_RULE_MANIFEST),
            &[one_rule("/new")],
            &test_cfg(None, None),
        )
        .unwrap();
        assert!(out.contains("value: '/new'"));
    }

    #[test]
    fn scaffold_has_kind_and_name() {
        let manifest = scaffold("myapp");

        assert!(manifest.contains("kind: HTTPRoute"));
        assert!(manifest.contains("name: myapp"));

        // Must also be a valid HTTPRoute skeleton that `locate_rules` can
        // insert a `rules:` entry into.
        let location = locate_rules(&manifest).expect("scaffold should be a locatable HTTPRoute");
        assert!(location.replace.is_none());
        assert_eq!(location.child_indent, 2);
    }

    #[test]
    fn apply_scaffolds_when_missing_then_inserts_rules() {
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/health")],
        }];
        let cfg = test_cfg(Some("myapp"), None);

        let result = apply(None, &rules, &cfg).expect("should scaffold and apply");

        assert_eq!(
            result,
            "apiVersion: gateway.networking.k8s.io/v1\n\
             kind: HTTPRoute\n\
             metadata:\n\
             \u{20}\u{20}name: myapp\n\
             spec:\n\
             \u{20}\u{20}# parentRefs:   (commented placeholder line — the user attaches this to their Gateway)\n\
             \u{20}\u{20}rules:\n\
             \u{20}\u{20}\u{20}\u{20}- matches:\n\
             \u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}- path:\n\
             \u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}type: Exact\n\
             \u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}\u{20}value: '/health'\n"
        );

        // The result must itself be re-locatable (valid, single HTTPRoute).
        let location = locate_rules(&result).expect("result should remain a valid HTTPRoute");
        assert!(location.replace.is_some());
    }

    #[test]
    fn resolve_prefers_existing_backend() {
        let existing = "- name: existing-svc\n  port: 1234";
        let fallback = Backend {
            name: "other-svc".to_string(),
            port: 9999,
        };

        let resolved = resolve_backend_refs(Some(existing), Some(&fallback));

        assert_eq!(resolved, Some(existing.to_string()));
    }

    #[test]
    fn resolve_falls_back_to_config_backend() {
        let fallback = Backend {
            name: "my-svc".to_string(),
            port: 8080,
        };

        let resolved = resolve_backend_refs(None, Some(&fallback));

        assert_eq!(resolved, Some("- name: my-svc\n  port: 8080".to_string()));
    }

    #[test]
    fn resolve_none_when_neither() {
        let resolved = resolve_backend_refs(None, None);

        assert_eq!(resolved, None);
    }

    #[test]
    fn apply_preserves_existing_backend_refs_end_to_end() {
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
";
        let rules = vec![GeneratedRule {
            matches: vec![exact_match("/new")],
        }];
        let cfg = test_cfg(
            None,
            Some(Backend {
                name: "other-svc".to_string(),
                port: 9999,
            }),
        );

        let result = apply(Some(manifest), &rules, &cfg).expect("should apply");

        assert_eq!(
            result,
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
            value: '/new'
      backendRefs:
        - name: svc
          port: 8080
"
        );
    }

    #[test]
    fn write_atomic_replaces_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("httproute.yaml");
        std::fs::write(&path, "old contents").expect("write initial file");

        write_atomic(&path, "new contents").expect("should write atomically");

        let contents = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(contents, "new contents");
    }
}
