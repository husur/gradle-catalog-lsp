# Gradle Version Catalog for Zed

Shows newer versions of the libraries and plugins declared in Gradle version catalogs
(`gradle/libs.versions.toml`), right next to the version you use.

```toml
[versions]
agp = "9.4.1"        # → 9.5.0
kotlin = "2.4.20"    # → 2.4.21
lifecycle = "2.10.0" # → 2.11.0
```

Versions are looked up on Maven Central, Google Maven (androidx, Android Gradle Plugin, Firebase…)
and the Gradle Plugin Portal, so it covers Android, Kotlin Multiplatform and JVM projects built with
Gradle. For `pom.xml` and other ecosystems, see [Depsy](https://github.com/mpiton/zed-depsy).

## Applying an update

- **Double-click the hint** (Zed 1.25.2 or later), or put the cursor on it and run `editor: accept inlay hint`.
- Or open the code actions on the line and pick **Update to x.y.z**.

## Installation

Run `zed: extensions`, search for **Gradle Version Catalog** and install it. The language server is
downloaded automatically.

Then make sure that:

- the **TOML** extension is installed, since Zed only starts the server on files it knows as TOML;
- inlay hints are enabled, as Zed turns them off by default. Run `editor: toggle inlay hints`, or add
  this to your settings:

  ```json
  "inlay_hints": { "enabled": true }
  ```

## Which version is suggested

- Versions are compared with [Gradle's ordering rules](https://docs.gradle.org/current/userguide/dependency_versions.html#sec:version-ordering).
- A version stays on its track: a stable version is never offered an alpha, beta or release
  candidate, and a flavored version keeps its flavor (Guava `33.3.1-android` gets `33.4.0-android`,
  not `-jre`). A pre-release is offered any newer version.
- A `version.ref` shared by several modules (say `kotlin` for the plugins and the standard library)
  is only bumped when all of them have published the new version, and only as far as the slowest one.

## Limits

- Private repositories (Nexus, Artifactory, JitPack…) are not queried: their modules get no hint.
- Rich versions (`strictly`, `require`, `prefer`, `reject`) and dynamic ones (`1.+`, ranges,
  `latest.release`) are skipped.
- Coordinates written directly in `build.gradle(.kts)` are not read, only catalogs.
- Versions are fetched once per session. To pick up a release published since, or after a network
  error, run `editor: restart language server`.

## Contributing

Bug reports and pull requests are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) explains how to build,
test and release the extension.

## License

Licensed under the [Apache License 2.0](LICENSE).
