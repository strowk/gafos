# gafos: matches-only ownership and indentation preservation

Date: 2026-09-28
Status: Approved for planning

## Problem

gafos regenerates the entire `rules:` block of an HTTPRoute from a fixed
template, carrying forward only the first rule's `backendRefs:`. Two
consequences, both observed in real use:

1. **Indentation style is not preserved.** The renderer hardcodes a `+2`
   block-sequence indent (dash indented two spaces past its key). A route
   authored in the dash-aligned style (dash in the same column as its key,
   the common `kubectl` style) is silently reformatted to the `+2` style.

2. **Hand-added per-rule fields are dropped.** A rule's siblings of
   `matches:` — `timeouts:`, `filters:`, and anything else a user adds —
   are wiped when the block is re-rendered, because only `backendRefs:`
   is carried forward. The README already promises these are preserved
   ("add a `timeouts:` block … gafos will never touch it — only the
   generated matches change"); the implementation does not deliver it.

Both stem from the same root cause: gafos re-renders the whole `rules:`
region instead of editing only the `matches:` within it.

## Goals

- gafos rewrites only the `matches:` list of the managed rule; everything
  else in that rule (`backendRefs:`, `timeouts:`, `filters:`, comments) is
  preserved byte-for-byte.
- The `matches:` it writes uses the file's own block-sequence indentation
  style, detected from the existing manifest.
- Existing fixtures continue to pass byte-for-byte (they use the `+2`
  style, a single rule, and `matches:` as the first rule key).

## Non-goals

- Preserving per-rule fields when a route legitimately spans multiple
  rules (>64 matches, or a hand-authored multi-rule file). Those fall back
  to full-block regeneration with a warning.
- Reconciling or reordering multiple existing rules.
- Configurable indentation (no new config key or CLI flag). Indentation is
  detected, not configured.
- Changing how matches themselves are computed (`translate.rs` is
  untouched) or how scalars are quoted.

## Ownership boundary (revised)

gafos owns the **`matches:` list of the single managed rule**. It
regenerates the whole `rules:` block only when it must *create* one (no
existing rule) or *fall back* (the route spans multiple rules).

Preserved in the matches-only path: `backendRefs:`, `timeouts:`,
`filters:`, any other rule-level key, comments inside the rule, the file's
sequence-indentation style, key order, and trailing-newline presence.

## Mode selection

`apply` chooses a mode from the generated rule count (`build_rules`
output) and the existing rule count in the manifest:

| Existing rules under `spec.rules` | Generated rules | Mode |
|---|---|---|
| exactly 1, and it has a `matches:` key | exactly 1 | **Mode 1 — splice matches in place** |
| 0 (absent / empty / null `spec:`) | any | **Mode 2 — full render (create)**, no warning |
| exactly 1 but no `matches:` key | any | **Mode 2 — full render**, warn |
| more than 1 | any | **Mode 2 — full render**, warn |
| any | more than 1 (>64 matches) | **Mode 2 — full render**, warn |

The warning (stderr, exit code unchanged): `warning: route spans multiple
rules; hand-added per-rule fields (timeouts, filters) are not preserved`.
It is emitted only in the fall-back sub-cases (multiple existing rules,
sole rule without `matches:`, or more than one generated rule) — never on
the pure create path (zero existing rules), where there is nothing to
lose.

Degenerate case: zero generated rules (an empty spec) takes Mode 2, which
renders an empty `rules:` entry exactly as today.

## Mode 1 — splice matches in place

1. Locate `spec.rules`, confirm exactly one rule and that it has a
   `matches:` key.
2. Compute the replacement span: from the byte offset of the `matches`
   **key** (its column, *not* the start of its line) through
   `snap_to_content_end` of its value block. Starting at the key column
   leaves whatever precedes it on that line — the rule item's `- ` dash
   when `matches:` is the first key, or pure indentation otherwise —
   untouched.
3. Render the replacement as a **bare** `matches:` entry: the first line is
   `matches:\n` with no leading indentation (the file already supplies what
   comes before it), and the match items are indented from the `matches`
   key column `M` using the detected sequence indent `S`.
4. Splice the rendered entry into the span. Everything after it
   (`backendRefs:`, `timeouts:`, …) and everywhere else in the file is
   preserved.

Reuses the existing `line_start_byte`, `marker_byte`, and
`snap_to_content_end` helpers. `snap_to_content_end` already handles the
`matches:`-followed-by-shallower-sibling boundary (covered by
`splice_preserves_shallower_sibling_indentation_and_comment`), which is
exactly the `matches:` → `backendRefs:` / `timeouts:` case one level
deeper.

`backendRefs:` handling in Mode 1: none. It is preserved in place as part
of "everything else." `resolve_backend_refs` / `capture_first_backend_refs`
are reached only on the Mode 2 create and fall-back paths.

## Mode 2 — full render (create and fall back)

Behaviorally today's `render_rules_entry` + `splice`, with two changes:

- It uses the detected sequence indent `S` rather than a hardcoded `2`.
- It emits the stderr warning above when entered as a fall-back (not on the
  pure create path).

`backendRefs:` is carried forward from the first existing rule exactly as
today (`capture_first_backend_refs` → `resolve_backend_refs`, existing wins
over config). Per-rule fields other than `backendRefs:` are not preserved
in this path — this is the documented limitation the warning announces.

## Indentation detection

Add `detect_seq_indent(manifest parse) -> usize`:

```
S = (first_item_start_col - 2) - seq_key_col
```

where `first_item_start_col` is the column of a block sequence's first
item (saphyr reports the item content column; the dash is two columns
before it) and `seq_key_col` is the column of the key whose value that
sequence is.

Source priority:
1. `spec.rules` (its first rule item), or the `matches:` sequence within
   the first rule.
2. Any other block sequence in the HTTPRoute document (e.g.
   `spec.parentRefs`).
3. Default `2`.

Guards: only block sequences with at least one item qualify; a computed
`S < 0` is discarded in favor of the next source, and an exhausted list
falls back to `2`.

Verified by hand:
- `parentRefs:` / `  - name: …` (key col 2, item col 4) → `S = (4-2)-2 = 0`.
- fixtures `parentRefs:` / `    - name: …` (key col 2, item col 6) →
  `S = (6-2)-2 = 2`.

## Renderer parameterization

Mapping nesting stays a fixed `+2`; only the sequence-dash offset varies
with `S`. Indents, given the `matches` key column `M`:

| Element | Indent |
|---|---|
| match item dash (`- path:`) | `M + S` |
| match keys (`method:`, `queryParams:`, `headers:`) | `M + S + 2` |
| `type:` / `value:` under `path:` | `M + S + 4` |
| param item dash (`- name:`) under a match key | `M + 2S + 2` |
| param keys (`type:` / `value:`) under a param | `M + 2S + 4` |

Factor a shared `render_match_items(out, matches, M, S)` that emits the
`- path:` items (no `matches:` key line). Both callers use it:

- **Mode 1**: emit `matches:\n` (bare), then `render_match_items(M, S)`.
- **Mode 2** (`render_rules_entry`): for each rule emit
  `<child_indent + S>- matches:\n`, then `render_match_items(M, S)` with
  `M = child_indent + S + 2`, then the optional `backendRefs:` block.

With `S = 2` and `M = child_indent + 4` this reproduces today's exact
output, so Mode 2 is a behavior-preserving refactor plus the `S` knob.

## Interfaces

`route.rs`:

- `detect_seq_indent(...) -> usize` — new.
- A locate step that returns what mode selection needs: existing-rule
  count, the sole rule's `matches:` key column and replacement span (when
  present), `child_indent`, `seq_indent`, and (for Mode 2)
  `existing_backend_refs` and the `rules:` replace/insert location. This
  extends `RulesLocation` / `locate_rules` (or adds a sibling); the public
  shape is an implementation detail of the plan.
