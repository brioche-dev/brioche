# Changelog

All notable changes to Brioche will be documented in this file.

Note that the individual Rust crates within this repo are not considered stable and changes will not be documented in the changelog.

## [Unreleased]

### Added

- Implement support for `Brioche.download("...")`. The provided URL will be downloaded, and the hash will automatically be recorded in the `brioche.lock` lockfile ([#102](https://github.com/brioche-dev/brioche/pull/102))
    - **NOTE**: This requires an updated version of the `std` package, see [brioche-dev/brioche-packages#75](https://github.com/brioche-dev/brioche-packages/pull/75)

### Changed

- Update most subcommands to take the `--project` (`-p`) or `--registry` (`-r`) argument more than once. The command will apply to each project. This applies to the following subcommands:
    - `brioche fmt` ([#84](https://github.com/brioche-dev/brioche/pull/84) by [@jaudiger](https://github.com/jaudiger))
    - `brioche check` ([#91](https://github.com/brioche-dev/brioche/pull/91) by [@jaudiger](https://github.com/jaudiger))
    - `brioche publish` ([#106](https://github.com/brioche-dev/brioche/pull/106) by [@jaudiger](https://github.com/jaudiger))
    - `brioche install` ([#107](https://github.com/brioche-dev/brioche/pull/107) by [@jaudiger](https://github.com/jaudiger))
- Update `brioche fmt` to print formatted files ([#75](https://github.com/brioche-dev/brioche/issues/75) by [@jaudiger](https://github.com/jaudiger))

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
