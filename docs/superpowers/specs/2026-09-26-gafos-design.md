# gafos design

gafos (Gateway API From OpenAPI Spec) is a dev-time CLI that reads an OpenAPI spec and
rewrites the `spec.rules` of an existing Kubernetes Gateway API HTTPRoute so the gateway
only routes requests the spec describes. Run it after editing the spec to regenerate the
managed portion of the manifest.

## Enforcement model

Enforcement is by omission: gafos generates route matches for every request the spec
allows; any request that matches no rule falls through to the gateway's default (typically
`404`). gafos does not emit deny rules.

Scope is a single HTTPRoute's `spec.rules` and nothing else. gafos never touches Gateway,
policy, or other resources, and within the HTTPRoute it owns only the `rules:` block.

## Ownership boundary

gafos owns exactly the `rules:` block under `spec:`. Everything else in the file --
`metadata`, `parentRefs`, `hostnames`, comments, key ordering, whitespace -- is preserved
byte-for-byte.

`backendRefs` inside rules are data gafos preserves, not data it authors. Resolution
precedence:

1. Reuse `backendRefs` captured from the existing managed rules.
2. Else use the `backend` from config.
3. Else emit rules with no `backendRefs`.

When existing rules carry differing `backendRefs`, gafos warns on stderr and reuses the
first.

## Command surface

```
gafos --spec openapi.yaml --route httproute.yaml [flags]
gafos --check --spec openapi.yaml --route httproute.yaml
```

Argument parsing uses `argh`.

| Flag | Meaning |
| --- | --- |
| `--spec <path>` | OpenAPI 3.0/3.1 document (required) |
| `--route <path>` | Target HTTPRoute manifest, edited in place (required) |
| `--config <path>` | Config file location (default: discover in working dir) |
| `--name <name>` | `metadata.name` used when scaffolding a missing manifest |
| `--match-methods` | Constrain by HTTP method (off = path-only) |
| `--match-query` | Add matches for `required` query parameters |
| `--match-headers` | Add matches for `required` header parameters |
| `--path-match regex\|prefix` | Templated-parameter strategy (default `regex`) |
| `--base-path <prefix>` | Prefix prepended to every generated path |
| `--backend name:port` | Fallback backend when the manifest has none |
| `--check` | No write; exit non-zero and print a diff if the file would change |

Match dimensions (methods, query, headers) are independent toggles. Paths are always
matched. New dimensions are added later as new toggles.

## Config

gafos reads config from two files, then applies CLI flags. Precedence, lowest to highest:

```
built-in defaults  <  gafos.yaml  <  gafos.local.yaml  <  CLI flags
```

`gafos.yaml` is committed project config. `gafos.local.yaml` is developer-local override
and should be gitignored. Every flag has a config equivalent.

```yaml
# gafos.yaml
spec: openapi.yaml
route: k8s/httproute.yaml
match:
  methods: true
  query: false
  headers: false
path_match: regex        # or prefix
base_path: /api/v1
backend:                 # fallback only; existing backendRefs win
  name: my-api-svc
  port: 8080
```

## Translation: OpenAPI to matches

Each operation in the spec produces one `HTTPRouteMatch`.

**Path (always).** `--base-path` is prepended before translation.

- Static path (`/health`) -> `{type: Exact, value: /health}`.
- Templated path (`/users/{id}`), `regex` -> `{type: RegularExpression, value: ^/users/[^/]+$}`.
- Templated path, `prefix` -> `{type: PathPrefix, value: /users/}`.

**Method** (`--match-methods`): adds `method: GET` to the match. When off, operations on
the same path collapse to one match per distinct path.

**Query** (`--match-query`): for each `required: true`, `in: query` parameter, add a
`queryParams` presence match `{name: q, type: RegularExpression, value: .+}`.

**Headers** (`--match-headers`): same for `required: true`, `in: header` parameters.

Query and header parameters may be `$ref`'d; gafos resolves internal `$ref`s when those
dimensions are on. Path keys are always literal.

### Rule structure

All generated matches go into a single rule; matches within a rule are OR'd. The rule
carries the resolved `backendRefs`. When the match count exceeds the Gateway API per-rule
limit of 64, gafos splits into multiple rules of at most 64 matches each, all sharing the
same `backendRefs`.

