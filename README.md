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
- To apply every update of the catalog at once, pick **Update all N versions** in the same menu.

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

## Private repositories

To get hints for modules published on your company's Nexus or Artifactory, on GitHub Packages or on
JitPack, list their repositories in your Zed settings. They are queried in this order, before Maven
Central, Google Maven and the Gradle Plugin Portal:

```json
"lsp": {
  "gradle-catalog-lsp": {
    "initialization_options": {
      "repositories": [
        { "url": "https://nexus.acme.com/repository/maven-public", "credentials": "acmeNexus" },
        { "url": "https://jitpack.io" }
      ]
    }
  }
}
```

`credentials` names a pair of Gradle properties, read like Gradle reads
`credentials(PasswordCredentials::class)`: here `acmeNexusUsername` and `acmeNexusPassword`, from
the environment variables `ORG_GRADLE_PROJECT_acmeNexusUsername` and
`ORG_GRADLE_PROJECT_acmeNexusPassword`, or else from `~/.gradle/gradle.properties` (in
`GRADLE_USER_HOME` if you set it). A token goes in the password, as for Gradle. This keeps
credentials out of the Zed settings, and they are only sent over HTTPS (or to localhost). A
repository whose credentials can't be found is skipped, and an entry with credentials in its URL
is ignored.

Settings and credentials are read when the language server starts: run
`editor: restart language server` after changing them. Once you trust a project in Zed, which
leaves [Restricted Mode](https://zed.dev/docs/worktree-trust), its `.zed/settings.json` can add
repositories too, and your credentials are then sent to the URL it names: check its settings before
trusting a project you didn't write.

## Which version is suggested

- Versions are compared with [Gradle's ordering rules](https://docs.gradle.org/current/userguide/dependency_versions.html#sec:version-ordering).
- A version stays on its track: a stable version is never offered an alpha, beta or release
  candidate, and a flavored version keeps its flavor (Guava `33.3.1-android` gets `33.4.0-android`,
  not `-jre`). A pre-release is offered any newer version.
- A `version.ref` shared by several modules (say `kotlin` for the plugins and the standard library)
  is only bumped when all of them have published the new version, and only as far as the slowest one.

## Limits

- Rich versions (`strictly`, `require`, `prefer`, `reject`) and dynamic ones (`1.+`, ranges,
  `latest.release`) are skipped.
- Coordinates written directly in `build.gradle(.kts)` are not read, only catalogs.
- Private repositories need Basic authentication, a username with a password or token: Bearer
  tokens and custom headers are not supported.
- A new release can take up to an hour to show, and a module that could not be found, offline for
  instance, is looked up again after 5 minutes.

## Contributing

Bug reports and pull requests are welcome: [CONTRIBUTING.md](CONTRIBUTING.md) explains how to build,
test and release the extension.

## License

Licensed under the [Apache License 2.0](LICENSE).

The language server binaries also contain third-party software: its licenses are in
`THIRD-PARTY-LICENSES.md` and `COPYRIGHT-library.html`, attached to each release.
