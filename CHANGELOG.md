# Changelog

All notable changes to Brioche will be documented in this file.

Note that the individual Rust crates within this repo are not considered stable and changes will not be documented in the changelog.

## [Unreleased]

### Standard library

> **NOTE**: These features require both the latest version of Brioche and an appropriate version of the `std` package. Consult the [std changelog] for more details

- Add `Brioche.download("...")`. The provided URL will be downloaded, and the hash will automatically be recorded in the `brioche.lock` lockfile ([#102](https://github.com/brioche-dev/brioche/pull/102))
- Add `Brioche.gitRef({ repository, ref })`. The git repository URL will be fetched, and the commit for the corresponding ref (branch or tag name) will be recorded in the `brioche.lock` lockfile. This is useful when used with the `gitCheckout` function from the `git` package ([#126](https://github.com/brioche-dev/brioche/pull/126))
- Add `std.glob(...)` recipe. This takes a directory and some glob patterns, and returns a new directory containing only the contents matching a pattern. This is similar to `Brioche.glob(...)`, but works with existing recipes instead of inputs from the project ([#119](https://github.com/brioche-dev/brioche/pull/119))

### Fixed

- Fix `brioche self-update` command. Unfortunately, upgrading from v0.1.1 will still need to be done manually, but auto-updates should work going forward! ([#112](https://github.com/brioche-dev/brioche/pull/112) by [@jaudiger](https://github.com/jaudiger))
- Fix `.unarchive()` recipes sometimes not properly baking when unarchiving a tarfile containing long filename entries ([#127](https://github.com/brioche-dev/brioche/pull/127), with work from [#117](https://github.com/brioche-dev/brioche/pull/117) by [@jaudiger](https://github.com/jaudiger)

### Changed

- Update most subcommands to take the `--project` (`-p`) or `--registry` (`-r`) argument more than once. The command will apply to each project. This applies to the following subcommands:
    - `brioche fmt` ([#84](https://github.com/brioche-dev/brioche/pull/84) by [@jaudiger](https://github.com/jaudiger))
    - `brioche check` ([#91](https://github.com/brioche-dev/brioche/pull/91) by [@jaudiger](https://github.com/jaudiger))
    - `brioche publish` ([#106](https://github.com/brioche-dev/brioche/pull/106) by [@jaudiger](https://github.com/jaudiger))
    - `brioche install` ([#107](https://github.com/brioche-dev/brioche/pull/107) by [@jaudiger](https://github.com/jaudiger))
- Update `brioche fmt` to print formatted files ([#75](https://github.com/brioche-dev/brioche/issues/75) by [@jaudiger](https://github.com/jaudiger))
- Update `Process.dependencies` recipes to support more flexible env var configurations. Namely, dependencies can now set env vars for fallbacks, in addition to the existing env vars that get appended

### Internals

- Upgrade OpenTelemetry SDK packages. Brioche now uses the conventional OpenTelemetry SDK environment variables for configuration
- Upgrade Deno Core from v0.201.0 to v0.303.0
- Update Rust toolchain to v1.81
- Restructure some internal crates. Some crates that were originally in the main Brioche repository now live in [`brioche-dev/brioche-runtime-utils`](https://github.com/brioche-dev/brioche-runtime-utils)

## [v0.1.1] - 2024-06-09

### Added

- **Add new TypeScript runtime op to get the version of Brioche** ([#59](https://github.com/brioche-dev/brioche/pull/59)). This will allow for updating packages to take advantage of new features without breaking backwards compatibility
- **Add new `collect_references` recipe type** ([#57](https://github.com/brioche-dev/brioche/pull/57)). This will be used by the `std` package to improve container sizes

### Changed

- **Increase timeouts when fetching from registry from 10s to 120s** ([#54](https://github.com/brioche-dev/brioche/pull/54) by [@matklad](https://github.com/matklad)). This is a workaround due to very slow cold start times seen in some cases when making requests to the registry
- **Tweak registry sync rules to avoid downloading unnecessary dependencies** ([#56](https://github.com/brioche-dev/brioche/pull/56)). This should drastically reduce the download times during the first-time user experience (especially with some changes to the registry itself)
- **Download project files in parallel from registry** ([#58](https://github.com/brioche-dev/brioche/pull/58))

## [v0.1.0] - 2024-06-02

### Added

- **Initial release!**

[Unreleased]: https://github.com/brioche-dev/brioche/compare/v0.1.1...HEAD
[v0.1.1]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.1
[v0.1.0]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.0

[std changelog]: https://github.com/brioche-dev/brioche-packages/blob/main/packages/std/CHANGELOG.md
