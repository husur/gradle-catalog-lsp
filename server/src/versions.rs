use std::cmp::Ordering;
use std::env;
use std::sync::LazyLock;
use std::time::Duration;

use crate::catalog::Module;

const DEFAULT_REPOSITORIES: [&str; 3] = [
    "https://repo1.maven.org/maven2",
    "https://dl.google.com/dl/android/maven2",
    "https://plugins.gradle.org/m2",
];

const PRERELEASES: [&str; 12] = [
    "alpha",
    "beta",
    "rc",
    "cr",
    "m",
    "milestone",
    "snapshot",
    "dev",
    "eap",
    "preview",
    "pre",
    "canary",
];

/// The tests serve the metadata from a local server, listed in `GRADLE_CATALOG_LSP_REPOSITORIES`
/// (comma-separated) in place of the default repositories.
static REPOSITORIES: LazyLock<Vec<String>> =
    LazyLock::new(|| match env::var("GRADLE_CATALOG_LSP_REPOSITORIES") {
        Ok(list) => list.split(',').map(String::from).collect(),
        Err(_) => DEFAULT_REPOSITORIES.map(String::from).to_vec(),
    });

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
});

pub fn fetch(module: &Module) -> Vec<String> {
    let path = format!(
        "{}/{}/maven-metadata.xml",
        module.group.replace('.', "/"),
        module.name
    );
    REPOSITORIES
        .iter()
        .find_map(|repository| {
            AGENT
                .get(format!("{repository}/{path}"))
                .call()
                .ok()?
                .body_mut()
                .read_to_string()
                .ok()
        })
        .map(|metadata| {
            metadata
                .split("<version>")
                .skip(1)
                .filter_map(|tail| Some(tail.split_once("</version>")?.0.trim().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn latest<'a>(current: &str, available: &'a [String]) -> Option<&'a str> {
    let flavor = qualifiers(current);
    let prerelease = flavor.iter().any(|q| PRERELEASES.contains(&q.as_str()));
    available
        .iter()
        .map(String::as_str)
        .filter(|candidate| prerelease || qualifiers(candidate) == flavor)
        .max_by(|a, b| compare(a, b))
        .filter(|newest| compare(newest, current).is_gt())
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Text(u8, String),
    Number(u64),
}

/// Gradle's ordering: numbers beat text, `dev` is the lowest text and
/// `rc < snapshot < final < ga < release < sp` beat any other text.
pub fn compare(a: &str, b: &str) -> Ordering {
    let (a, b) = (parts(a), parts(b));
    for index in 0..a.len().max(b.len()) {
        let ordering = match (a.get(index), b.get(index)) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(Part::Number(_)), None) | (None, Some(Part::Text(..))) => Ordering::Greater,
            _ => Ordering::Less,
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
    Ordering::Equal
}

fn parts(version: &str) -> Vec<Part> {
    tokens(version)
        .into_iter()
        .map(|token| match token.parse() {
            Ok(number) => Part::Number(number),
            Err(_) if token.chars().all(|c| c.is_ascii_digit()) => Part::Number(u64::MAX),
            Err(_) => {
                let rank = ["dev", "", "rc", "snapshot", "final", "ga", "release", "sp"]
                    .iter()
                    .position(|special| *special == token)
                    .unwrap_or(1) as u8;
                Part::Text(rank, if rank == 1 { token } else { String::new() })
            }
        })
        .collect()
}

fn qualifiers(version: &str) -> Vec<String> {
    tokens(version)
        .into_iter()
        .filter(|token| !token.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

fn tokens(version: &str) -> Vec<String> {
    let mut tokens: Vec<String> = vec![];
    let mut previous_digit = None;
    for c in version.to_lowercase().chars() {
        if matches!(c, '.' | '-' | '_' | '+') {
            previous_digit = None;
            continue;
        }
        let digit = c.is_ascii_digit();
        match tokens.last_mut() {
            Some(token) if previous_digit == Some(digit) => token.push(c),
            _ => tokens.push(c.to_string()),
        }
        previous_digit = Some(digit);
    }
    tokens
}
