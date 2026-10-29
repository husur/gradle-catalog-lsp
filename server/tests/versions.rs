use gradle_catalog_lsp::versions::{compare, latest};

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
        assert!(
            compare(pair[1], pair[0]).is_gt(),
            "{} > {}",
            pair[1],
            pair[0]
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
