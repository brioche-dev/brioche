# Changelog

All notable changes to Brioche will be documented in this file.

Note that the individual Rust crates within this repo are not considered stable and changes will not be documented in the changelog.

## [Unreleased]

### Changed

- Update `brioche fmt` command to take the `-p` option more than once, allowing for formatting multiple projects at a time ([#84](https://github.com/brioche-dev/brioche/pull/84) by [@jaudiger](https://github.com/jaudiger))

### Removed

- Some executables that were originally included as part of this repo have been removed from this repo and instead moved into [`brioche-runtime-utils`](https://github.com/brioche-dev/brioche-runtime-utils) instead ([#62](https://github.com/brioche-dev/brioche/pull/62)). The moved executables include:
    - `brioche-ld`
    - `brioche-packed-plain-exec`
    - `brioche-packed-userland-exec`
    - `brioche-packer`
- Several utility functions that were previously exported from `brioche-pack` have instead been moved into the new crates `brioche-resources` and `brioche-autowrap` in the [`brioche-runtime-utils`](https://github.com/brioche-dev/brioche-runtime-utils) repo ([#63](https://github.com/brioche-dev/brioche/pull/63))

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
