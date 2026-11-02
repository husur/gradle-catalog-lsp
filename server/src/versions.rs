use std::cmp::Ordering;

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

pub fn latest<'a>(current: &str, available: &'a [String]) -> Option<&'a str> {
    let flavor = qualifiers(current);
    let prerelease = is_prerelease(current);
    newest(current, available, |candidate| {
        prerelease || qualifiers(candidate) == flavor
    })
}

/// The newest pre-release of the flavor of `current` (Guava `-android` or `-jre`) newer than it.
pub fn prerelease<'a>(current: &str, available: &'a [String]) -> Option<&'a str> {
    let (_, flavor): (Vec<_>, Vec<_>) = qualifiers(current)
        .into_iter()
        .partition(|q| prerelease_qualifier(q));
    newest(current, available, |candidate| {
        let (prerelease, other): (Vec<_>, Vec<_>) = qualifiers(candidate)
            .into_iter()
            .partition(|q| prerelease_qualifier(q));
        !prerelease.is_empty() && other == flavor
    })
}

fn newest<'a>(
    current: &str,
    available: &'a [String],
    eligible: impl Fn(&str) -> bool,
) -> Option<&'a str> {
    available
        .iter()
        .map(String::as_str)
        .filter(|candidate| eligible(candidate))
        .max_by(|a, b| compare(a, b))
        .filter(|newest| compare(newest, current).is_gt())
}

pub fn is_prerelease(version: &str) -> bool {
    qualifiers(version).iter().any(|q| prerelease_qualifier(q))
}

fn prerelease_qualifier(qualifier: &str) -> bool {
    PRERELEASES.contains(&qualifier)
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
    let chars: Vec<char> = version.to_lowercase().chars().collect();
    chars
        .split(|c| matches!(c, '.' | '-' | '_' | '+'))
        .flat_map(|piece| piece.chunk_by(|a, b| a.is_ascii_digit() == b.is_ascii_digit()))
        .map(|token| token.iter().collect())
        .collect()
}
