//! Layered configuration: built-in defaults < gafos.yaml < gafos.local.yaml < CLI flags.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// How the route path is matched against the OpenAPI spec's paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathMatch {
    #[default]
    Regex,
    Prefix,
}

/// A backendRef target: a Service name and port.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Backend {
    pub name: String,
    pub port: u16,
}

/// Fully resolved configuration, after folding all override layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub spec: PathBuf,
    pub route: PathBuf,
    pub name: Option<String>,
    pub match_methods: bool,
    pub match_query: bool,
    pub match_headers: bool,
    pub path_match: PathMatch,
    pub base_path: Option<String>,
    pub backend: Option<Backend>,
    pub check: bool,
}

/// One layer of configuration: a config file, or CLI flags. All fields
/// optional so an unset field does not clobber a value set by a lower layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    pub spec: Option<PathBuf>,
    pub route: Option<PathBuf>,
    pub name: Option<String>,
    pub match_methods: Option<bool>,
    pub match_query: Option<bool>,
    pub match_headers: Option<bool>,
    pub path_match: Option<PathMatch>,
    pub base_path: Option<String>,
    pub backend: Option<Backend>,
    pub check: Option<bool>,
}

/// Match-dimension toggles as they appear nested under `match:` in a config file.
#[derive(Debug, Deserialize, Default)]
struct MatchFile {
    #[serde(default)]
    methods: Option<bool>,
    #[serde(default)]
    query: Option<bool>,
    #[serde(default)]
    headers: Option<bool>,
}

/// The on-disk shape of a `gafos.yaml` / `gafos.local.yaml` file.
#[derive(Debug, Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    spec: Option<PathBuf>,
    #[serde(default)]
    route: Option<PathBuf>,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "match", default)]
    r#match: Option<MatchFile>,
    #[serde(default)]
    path_match: Option<PathMatch>,
    #[serde(default)]
    base_path: Option<String>,
    #[serde(default)]
    backend: Option<Backend>,
    #[serde(default)]
    check: Option<bool>,
}

impl From<ConfigFile> for Overrides {
    fn from(file: ConfigFile) -> Self {
        let (match_methods, match_query, match_headers) = match file.r#match {
            Some(m) => (m.methods, m.query, m.headers),
            None => (None, None, None),
        };
        Overrides {
            spec: file.spec,
            route: file.route,
            name: file.name,
            match_methods,
            match_query,
            match_headers,
            path_match: file.path_match,
            base_path: file.base_path,
            backend: file.backend,
            check: file.check,
        }
    }
}

/// Fold override layers left to right; later layers win field-by-field.
/// `spec` and `route` must be resolved by some layer or this errors.
pub fn resolve(layers: &[Overrides]) -> Result<Config> {
    let mut spec: Option<PathBuf> = None;
    let mut route: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut match_methods: Option<bool> = None;
    let mut match_query: Option<bool> = None;
    let mut match_headers: Option<bool> = None;
    let mut path_match: Option<PathMatch> = None;
    let mut base_path: Option<String> = None;
    let mut backend: Option<Backend> = None;
    let mut check: Option<bool> = None;

    for layer in layers {
        if layer.spec.is_some() {
            spec = layer.spec.clone();
        }
        if layer.route.is_some() {
            route = layer.route.clone();
        }
        if layer.name.is_some() {
            name = layer.name.clone();
        }
        if layer.match_methods.is_some() {
            match_methods = layer.match_methods;
        }
        if layer.match_query.is_some() {
            match_query = layer.match_query;
        }
        if layer.match_headers.is_some() {
            match_headers = layer.match_headers;
        }
        if layer.path_match.is_some() {
            path_match = layer.path_match;
        }
        if layer.base_path.is_some() {
            base_path = layer.base_path.clone();
        }
        if layer.backend.is_some() {
            backend = layer.backend.clone();
        }
        if layer.check.is_some() {
            check = layer.check;
        }
    }

    let spec = spec.ok_or_else(|| anyhow::anyhow!("missing required config field: spec"))?;
    let route = route.ok_or_else(|| anyhow::anyhow!("missing required config field: route"))?;

    Ok(Config {
        spec,
        route,
        name,
        match_methods: match_methods.unwrap_or(false),
        match_query: match_query.unwrap_or(false),
        match_headers: match_headers.unwrap_or(false),
        path_match: path_match.unwrap_or_default(),
        base_path,
        backend,
        check: check.unwrap_or(false),
    })
}

