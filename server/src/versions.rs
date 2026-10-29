use std::cmp::Ordering;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};
use std::time::Duration;
use std::{env, fs};

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::Value;
use ureq::http::Uri;

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

pub struct Repository {
    url: String,
    authorization: Option<String>,
}

impl Repository {
    /// Reads an entry of the `repositories` initialization option, and resolves its credentials
    /// through `property`. Refuses an entry whose URL embeds credentials, since errors can echo a
    /// URL, and one that would send credentials in clear text to another machine.
    pub fn configured(entry: &Value, property: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let url = entry.get("url")?.as_str()?.trim_end_matches('/');
        let uri: Uri = url.parse().ok()?;
        let authority = uri.authority()?;
        if authority.as_str().contains('@') {
            return None;
        }
        let authorization = match entry.get("credentials") {
            None => None,
            Some(name) => {
                let name = name.as_str()?;
                let username = property(&format!("{name}Username"))?;
                let password = property(&format!("{name}Password"))?;
                let encoded = BASE64_STANDARD.encode(format!("{username}:{password}"));
                Some(format!("Basic {encoded}"))
            }
        };
        let host = authority
            .host()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let loopback = host == "localhost" || host.parse().is_ok_and(|ip: IpAddr| ip.is_loopback());
        let secure = match uri.scheme_str()? {
            "https" => true,
            "http" => authorization.is_none() || loopback,
            _ => false,
        };
        secure.then(|| Self {
            url: url.to_string(),
            authorization,
        })
    }
}

static REPOSITORIES: OnceLock<Vec<Repository>> = OnceLock::new();

/// Queries the repositories configured in the initialization options first, then the default ones.
/// The tests serve the metadata from local servers, listed in `GRADLE_CATALOG_LSP_REPOSITORIES`
/// (comma-separated) in place of the default repositories.
pub fn configure(options: Option<&Value>) {
    let configured = options
        .and_then(|options| options.get("repositories")?.as_array())
        .into_iter()
        .flatten()
        .filter_map(|entry| Repository::configured(entry, gradle_property));
    let defaults: Vec<String> = match env::var("GRADLE_CATALOG_LSP_REPOSITORIES") {
        Ok(list) => list.split(',').map(String::from).collect(),
        Err(_) => DEFAULT_REPOSITORIES.map(String::from).to_vec(),
    };
    let defaults = defaults.into_iter().map(|url| Repository {
        url,
        authorization: None,
    });
    let _ = REPOSITORIES.set(configured.chain(defaults).collect());
}

/// Reads a property the way Gradle does for `credentials(PasswordCredentials)`: from
/// `ORG_GRADLE_PROJECT_<name>`, then from `gradle.properties` in the Gradle user home.
fn gradle_property(name: &str) -> Option<String> {
    if let Ok(value) = env::var(format!("ORG_GRADLE_PROJECT_{name}")) {
        return Some(value);
    }
    let home = match env::var_os("GRADLE_USER_HOME") {
        Some(home) => PathBuf::from(home),
        None => env::home_dir()?.join(".gradle"),
    };
    // ponytail: no escapes or line continuations, add them if a password needs a backslash
    fs::read_to_string(home.join("gradle.properties"))
        .ok()?
        .lines()
        .rev()
        .find_map(|line| {
            let (key, value) = line.trim_start().split_once(['=', ':'])?;
            (key.trim_end() == name).then(|| value.trim_start().to_string())
        })
}

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
        .get()
        .into_iter()
        .flatten()
        .find_map(|repository| {
            let mut request = AGENT.get(format!("{}/{path}", repository.url));
            if let Some(authorization) = &repository.authorization {
                request = request.header("Authorization", authorization);
            }
            request.call().ok()?.body_mut().read_to_string().ok()
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
