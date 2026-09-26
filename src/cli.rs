//! CLI argument parsing and the `run` entry point.
//!
//! Wires together `config` (resolve the effective settings), `openapi`
//! (parse the spec), `translate` (build match rules), and `route` (render
//! and splice the rules into the HTTPRoute manifest, or write it to disk).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use argh::FromArgs;
use similar::TextDiff;

use crate::config::{self, Backend, Overrides, PathMatch};
use crate::openapi;
use crate::route;
use crate::translate;

/// Generate or update a Gateway API HTTPRoute from an OpenAPI spec.
#[derive(FromArgs, Debug)]
pub struct Args {
    /// path to the OpenAPI spec (overrides config)
    #[argh(option)]
    pub spec: Option<String>,

    /// path to the HTTPRoute manifest to write or update (overrides config)
    #[argh(option)]
    pub route: Option<String>,

    /// path to a specific config file; skips discovery of gafos.yaml /
    /// gafos.local.yaml when given
    #[argh(option)]
    pub config: Option<String>,

    /// name to use when scaffolding a fresh HTTPRoute (overrides config)
    #[argh(option)]
    pub name: Option<String>,

    /// match on HTTP method. Can only enable this dimension over what a
    /// config file set; to disable it, edit the config file instead
    #[argh(switch)]
    pub match_methods: bool,

    /// match on required query parameters. Can only enable this dimension
    /// over what a config file set; to disable it, edit the config file
    /// instead
    #[argh(switch)]
    pub match_query: bool,

    /// match on required headers. Can only enable this dimension over what
    /// a config file set; to disable it, edit the config file instead
    #[argh(switch)]
    pub match_headers: bool,

    /// how to match templated paths: "regex" or "prefix" (overrides config)
    #[argh(option)]
    pub path_match: Option<String>,

    /// prefix prepended to every OpenAPI path (overrides config)
    #[argh(option)]
    pub base_path: Option<String>,

    /// backend target as "name:port" (overrides config)
    #[argh(option)]
    pub backend: Option<String>,

    /// print a diff of what would change instead of writing the file; exit
    /// 1 if the route file is stale, 0 if it is already in sync
    #[argh(switch)]
    pub check: bool,
}

impl Args {
    /// Convert the CLI flags into a config `Overrides` layer.
    ///
    /// The three match-dimension switches and `--check` are plain booleans:
    /// present becomes `Some(true)` (turn the dimension on), absent becomes
    /// `None` (defer to a lower layer) — never `Some(false)`. So a CLI
    /// switch can only ENABLE a dimension over what a config file set; to
    /// disable one, edit the config file.
    fn to_overrides(&self) -> Result<Overrides> {
        let path_match = match self.path_match.as_deref() {
            Some("regex") => Some(PathMatch::Regex),
            Some("prefix") => Some(PathMatch::Prefix),
            Some(other) => {
                bail!("invalid --path-match value {other:?}: expected \"regex\" or \"prefix\"")
            }
            None => None,
        };
        let backend = self.backend.as_deref().map(parse_backend).transpose()?;

        Ok(Overrides {
            spec: self.spec.as_ref().map(PathBuf::from),
            route: self.route.as_ref().map(PathBuf::from),
            name: self.name.clone(),
            match_methods: switch_override(self.match_methods),
            match_query: switch_override(self.match_query),
            match_headers: switch_override(self.match_headers),
            path_match,
            base_path: self.base_path.clone(),
            backend,
            check: switch_override(self.check),
        })
    }
}

/// A present CLI switch enables a dimension (`Some(true)`); an absent one
/// defers to a lower config layer (`None`), never disabling it.
fn switch_override(present: bool) -> Option<bool> {
    present.then_some(true)
}

/// Parse a `--backend name:port` value into a `Backend`.
fn parse_backend(spec: &str) -> Result<Backend> {
    let (name, port) = spec
        .split_once(':')
        .ok_or_else(|| anyhow!("invalid --backend {spec:?}: expected \"name:port\""))?;
    let port: u16 = port.parse().map_err(|_| {
        anyhow!("invalid --backend {spec:?}: port must be a number between 0 and 65535")
    })?;
    Ok(Backend {
        name: name.to_string(),
        port,
    })
}

/// Resolve config, parse the spec, translate it into rules, and either
/// write the updated HTTPRoute manifest or (`--check`) report whether it is
/// stale. Returns the intended process exit code: 0 on a successful write
/// or an in-sync check, 1 when `--check` finds the file stale (after
/// printing a unified diff to stdout).
pub fn run(args: Args) -> Result<i32> {
    let mut layers: Vec<Overrides> = Vec::new();
    match &args.config {
        Some(path) => layers.push(config::load_file(Path::new(path))?),
        None => {
            for path in config::discover(Path::new(".")) {
                layers.push(config::load_file(&path)?);
            }
        }
    }
    layers.push(args.to_overrides()?);

    let mut cfg = config::resolve(&layers)?;

    let spec_text = std::fs::read_to_string(&cfg.spec)
        .with_context(|| format!("reading spec file {}", cfg.spec.display()))?;
    let ops = openapi::parse(&spec_text)?;
    if ops.is_empty() {
        bail!(
            "spec {} describes no operations; nothing to route",
            cfg.spec.display()
        );
    }

    let rules = translate::build_rules(&ops, &cfg);

    let existing: Option<String> = if cfg.route.is_file() {
        Some(
            std::fs::read_to_string(&cfg.route)
                .with_context(|| format!("reading route file {}", cfg.route.display()))?,
        )
    } else {
        None
    };

    if cfg.name.is_none()
        && let Some(stem) = cfg.route.file_stem().and_then(|s| s.to_str())
    {
        cfg.name = Some(stem.to_string());
    }

    let new_text = route::apply(existing.as_deref(), &rules, &cfg)?;

    if cfg.check {
        let current = existing.as_deref().unwrap_or("");
        if current == new_text {
            return Ok(0);
        }
        let diff = TextDiff::from_lines(current, new_text.as_str())
            .unified_diff()
            .header("current", "generated")
            .to_string();
        print!("{diff}");
        return Ok(1);
    }

    route::write_atomic(&cfg.route, &new_text)?;
    Ok(0)
}