- `render_match_items(out, matches, m_indent, seq_indent)` — new shared
  helper.
- `render_rules_entry(rules, backend_refs, child_indent, seq_indent)` —
  gains `seq_indent`; delegates item rendering to `render_match_items`.
- `render_matches_entry(matches, m_indent, seq_indent) -> String` — new,
  bare `matches:` for Mode 1.
- `apply(...)` — implements mode selection above.

`main.rs` / `cli.rs`: unchanged flags; the warning is printed from within
the route module (stderr) as gafos already does for differing
`backendRefs`.

## Testing

Unit (`route.rs`):
- `detect_seq_indent` on `S=0`, `S=2`, missing-sequence-defaults-to-2,
  and negative-guard inputs.
- Mode 1 splice: single rule, `matches:` first key, `S=0` and `S=2`;
  preserves a following `timeouts:` and `backendRefs:` byte-for-byte;
  rewrites only the matches.
- Mode 2 fall back: two existing rules and >64 generated matches both
  regenerate and warn; create path (no rule) does not warn.
- `render_rules_entry` with `S=2` still equals the current golden strings
  (regression); with `S=0` produces the dash-aligned layout.

Fixtures (`tests/fixtures/`):
- All existing cases must stay byte-for-byte identical (run the suite to
  confirm; no expected-file edits anticipated).
- New `seq-indent-zero`: a route in dash-aligned style; `expected.yaml`
  keeps `S=0` and rewrites matches.
- New `timeouts-preserved`: a single rule with `matches:` then
  `timeouts:`; `expected.yaml` keeps `timeouts:` and changes only matches.

CLI (`tests/cli.rs`): a multi-rule input (or >64 matches) prints the
fall-back warning to stderr and still exits 0.

## Documentation

Update README "Ownership boundary": gafos owns the `matches:` of the
managed rule, preserving `backendRefs:`/`timeouts:`/`filters:`/comments and
the file's sequence-indentation style; it regenerates the full `rules:`
block only when creating a route or when the route spans multiple rules,
warning in the latter case. Adjust the worked-examples table for the two
new fixtures. Update module and function doc comments in `route.rs` to
describe the two modes and the detected indent.

## Risks

- **saphyr column reporting.** Detection and the Mode 1 key-column span
  depend on `Marker` columns being 0-based and pointing where assumed.
  The plan verifies against real fixtures before building on it; if a
  marker points at the dash rather than the content, the `-2` in
  `detect_seq_indent` and the span start are adjusted once, centrally.
- **Fixture drift.** If any existing fixture is not byte-identical under
  Mode 1, that is a signal the splice boundary is wrong, not a reason to
  edit the fixture. Investigate the boundary first.
