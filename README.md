# gafos

**gafos** — Gateway API From OpenAPI Spec — generates and maintains the
`rules:` of a Kubernetes Gateway API `HTTPRoute` from an OpenAPI document.
Point it at a spec and a route manifest, and it keeps the route's matches in
sync with the spec's paths, methods, and required parameters.

## Installation

macOS, Linux, or Windows Git Bash:

```sh
curl -fsSL https://raw.githubusercontent.com/strowk/gafos/main/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/strowk/gafos/main/install.ps1 | iex
```

Both scripts detect your OS/architecture, download the matching release
binary, and install it (default: `$HOME/.local/bin` on Unix,
`%LOCALAPPDATA%\gafos\bin` on Windows). Override the target version with
`GAFOS_VERSION` and the install location with `GAFOS_INSTALL_DIR`.

The repository is currently private, so downloading a release requires a
`GITHUB_TOKEN` (or `GH_TOKEN`) environment variable set to a token with
`repo` scope until the repo goes public.

### Build from source

```sh
cargo build --release
```

The binary is written to `target/release/gafos`.

## Usage

```
gafos --spec openapi.yaml --route k8s/httproute.yaml
```

This parses `openapi.yaml`, builds match rules from its paths, and writes
them into the `rules:` block of `k8s/httproute.yaml` (scaffolding a minimal
HTTPRoute first if that file doesn't exist yet).

Run the same command again and nothing changes — the tool is idempotent.

### `--check`

```
gafos --spec openapi.yaml --route k8s/httproute.yaml --check
```

Instead of writing, prints a unified diff of what would change and exits `1`
if the route is stale, `0` if it's already in sync. Use this in CI to fail a
build when someone edited the spec but forgot to regenerate the route (or
vice versa).

### Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Wrote the route, or (`--check`) it was already in sync |
| `1`  | (`--check` only) the route is stale |
| `2`  | Error (bad spec, bad config, bad arguments, I/O failure, ...) |

## Flags

| Flag | Description |
|------|-------------|
| `--spec <path>` | Path to the OpenAPI spec (YAML or JSON, 3.0 or 3.1). |
| `--route <path>` | Path to the HTTPRoute manifest to write or update. |
| `--config <path>` | Use this exact config file instead of discovering `gafos.yaml` / `gafos.local.yaml` in the current directory. |
| `--name <name>` | Name to give a freshly scaffolded HTTPRoute (ignored if the route file already exists). |
| `--match-methods` | Match on HTTP method. |
| `--match-query` | Match on required query parameters. |
| `--match-headers` | Match on required headers. |
| `--path-match <regex\|prefix>` | How to match templated paths (`{id}`-style segments): as an anchored regex, or by truncating to the literal prefix before the first `{`. |
| `--base-path <path>` | Prefix prepended to every path from the spec before it's matched. |
| `--backend name:port` | Backend Service and port to attach as `backendRefs`, used only when the route has none already. |
| `--check` | Print a diff and report sync status instead of writing (see above). |

The three `--match-*` flags and `--check` are switches: passing one only
turns that dimension **on** over what a config file set. There's no
`--no-match-methods` — to turn a dimension off, edit the config file.

## Configuration and precedence

gafos looks for `gafos.yaml`, then `gafos.local.yaml`, in the current
directory (unless `--config <path>` names an exact file, in which case
discovery is skipped). Settings are layered, each layer overriding only the
fields it sets:

```
built-in defaults  <  gafos.yaml  <  gafos.local.yaml  <  CLI flags
```

`gafos.local.yaml` is meant for a developer's local overrides (e.g. pointing
`route` at a scratch file) and is typically gitignored; `gafos.yaml` is the
committed, shared configuration.

`gafos.yaml` shape:

```yaml
spec: openapi.yaml
route: k8s/httproute.yaml
name: my-route            # used only when scaffolding a fresh HTTPRoute
match:
  methods: true
  query: false
  headers: false
path_match: regex          # "regex" (default) or "prefix"
base_path: /api/v1
backend:
  name: my-api-svc
  port: 8080
```

`spec` and `route` must be resolved by some layer (config file or `--spec`
`--route` flags) — gafos exits `2` if either is missing after folding all
layers.

## Design Philosophy

gafos is designed to solve one problem very well: 
**keeping an HTTPRoute's `rules:` in a file in sync with a spec file**.
One file in - one (modified) file out.

There is no goal to run this as a k8s controller or operator,
gafos is meant to run in CI or as a pre-commit step or manually against
a spec that is the source of truth for the service's API surface, making
re-generated HTTPRoute manifest a deterministically derived artifact,
which should (most likely) still be checked into the repo alongside the spec,
because you would most likely include in that file other parts besides 
the `rules:` block, most importantly the `backendRefs:`.

## Ownership boundary

gafos owns exactly one thing in the HTTPRoute manifest: the `rules:` mapping
entry under `spec:`. Everything else — `apiVersion`, `metadata`, `hostnames`,
`parentRefs`, comments, key order, indentation, trailing-newline presence —
is preserved byte-for-byte. If `route` doesn't exist yet, gafos scaffolds a
minimal HTTPRoute (with a commented `parentRefs:` placeholder reminding you
to attach it to your Gateway) and inserts `rules:` into that.

This means you can hand-edit hostnames, add a `timeouts:` block, or leave a
comment next to `rules:`, and gafos will never touch it — only the generated
matches change from run to run.

### backendRef precedence

Each generated rule can carry a `backendRefs:` entry, resolved in this order:

1. **Existing** — if the route's current `rules:` already has a
   `backendRefs:` on its first rule, that value is kept as-is and carried
   forward untouched.
2. **Config** — otherwise, `backend:` from the resolved config (`gafos.yaml`
   or `--backend name:port`) is rendered as the `backendRefs:` value.
3. **Empty** — otherwise, no `backendRefs:` is emitted at all.

In other words: gafos never overwrites a `backendRefs:` you (or a previous
gafos run) already put in the route. This lets you attach the real backend
by hand once and have gafos regenerate matches indefinitely without clobbering
it.

## Worked examples

`tests/fixtures/` contains one directory per feature, each a minimal,
runnable example: a `spec.yaml`, a `gafos.yaml`, a starting `route.yaml`
(where applicable), and the exact `expected.yaml` gafos produces from them.
`tests/fixtures_test.rs` runs the real binary against every case and asserts
byte-for-byte output, so these examples are also the tool's acceptance
suite:

| Case | Demonstrates |
|------|--------------|
| `path-only` | Baseline: static paths only, no method/query/header matching. |
| `methods` | `--match-methods` / `match.methods`, one match per method. |
| `path-regex` | `path_match: regex` on a templated path (`RegularExpression`). |
| `path-prefix` | `path_match: prefix` on a templated path (`PathPrefix`). |
| `base-path` | `base_path` prepended to every spec path. |
| `query-params` | `--match-query` / `match.query`, required query params only. |
| `header-params` | `--match-headers` / `match.headers`, required headers only. |
| `backend-preserved` | An existing `backendRefs:` wins over the config's `backend:`. |
| `scaffold-missing` | Scaffolding a fresh HTTPRoute when `route` doesn't exist yet. |
