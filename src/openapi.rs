//! Flattens an OpenAPI spec into a flat list of operations for route matching.

use anyhow::{Context, Result};
use oas3::Spec;
use oas3::spec::{ObjectOrReference, Parameter, ParameterIn};

/// One method+path operation, with its required query and header parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub path: String,
    pub method: String,
    pub required_query: Vec<String>,
    pub required_header: Vec<String>,
}

/// Parse an OpenAPI document (YAML or JSON, 3.0 or 3.1) into a flat list of
/// operations.
///
/// Path-item-level and operation-level parameters are merged; `$ref`
/// parameters are resolved against the spec's `components`. Only parameters
/// marked `required: true` are kept, split into `required_query` /
/// `required_header` by their `in` location. Returns an empty `Vec` if the
/// spec has no paths.
pub fn parse(spec_src: &str) -> Result<Vec<Operation>> {
    let spec: Spec = oas3::from_yaml(spec_src).context("parsing OpenAPI spec")?;

    let Some(paths) = spec.paths.as_ref() else {
        return Ok(Vec::new());
    };

    let mut operations = Vec::new();

    for (path, item) in paths.iter() {
        for (method, op) in item.methods() {
            let mut required_query = Vec::new();
            let mut required_header = Vec::new();

            for oor in item.parameters.iter().chain(op.parameters.iter()) {
                let param = resolve_parameter(oor, &spec)?;
                if param.required != Some(true) {
                    continue;
                }
                match param.location {
                    ParameterIn::Query => required_query.push(param.name),
                    ParameterIn::Header => required_header.push(param.name),
                    _ => {}
                }
            }

            operations.push(Operation {
                path: path.clone(),
                method: method.as_str().to_string(),
                required_query,
                required_header,
            });
        }
    }

    Ok(operations)
}

/// Resolve a parameter, following an internal `$ref` against `spec` if needed.
fn resolve_parameter(oor: &ObjectOrReference<Parameter>, spec: &Spec) -> Result<Parameter> {
    oor.resolve(spec).context("resolving parameter $ref")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_30: &str = r#"
openapi: 3.0.3
info:
  title: Test
  version: "1.0"
paths:
  /health:
    get:
      responses:
        "200":
          description: ok
  /users/{id}:
    get:
      responses:
        "200":
          description: ok
    delete:
      responses:
        "204":
          description: ok
"#;

    const SPEC_31: &str = r#"
openapi: 3.1.0
info:
  title: Test
  version: "1.0"
paths:
  /health:
    get:
      responses:
        "200":
          description: ok
  /users/{id}:
    get:
      responses:
        "200":
          description: ok
    delete:
      responses:
        "204":
          description: ok
"#;

    const SPEC_REQUIRED_PARAMS: &str = r#"
openapi: 3.0.3
info:
  title: Test
  version: "1.0"
paths:
  /items:
    parameters:
      - name: page
        in: query
        required: true
        schema:
          type: integer
    get:
      parameters:
        - name: q
          in: query
          required: true
          schema:
            type: string
        - name: X-Key
          in: header
          required: true
          schema:
            type: string
        - name: optional
          in: query
          required: false
          schema:
            type: string
      responses:
        "200":
          description: ok
"#;

    const SPEC_REF_PARAM: &str = r#"
openapi: 3.0.3
info:
  title: Test
  version: "1.0"
paths:
  /items:
    get:
      parameters:
        - $ref: '#/components/parameters/ApiKey'
      responses:
        "200":
          description: ok
components:
  parameters:
    ApiKey:
      name: X-Api-Key
      in: header
      required: true
      schema:
        type: string
"#;

    const SPEC_NO_PATHS: &str = r#"
openapi: 3.0.3
info:
  title: Test
  version: "1.0"
paths: {}
"#;

    #[test]
    fn parses_paths_and_methods_30() {
        let ops = parse(SPEC_30).expect("should parse");

        assert_eq!(ops.len(), 3);
        assert!(
            ops.iter()
                .any(|op| op.path == "/health" && op.method == "GET")
        );
        assert!(
            ops.iter()
                .any(|op| op.path == "/users/{id}" && op.method == "GET")
        );
        assert!(
            ops.iter()
                .any(|op| op.path == "/users/{id}" && op.method == "DELETE")
        );
    }

    #[test]
    fn parses_31_spec() {
        let ops = parse(SPEC_31).expect("should parse");

        assert_eq!(ops.len(), 3);
        assert!(
            ops.iter()
                .any(|op| op.path == "/health" && op.method == "GET")
        );
        assert!(
            ops.iter()
                .any(|op| op.path == "/users/{id}" && op.method == "GET")
        );
        assert!(
            ops.iter()
                .any(|op| op.path == "/users/{id}" && op.method == "DELETE")
        );
    }

    #[test]
    fn collects_required_query_and_header() {
        let ops = parse(SPEC_REQUIRED_PARAMS).expect("should parse");

        assert_eq!(ops.len(), 1);
        let op = &ops[0];

        let mut required_query = op.required_query.clone();
        required_query.sort();
        assert_eq!(required_query, vec!["page".to_string(), "q".to_string()]);

        assert_eq!(op.required_header, vec!["X-Key".to_string()]);

        assert!(!op.required_query.contains(&"optional".to_string()));
    }

    #[test]
    fn resolves_ref_parameters() {
        let ops = parse(SPEC_REF_PARAM).expect("should parse");

        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].required_header, vec!["X-Api-Key".to_string()]);
        assert!(ops[0].required_query.is_empty());
    }

    #[test]
    fn no_paths_returns_empty() {
        let ops = parse(SPEC_NO_PATHS).expect("should parse");

        assert!(ops.is_empty());
    }
}
