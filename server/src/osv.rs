use std::env;
use std::error::Error;
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::catalog::Module;
use crate::repositories::AGENT;

const OSV: &str = "https://api.osv.dev";

static QUERY_URL: OnceLock<Option<String>> = OnceLock::new();

/// On unless the initialization options set `vulnerabilities` to `false`. The tests point it to a
/// local server through `GRADLE_CATALOG_LSP_OSV`.
pub fn configure(options: Option<&Value>) {
    let off =
        options.and_then(|options| options.get("vulnerabilities")) == Some(&Value::Bool(false));
    let osv = env::var("GRADLE_CATALOG_LSP_OSV").unwrap_or_else(|_| OSV.to_string());
    let _ = QUERY_URL.set((!off).then(|| format!("{osv}/v1/query")));
}

pub fn enabled() -> bool {
    QUERY_URL.get().is_some_and(Option::is_some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Unknown,
    Low,
    Moderate,
    High,
    Critical,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Advisory {
    pub id: String,
    pub severity: Severity,
}

/// The advisories that affect this version of the module, or `None` when OSV failed.
pub fn query(module: &Module, version: &str) -> Option<Vec<Advisory>> {
    let url = QUERY_URL.get()?.as_deref()?;
    let name = format!("{}:{}", module.group, module.name);
    let query = json!({ "package": { "ecosystem": "Maven", "name": name }, "version": version });
    let answer = post(url, &query)
        .inspect_err(|error| eprintln!("{url} ({name}:{version}): {error}"))
        .ok()?;
    let vulns = answer.get("vulns").and_then(Value::as_array);
    let advisories = vulns
        .into_iter()
        .flatten()
        .filter_map(|vuln| {
            Some(Advisory {
                id: vuln.get("id")?.as_str()?.to_string(),
                severity: severity(vuln),
            })
        })
        .collect();
    Some(advisories)
}

fn post(url: &str, query: &Value) -> Result<Value, Box<dyn Error>> {
    let answer = AGENT
        .post(url)
        .content_type("application/json")
        .send(query.to_string())?
        .body_mut()
        .read_to_string()?;
    Ok(serde_json::from_str(&answer)?)
}

/// The severity GitHub gives its advisories, which cover Maven on OSV. The CVSS vectors that other
/// databases give instead are not scored.
fn severity(vuln: &Value) -> Severity {
    match vuln
        .pointer("/database_specific/severity")
        .and_then(Value::as_str)
    {
        Some("CRITICAL") => Severity::Critical,
        Some("HIGH") => Severity::High,
        Some("MODERATE") => Severity::Moderate,
        Some("LOW") => Severity::Low,
        _ => Severity::Unknown,
    }
}
