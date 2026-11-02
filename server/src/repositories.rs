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

const MAVEN_CENTRAL: &str = "https://repo1.maven.org/maven2";
const GOOGLE_MAVEN: &str = "https://dl.google.com/dl/android/maven2";
const PLUGIN_PORTAL: &str = "https://plugins.gradle.org/m2";
const DEFAULT_REPOSITORIES: [&str; 3] = [MAVEN_CENTRAL, GOOGLE_MAVEN, PLUGIN_PORTAL];

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

/// The repositories from the settings, then the default ones.
static REPOSITORIES: OnceLock<(Vec<Repository>, Vec<Repository>)> = OnceLock::new();

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
    let _ = REPOSITORIES.set((configured.collect(), defaults.collect()));
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

pub(crate) static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into()
});

/// The URL of the first repository that knows the module, with the versions it lists.
pub fn fetch(module: &Module) -> Option<(&'static str, Vec<String>)> {
    let (configured, defaults) = REPOSITORIES.get()?;
    let path = format!(
        "{}/{}/maven-metadata.xml",
        module.group.replace('.', "/"),
        module.name
    );
    // Like Gradle, which resolves plugins on the Plugin Portal first. It hosts the plugins that
    // have a page there, and redirects to Maven Central for the others.
    let (portal, others): (Vec<_>, Vec<_>) = defaults
        .iter()
        .partition(|repository| module.plugin().is_some() && repository.url == PLUGIN_PORTAL);
    let configured = configured.iter().map(|repository| (repository, true));
    let defaults = portal
        .into_iter()
        .chain(others)
        .map(|repository| (repository, false));
    let found = configured
        .chain(defaults)
        .find_map(|(repository, follow_redirects)| {
            let url = format!("{}/{path}", repository.url);
            let mut request = AGENT.get(&url);
            if let Some(authorization) = &repository.authorization {
                request = request.header("Authorization", authorization);
            }
            // A default repository that redirects does not host the module, which the next ones
            // answer. A repository from the settings can redirect to the storage of its files.
            if !follow_redirects {
                request = request.config().max_redirects(0).build();
            }
            request
                .call()
                .and_then(|mut response| {
                    let metadata = response.body_mut().read_to_string()?;
                    Ok((!response.status().is_redirection()).then_some(metadata))
                })
                // Zed shows stderr in the language server logs. A 404 only means that the module
                // lives in another repository.
                .inspect_err(|error| {
                    if !matches!(error, ureq::Error::StatusCode(404)) {
                        eprintln!("{url}: {error}");
                    }
                })
                .ok()
                .flatten()
                .map(|metadata| (repository.url.as_str(), metadata))
        });
    let (repository, metadata) = found?;
    let versions = metadata
        .split("<version>")
        .skip(1)
        .filter_map(|tail| Some(tail.split_once("</version>")?.0.trim().to_string()))
        .collect();
    Some((repository, versions))
}

/// Whether the repository is a default, public one rather than one from the settings.
pub fn is_default(repository: &str) -> bool {
    REPOSITORIES
        .get()
        .is_some_and(|(_, defaults)| defaults.iter().any(|default| default.url == repository))
}

/// The page of a version of the module on the website of the default repository it was found in.
/// A repository from the settings gets none: its URL can be internal, and there is no standard
/// page layout. Neither does a library found on the Plugin Portal, which only lists its files.
pub fn page(repository: &str, module: &Module, version: &str) -> Option<String> {
    let Module { group, name } = module;
    match (repository, module.plugin()) {
        (MAVEN_CENTRAL, _) => Some(format!(
            "https://central.sonatype.com/artifact/{group}/{name}/{version}"
        )),
        (GOOGLE_MAVEN, _) => Some(format!(
            "https://maven.google.com/web/index.html#{group}:{name}:{version}"
        )),
        (PLUGIN_PORTAL, Some(id)) => {
            Some(format!("https://plugins.gradle.org/plugin/{id}/{version}"))
        }
        _ => None,
    }
}
