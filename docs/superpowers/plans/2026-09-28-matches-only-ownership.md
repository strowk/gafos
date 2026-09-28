# Matches-only Ownership and Indentation Preservation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** gafos rewrites only the `matches:` list of the managed HTTPRoute rule — preserving `backendRefs:`/`timeouts:`/`filters:`/comments byte-for-byte — and writes matches in the file's own block-sequence indentation style.

**Architecture:** All changes are in `src/route.rs`. Detection of the sequence-indent width and mode selection are added; the renderer is parameterized by that width and split so a bare `matches:` block and the full `rules:` block share one item renderer. `apply` gains two modes: an in-place `matches:` splice for the common single-rule case, and today's full-block regeneration (now indent-aware, with a warning) for create and multi-rule fall-backs.

**Tech Stack:** Rust, `saphyr` (marked YAML parsing), `anyhow`. Tests are Rust `#[test]` units in `route.rs` plus fixture and CLI integration tests.

**Spec:** `docs/superpowers/specs/2026-09-28-matches-only-ownership-design.md`

## Global Constraints

- All production changes live in `src/route.rs`; `translate.rs`, `openapi.rs`, `config.rs`, `cli.rs`, `main.rs` are not modified. (`README.md`, fixtures, and test files are edited only in their own tasks.)
- Mapping nesting is a fixed `+2`; only the block-sequence dash offset varies with the detected `seq_indent`.
- With `seq_indent = 2` the renderer must reproduce today's exact byte output (existing golden strings and fixtures are the regression oracle).
- Existing `tests/fixtures/*/expected.yaml` files must stay byte-for-byte identical — no expected-file edits. A fixture that drifts signals a splice-boundary bug, not a fixture to update.
- Scalars keep today's quoting: path/`value:` single-quoted (internal `'` doubled), `type:`/`method:`/`name:` unquoted.
- `saphyr` `Marker` columns are 0-based; a block sequence item's `span.start.col()` points at the item's content, with the `- ` dash two columns before it. Verify this on the first task and adjust the single `-2` in `detect_seq_indent` (and the Mode-1 span start) centrally if it does not hold.
- Warning text, verbatim: `warning: route spans multiple rules; hand-added per-rule fields (timeouts, filters) are not preserved`, printed to stderr; exit code unchanged.

## Review Focus

- **`matches:` is not the first key of the rule** (e.g. `backendRefs:` precedes it): the Mode-1 span must start at the `matches` key column so the leading indentation is preserved, not the `- ` of a different key. → Task 4 test.
- **`matches:` is the last key of the rule** (no sibling after it, at end of document): the snap must land at the end of the last match line, not overshoot the document end or eat a trailing newline. → Task 4 test.
- **A comment sits between `matches:` and the next rule key**: the comment must survive the splice (it belongs to the preserved remainder). → Task 4 test.
- **The manifest has no trailing newline**: Mode-1 splice must not add or drop one. → Task 4 test.
- **`rules:` absent but another sequence (`parentRefs:`) present**: `detect_seq_indent` must fall back to that sequence's style, not the default. → Task 1 test.

---

### Task 1: Detect the block-sequence indentation width

**Files:**
- Modify: `src/route.rs`
- Test: `src/route.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces: `pub fn detect_seq_indent(manifest: &str) -> usize` — parses `manifest`, finds the HTTPRoute document, and returns the dash offset `(first_item_col - 2) - seq_key_col` of a block sequence, tried in priority order: `spec.rules` then the first rule's `matches:`, then any other `spec` child whose value is a non-empty block sequence (e.g. `parentRefs:`); returns `2` when none qualifies. Offsets computing `< 0` are skipped. Non-HTTPRoute / unparseable input returns `2`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn seq_indent_detects_dash_aligned() {
    // rules items dashed in the same column as `rules:` -> 0
    assert_eq!(detect_seq_indent(DASH_ALIGNED_MANIFEST), 0);
}
#[test] fn seq_indent_detects_indented() {
    // rules items dashed two past `rules:` -> 2
    assert_eq!(detect_seq_indent(INDENTED_MANIFEST), 2);
}
#[test] fn seq_indent_falls_back_to_parent_refs_when_no_rules() {
    // no rules:, parentRefs items dash-aligned -> 0
    assert_eq!(detect_seq_indent(NO_RULES_DASH_ALIGNED), 0);
}
#[test] fn seq_indent_defaults_to_two_without_sequences() {
    // spec has only scalar/mapping children -> 2
    assert_eq!(detect_seq_indent(NO_SEQUENCES), 2);
}
```

Use dash-aligned and indented literals modeled on the existing `HTTP_ROUTE_WITH_RULES` constant (dash-aligned = `rules:` at col 2 with `- matches:` also at col 2; indented = the existing `+2` layout).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib route::` — Expected: FAIL (`detect_seq_indent` not found).