Matches are emitted in a deterministic order (by path, then method) so output is stable
and `--check` diffs are meaningful.

## Merge mechanism

The manifest is edited as text with a single spliced region, so comments, ordering, and
formatting outside `rules:` survive.

1. Locate the byte range of the `rules:` block under `spec:` using a span-aware YAML
   parser (`saphyr`, which exposes source markers).
2. Capture existing `backendRefs` from within that region.
3. Render the new `rules:` block as YAML at the correct indentation.
4. Replace only those bytes.

Writes go through a temp file and atomic rename so a crash cannot leave a half-edited
manifest.

### Merge edge cases

- **No `spec.rules` present:** insert a `rules:` block at the end of `spec:`.
- **Missing target file:** scaffold a minimal HTTPRoute (apiVersion, kind, `metadata.name`
  from `--name`/config or the route filename stem, a commented `parentRefs` placeholder,
  and the generated `rules:`). Subsequent runs treat it as rules-only.
- **Not an HTTPRoute, or 0 / more than 1 HTTPRoute documents in the file:** error, file
  untouched. v1 assumes one HTTPRoute per file.
- **Idempotent:** running twice with the same inputs produces identical bytes.

## Error handling

Fail loud; never corrupt the target.

- Unparseable spec: error with location, no write.
- Empty spec (no paths): hard error. It describes no routable surface.
- `regex` requested but a path cannot be safely regex-encoded: error naming the path.
- Differing existing `backendRefs`: warn on stderr, reuse the first, continue.

`--check` exit codes: `0` in sync, `1` stale (prints unified diff), `2` error. CI can
distinguish drift from a broken run.

## Architecture

Rust, small modules each testable in isolation.

- **`config`** -- resolve defaults, `gafos.yaml`, `gafos.local.yaml`, and CLI flags into
  one `Config`.
- **`openapi`** -- parse the spec, resolve internal `$ref`s, yield a flat list of
  `Operation { path, method, required_query, required_header }`. Pure over its input.
- **`translate`** -- `(Operation, Config) -> HTTPRouteMatch`: path strategy, base-path,
  dimension toggles, dedupe, deterministic sort, rule splitting. Pure, no I/O.
- **`route`** -- locate and splice the `rules:` region, capture `backendRefs`, render,
  write or diff. Owns the `saphyr` span logic.
- **`cli`** -- `argh` parsing, wiring, `--check` versus write, exit codes, diff output.

Data flow: `config` -> read spec -> `openapi` -> `translate` -> `route` -> write or
`--check` diff.

Candidate dependencies: `argh`, `saphyr` plus a YAML serializer for the rules block, an
OpenAPI parser (`oas3` for 3.0 and 3.1, or `openapiv3`), `similar` for diffs, and
`anyhow`/`thiserror`. Exact choices are pinned during planning.

## Testing

TDD throughout. The pure modules carry most coverage as fast unit tests.

- **`translate`:** table-driven tests over path translation (static, templated, regex
  versus prefix, base-path), each dimension toggle, dedupe with methods off, deterministic
  ordering, and rule splitting at the 64-match boundary.
- **`openapi`:** 3.0 and 3.1 fixtures, `$ref`-resolved query/header parameters,
  required-versus-optional filtering.
- **`config`:** the precedence ladder and file discovery.
- **`route`:** golden-file tests. An input manifest with comments, odd ordering, and extra
  fields is processed; everything outside `rules:` must be byte-identical and `rules:`
  correct. Covers existing rules replaced, `backendRefs` preserved, `rules:` insertion,
  scaffold-from-missing, multi-doc rejection, atomic write.
- **`--check` and CLI:** end-to-end tests asserting exit codes 0/1/2 and diff output.
- **Idempotency:** run twice, assert identical bytes.

### Fixture integration suite

Several committed `(input spec, expected HTTPRoute)` pairs live in the repo, each isolating
one feature. Tests assert generated output equals the committed expected file. The same
fixtures serve as `examples/` in the README. Initial set:

- path-only
- methods enabled
- regex versus prefix path matching
- base-path prepend
- query parameter matching
- header parameter matching
- `backendRefs` preservation
- scaffold from a missing manifest