/// Parse a YAML config file into an `Overrides` layer.
pub fn load_file(path: &Path) -> Result<Overrides> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file {}", path.display()))?;
    let file: ConfigFile = serde_yaml::from_str(&text)
        .with_context(|| format!("parsing config file {}", path.display()))?;
    Ok(file.into())
}

/// Return existing `gafos.yaml` then `gafos.local.yaml` under `dir`, in that
/// order, skipping either that does not exist.
pub fn discover(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let base = dir.join("gafos.yaml");
    if base.is_file() {
        found.push(base);
    }
    let local = dir.join("gafos.local.yaml");
    if local.is_file() {
        found.push(local);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn defaults_then_file_then_cli_precedence() {
        let file = Overrides {
            spec: Some(PathBuf::from("openapi.yaml")),
            route: Some(PathBuf::from("k8s/httproute.yaml")),
            match_methods: Some(true),
            ..Default::default()
        };
        let cli = Overrides {
            match_methods: Some(false),
            ..Default::default()
        };

        let config = resolve(&[file, cli]).expect("should resolve");

        assert_eq!(config.spec, PathBuf::from("openapi.yaml"));
        assert_eq!(config.route, PathBuf::from("k8s/httproute.yaml"));
        assert!(!config.match_methods);
    }

    #[test]
    fn missing_spec_or_route_is_error() {
        let file = Overrides {
            spec: Some(PathBuf::from("openapi.yaml")),
            // route is missing
            ..Default::default()
        };

        let result = resolve(&[file]);

        assert!(result.is_err());
    }

    #[test]
    fn load_file_parses_backend_and_path_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("gafos.yaml");
        std::fs::write(
            &path,
            r#"
spec: openapi.yaml
route: k8s/httproute.yaml
match: { methods: true, query: false, headers: false }
path_match: regex
base_path: /api/v1
backend: { name: my-api-svc, port: 8080 }
"#,
        )
        .expect("write config file");

        let overrides = load_file(&path).expect("should parse");

        assert_eq!(overrides.spec, Some(PathBuf::from("openapi.yaml")));
        assert_eq!(overrides.route, Some(PathBuf::from("k8s/httproute.yaml")));
        assert_eq!(overrides.match_methods, Some(true));
        assert_eq!(overrides.match_query, Some(false));
        assert_eq!(overrides.match_headers, Some(false));
        assert_eq!(overrides.path_match, Some(PathMatch::Regex));
        assert_eq!(overrides.base_path, Some("/api/v1".to_string()));
        assert_eq!(
            overrides.backend,
            Some(Backend {
                name: "my-api-svc".to_string(),
                port: 8080,
            })
        );
    }

    #[test]
    fn discover_orders_base_before_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().join("gafos.yaml");
        let local = dir.path().join("gafos.local.yaml");
        std::fs::write(&base, "spec: openapi.yaml\nroute: k8s/httproute.yaml\n")
            .expect("write base");
        std::fs::write(&local, "spec: openapi.yaml\n").expect("write local");

        let found = discover(dir.path());

        assert_eq!(found, vec![base, local]);
    }

    #[test]
    fn discover_skips_absent_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = dir.path().join("gafos.local.yaml");
        std::fs::write(&local, "spec: openapi.yaml\n").expect("write local");

        let found = discover(dir.path());

        assert_eq!(found, vec![local]);
    }
}