- [ ] **Step 3: Implement `detect_seq_indent`**

Add a private `seq_offset(key: &MarkedYaml, value: &MarkedYaml) -> Option<usize>` returning `Some((first_item.span.start.col() - 2).checked_sub(key.span.start.col())?)` only when `value` is a sequence with ≥1 item; `detect_seq_indent` walks the located HTTPRoute doc in the priority order above and returns the first `Some`, else `2`. Reuse the same document-finding logic shape as `locate_rules` (find the `HTTPRoute` doc, get `spec` mapping); a private shared helper is fine but not required.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib route::` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/route.rs
git commit -m "feat(route): detect block-sequence indentation width"
```

---

### Task 2: Parameterize the renderer by sequence indent

**Files:**
- Modify: `src/route.rs`
- Test: `src/route.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces:
  - `fn render_match_items(out: &mut String, matches: &[Match], m_indent: usize, seq_indent: usize)` — emits the `- path:` items of a matches list (no `matches:` key line). `m_indent` is the `matches` key column. Indents: match item dash `m_indent + seq_indent`; match keys `m_indent + seq_indent + 2`; `type:`/`value:` under `path:` `m_indent + seq_indent + 4`; param item dash `m_indent + 2*seq_indent + 2`; param keys `m_indent + 2*seq_indent + 4`.
  - `pub fn render_matches_entry(matches: &[Match], m_indent: usize, seq_indent: usize) -> String` — returns `"matches:\n"` (bare, no leading indent) followed by `render_match_items`.
  - `pub fn render_rules_entry(rules: &[GeneratedRule], backend_refs: Option<&str>, child_indent: usize, seq_indent: usize) -> String` — same output contract as today, now indent-aware: rule dash at `child_indent + seq_indent`, `matches`/`backendRefs` keys at `child_indent + seq_indent + 2`, item rendering delegated to `render_match_items` with `m_indent = child_indent + seq_indent + 2`.

- [ ] **Step 1: Update existing render tests and add new ones**

Pass `2` as the new `seq_indent` argument to every existing `render_rules_entry(...)` call in the `tests` module; their expected golden strings are unchanged. Add:

```rust
#[test] fn renders_matches_entry_bare_for_splice() {
    let rules = vec![GeneratedRule { matches: vec![exact_match("/health")] }];
    let out = render_matches_entry(&rules[0].matches, 6, 2);
    assert_eq!(out, "matches:\n        - path:\n            type: Exact\n            value: '/health'\n");
}
#[test] fn renders_rules_entry_dash_aligned_when_seq_indent_zero() {
    let rules = vec![GeneratedRule { matches: vec![exact_match("/health")] }];
    let out = render_rules_entry(&rules, None, 2, 0);
    assert_eq!(out, "  rules:\n  - matches:\n    - path:\n        type: Exact\n        value: '/health'\n");
}
```

(Derive the `seq_indent = 0` layout from the indent formulas: `rules:` col 2, `- matches:` col 2, `- path:` col 4, `type:`/`value:` col 8.)

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test --lib route::` — Expected: FAIL (arity mismatch / new fns missing).

- [ ] **Step 3: Implement the refactor**

