use std::cmp::Ordering;
use std::sync::LazyLock;
use std::time::Duration;

use crate::catalog::Module;

const REPOSITORIES: [&str; 3] = [
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

#[cfg(test)]
mod tests {
    use super::*;

    fn latest_of(current: &str, available: &[&str]) -> Option<String> {
        let available: Vec<String> = available.iter().map(|v| v.to_string()).collect();
        latest(current, &available).map(str::to_string)
    }

    #[test]
    fn orders_like_gradle() {
        let ascending = [
            "1.0-dev",
            "1.0-alpha01",
            "1.0-alpha02",
            "1.0-beta01",
            "1.0-rc01",
            "1.0",
            "1.0.1",
            "1.1",
            "1.10",
            "2.0.21-1.0.27",
            "2.1.0-1.0.29",
        ];
        for pair in ascending.windows(2) {
            assert!(
                compare(pair[0], pair[1]).is_lt(),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert!(compare("1.0-SNAPSHOT", "1.0-RC1").is_gt());
    }

    #[test]
    fn suggests_the_newest_version_of_the_same_kind() {
        let lifecycle = ["2.10.0", "2.11.0-rc01", "2.11.0", "2.12.0-alpha05"];
        assert_eq!(latest_of("2.10.0", &lifecycle).as_deref(), Some("2.11.0"));
        assert_eq!(latest_of("2.11.0", &lifecycle), None);
        assert_eq!(
            latest_of("2.12.0-alpha01", &lifecycle).as_deref(),
            Some("2.12.0-alpha05")
        );
        let guava = [
            "33.3.1-jre",
            "33.3.1-android",
            "33.4.0-android",
            "33.4.0-jre",
        ];
        assert_eq!(
            latest_of("33.3.1-android", &guava).as_deref(),
            Some("33.4.0-android")
        );
        let backport = ["1.9.0", "2.0.0", "1.9.5"];
        assert_eq!(latest_of("1.9.0", &backport).as_deref(), Some("2.0.0"));
    }
}