Factor today's per-match emission out of `render_rules_entry` into `render_match_items`; add `render_matches_entry`; thread `seq_indent` through. Keep `render_match`/`render_param`/`quote`/`render_backend_refs_block` as-is, called with the computed indents. Update the sole non-test caller `apply` to pass `detect_seq_indent(manifest_text)` as `seq_indent` so the crate compiles (mode selection is Task 4).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib route::` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/route.rs
git commit -m "refactor(route): parameterize renderer by sequence indent"
```

---

### Task 3: Locate the sole rule's matches span and count rules

**Files:**
- Modify: `src/route.rs`
- Test: `src/route.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: existing `RulesLocation`, `marker_byte`, `snap_to_content_end`.
- Produces, added to `RulesLocation`:
  - `pub existing_rule_count: usize` — number of items in the `spec.rules` sequence (`0` when `rules:` is absent, null, or not a sequence).
  - `pub sole_rule_matches: Option<MatchesSplice>` — `Some` only when `existing_rule_count == 1` and that rule mapping has a `matches:` key.
  - `pub struct MatchesSplice { pub range: Range<usize>, pub key_indent: usize }` where `range` is `[marker_byte(matches_key.start) .. snap_to_content_end(manifest, that_start, marker_byte(matches_value.end))]` and `key_indent` is `matches_key.span.start.col()`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn locate_reports_single_rule_matches_span() {
    let loc = locate_rules(HTTP_ROUTE_WITH_RULES).unwrap();
    assert_eq!(loc.existing_rule_count, 1);
    let ms = loc.sole_rule_matches.expect("single rule with matches");
    assert_eq!(ms.key_indent, 6);
    assert_eq!(&HTTP_ROUTE_WITH_RULES[ms.range],
        "matches:\n        - path:\n            type: Exact\n            value: /health\n");
}
#[test] fn locate_reports_multiple_rules_no_splice() {
    // two-rule manifest (reuse the shape from errors_on_multiple_httproutes' single doc)
    let loc = locate_rules(TWO_RULE_MANIFEST).unwrap();
    assert_eq!(loc.existing_rule_count, 2);
    assert!(loc.sole_rule_matches.is_none());
}
#[test] fn locate_reports_zero_rules() {
    let loc = locate_rules(HTTP_ROUTE_NO_RULES).unwrap();
    assert_eq!(loc.existing_rule_count, 0);
    assert!(loc.sole_rule_matches.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib route::` — Expected: FAIL (fields/struct missing).

- [ ] **Step 3: Implement**

In `locate_rules`, after resolving `rules_entry`, inspect its value as a sequence to set `existing_rule_count`; when exactly one item and it is a mapping with a `matches` key, build `MatchesSplice` from that key/value's markers (the `range` start is the key byte, *not* the line start, so the preceding `- ` is preserved). Populate the two new fields on every `RulesLocation` return path.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib route::` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/route.rs
git commit -m "feat(route): locate single rule matches span and rule count"
```

---

### Task 4: Mode selection in `apply`

**Files:**
- Modify: `src/route.rs`
- Test: `src/route.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `detect_seq_indent` (Task 1), `render_matches_entry` / `render_rules_entry` (Task 2), `RulesLocation.existing_rule_count` / `.sole_rule_matches` (Task 3), existing `resolve_backend_refs` / `splice`.
- Produces:
  - `fn replace_range(manifest: &str, range: &Range<usize>, entry: &str) -> String` — `manifest[..start] + entry + manifest[end..]`. Factor the `Some(range)` arm of `splice` to call it.
  - `apply` rewritten: compute `seq_indent = detect_seq_indent(text)`. **Mode 1** when `rules.len() == 1 && loc.existing_rule_count == 1 && loc.sole_rule_matches.is_some()`: `replace_range(text, &ms.range, &render_matches_entry(&rules[0].matches, ms.key_indent, seq_indent))`. **Mode 2** otherwise: warn to stderr (verbatim text above) iff `loc.existing_rule_count >= 1`, then `splice(text, &loc, &render_rules_entry(rules, resolve_backend_refs(...).as_deref(), loc.child_indent, seq_indent))`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn apply_mode1_rewrites_matches_preserving_timeouts() {
    // single rule: matches:, then backendRefs:, then timeouts: (all +2 style)
    let out = apply(Some(MANIFEST_MATCHES_BACKEND_TIMEOUTS), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(out.contains("value: '/new'"));
    assert!(out.contains("timeouts:\n      request: 60s"));   // preserved byte-for-byte
    assert!(out.contains("backendRefs:\n        - name:"));   // preserved
    assert!(!out.contains("/old"));                            // matches replaced
}
#[test] fn apply_mode1_preserves_zero_seq_indent() {
    // dash-aligned single-rule manifest; output keeps S=0 and rewrites matches
    let out = apply(Some(DASH_ALIGNED_ONE_RULE), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(out.contains("  - matches:\n    - path:"));
}
#[test] fn apply_mode1_matches_not_first_key() {
    // rule with backendRefs: before matches:; matches rewritten, backendRefs untouched, order kept
    let out = apply(Some(BACKEND_BEFORE_MATCHES), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(out.contains("value: '/new'"));
    assert!(out.contains("- name: kept-svc"));
}
#[test] fn apply_mode1_matches_last_key_no_trailing_newline() {
    assert!(!MANIFEST_NO_TRAILING_NL.ends_with('\n'));
    let out = apply(Some(MANIFEST_NO_TRAILING_NL), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(!out.ends_with('\n'));
    assert!(out.contains("value: '/new'"));
}
#[test] fn apply_mode1_preserves_comment_between_matches_and_sibling() {
    let out = apply(Some(MANIFEST_COMMENT_IN_RULE), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(out.contains("# keep me"));
}
#[test] fn apply_mode2_regenerates_when_multiple_rules() {
    // two existing rules -> full regeneration, single generated rule replaces the block
    let out = apply(Some(TWO_RULE_MANIFEST), &[one_rule("/new")], &test_cfg(None, None)).unwrap();
    assert!(out.contains("value: '/new'"));
}
```

Add `fn one_rule(path: &str) -> GeneratedRule { GeneratedRule { matches: vec![exact_match(path)] } }` to the test module. (The stderr warning itself is pinned by the CLI test in Task 6; these unit tests assert the returned text.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib route::` — Expected: FAIL.

- [ ] **Step 3: Implement `replace_range` and the new `apply`**

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib` — Expected: PASS (all `route.rs` units, including the untouched Task 2/3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/route.rs
git commit -m "feat(route): splice matches in place, regenerate as fallback"
```

---

### Task 5: End-to-end fixtures for indent and timeouts preservation

**Files:**
- Create: `tests/fixtures/seq-indent-zero/{spec.yaml,gafos.yaml,route.yaml,expected.yaml}`
- Create: `tests/fixtures/timeouts-preserved/{spec.yaml,gafos.yaml,route.yaml,expected.yaml}`
- Modify: `tests/fixtures_test.rs`

**Interfaces:**
- Consumes: the full binary behavior from Tasks 1–4. Fixture format follows existing cases: `gafos.yaml` = `spec: spec.yaml` / `route: route.yaml`; `spec.yaml` an OpenAPI 3.0.3 doc with a couple of static paths.

- [ ] **Step 1: Author `seq-indent-zero`**

`route.yaml`: a single-rule HTTPRoute in dash-aligned style (`rules:` col 2, `- matches:` col 2, `- path:` col 4) with an `/old-placeholder` match. `expected.yaml`: identical file with matches rewritten to the spec's paths, still dash-aligned (`seq_indent = 0`). `spec.yaml`: two static paths (mirror `path-only`).

- [ ] **Step 2: Author `timeouts-preserved`**

`route.yaml`: a single rule (`+2` style) with `matches:` (an `/old-placeholder` match) then a `timeouts:` block (`request: 60s`, `backendRequest: 60s`). `expected.yaml`: matches rewritten to the spec paths; the `timeouts:` block byte-for-byte unchanged. `spec.yaml`: one or two static paths.

- [ ] **Step 3: Register both cases**

Add `#[test] fn seq_indent_zero() { run_case("seq-indent-zero"); }` and `#[test] fn timeouts_preserved() { run_case("timeouts-preserved"); }` to `tests/fixtures_test.rs`.

- [ ] **Step 4: Run the fixture suite**

Run: `cargo test --test fixtures_test` — Expected: PASS for all cases, including the pre-existing ones (they must remain byte-identical; the harness's second `--check` run also proves idempotence for the new cases).

- [ ] **Step 5: Commit**

```bash
git add tests/fixtures/seq-indent-zero tests/fixtures/timeouts-preserved tests/fixtures_test.rs
git commit -m "test(fixtures): cover zero-indent and timeouts preservation"
```

---

### Task 6: CLI test for the multi-rule fall-back warning

**Files:**
- Modify: `tests/cli.rs`

**Interfaces:**
- Consumes: the binary. A route with two existing rules under `spec.rules` triggers Mode 2 with `existing_rule_count >= 1`, printing the warning to stderr and exiting `0`.

- [ ] **Step 1: Write the failing test**

```rust
#[test] fn multi_rule_route_warns_and_succeeds() {
    // route skeleton with two rules; run gafos; assert exit 0 and the warning on stderr
    // assert stderr.contains("route spans multiple rules")
}
```

Build the two-rule route inline (like `ROUTE_SKELETON`) and use `SPEC_HEALTH`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --test cli multi_rule_route_warns_and_succeeds` — Expected: FAIL.

- [ ] **Step 3: Confirm implementation**

No production change expected (the warning ships in Task 4); if the test fails on the warning text, reconcile with the verbatim string in Global Constraints.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --test cli` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tests/cli.rs
git commit -m "test(cli): warn when route spans multiple rules"
```

---

### Task 7: Documentation

**Files:**
- Modify: `README.md`
- Modify: `src/route.rs` (module and function doc comments)

**Interfaces:** none (docs only). Apply the tech-writing skill's terseness.

- [ ] **Step 1: Update README "Ownership boundary"**

State that gafos owns the `matches:` list of the managed rule, preserving `backendRefs:`/`timeouts:`/`filters:`/comments and the file's sequence-indentation style; it regenerates the whole `rules:` block only when creating a route or when the route spans multiple rules, warning in the latter case. Add `seq-indent-zero` and `timeouts-preserved` to the worked-examples table.

- [ ] **Step 2: Update `route.rs` doc comments**

Revise the module header and `render_rules_entry`/`apply` docs to describe the two modes, the bare `matches:` splice, and the detected `seq_indent` (drop any claim that indentation is hardcoded).

- [ ] **Step 3: Verify the full suite and formatting**

Run: `cargo test && cargo fmt --check && cargo clippy` — Expected: all PASS / clean.

- [ ] **Step 4: Commit**

```bash
git add README.md src/route.rs
git commit -m "docs: describe matches-only ownership and indent preservation"
```
