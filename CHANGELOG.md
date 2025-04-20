# Changelog

All notable changes to Brioche will be documented in this file.

Note that the individual Rust crates within this repo are not considered stable and changes will not be documented in the changelog.

## [Unreleased]

## [v0.1.5]

**Check the blog post ["Announcing Brioche v0.1.5"](http://brioche.dev/blog/announcing-brioche-v0-1-5) for an overview of the new features in this release**

### Breaking

> **Note**: This is a minor release, but Brioche is still pre-1.0 and not widely adopted, so we may ship minor breaking changes from time to time if we think the breakage will not affect many people

- (**Minor breaking**) The new caching system is replacing (most) uses of the Brioche registry. If you were syncing build artifacts to a custom registry before, you will need to migrate to a custom cache instead.
    - The registry is still used for resolving projects. For larger teams, it might still make sense to host a custom registry.
    - This release includes some minimal tooling to help migrate existing cached data from the old registry to a new cache, but it will be removed before the next release.
    - If you are self-hosting your own infrastructure for Brioche today (or are interested in doing so), please reach out for more information

### Added

- Implement new caching system ([#179](https://github.com/brioche-dev/brioche/pull/179))
    - Fetching from the official Brioche cache should be much faster in most cases
    - Using a private / self-hosted cache is much easier now. The `cache.url` config key or the `$BRIOCHE_CACHE_URL` env var can be used to cache with several different object storage backends. Check the [documentation](https://brioche.dev/docs/configuration/) for more details
- Add support for unarchiving `.zip` archives ([#176](https://github.com/brioche-dev/brioche/pull/176) by [**@paricbat**](https://github.com/paricbat))
- Add support for projects with cyclic imports ([#211](https://github.com/brioche-dev/brioche/pull/211))
- Add command to start a debug shell within a failed build ([#215](https://github.com/brioche-dev/brioche/pull/215))
- Add support for `Brioche.gitCheckout()` as a static ([#218](https://github.com/brioche-dev/brioche/pull/218))
- Add `unsandboxed` sandbox backend ([#230](https://github.com/brioche-dev/brioche/pull/230))
- Add `process.currentDir` option to change which directory a process starts in ([#231](https://github.com/brioche-dev/brioche/pull/231))

### Changed

- Use `$BRIOCHE_DATA_DIR` env var to control where Brioche's data is stored-- in most cases, the default path is `~/.local/share/brioche` on Linux ([#171](https://github.com/brioche-dev/brioche/pull/171))
- Allow processes to inherit CA certificates from the host when the unsafe `networking` option is enabled ([#232](https://github.com/brioche-dev/brioche/pull/232))
    - In practice, this means that builds that access the network will no longer need to pull in the `ca_certificates` package

### Fixed

- Fix various issues with the LSP ([#188](https://github.com/brioche-dev/brioche/pull/188))
- Tweak LSP to keep unused dependencies when saving ([#192](https://github.com/brioche-dev/brioche/pull/192))
- Fix `brioche fmt` when called without any args: it now defaults to formatting the project in the current directory ([#190](https://github.com/brioche-dev/brioche/pull/190) by [**@asheliahut**](https://github.com/asheliahut))
- Add missing implementations for several expressions when evaluating statics ([#195](https://github.com/brioche-dev/brioche/pull/195))
- Fix potential segfault from V8 depending on CPU flags ([#225](https://github.com/brioche-dev/brioche/pull/225))

## [v0.1.4] - 2025-01-18

**Check the blog post ["Announcing Brioche v0.1.4"](http://brioche.dev/blog/announcing-brioche-v0-1-4) for an overview of all the new features in this release**

### Changed

- Overhaul console output ([#137](https://github.com/brioche-dev/brioche/pull/137))
    - The new output format uses colors and symbols, runs at a faster refresh rate, and generally should make it clearer what's going on. See [this Asciinema recording](https://asciinema.org/a/PMZ9kKSD6VAmz3K5A3FLWZbp5) for what the new format looks like.
- Overhaul output format when a process recipe fails ([#138](https://github.com/brioche-dev/brioche/pull/138), [#139](https://github.com/brioche-dev/brioche/pull/139))

### Added

- Add `--locked` flag for several subcommands ([#133](https://github.com/brioche-dev/brioche/pull/133))
    - Applies to `build`, `check`, `run`, and `install`. When passed, the command will fail if the lockfile isn't up-to-date.
- Add `--display` flag, plus new `plain-reduced` output format ([#141](https://github.com/brioche-dev/brioche/pull/141))
- Add new `attach_resources` recipe type ([#149](https://github.com/brioche-dev/brioche/pull/149))

### Fixed

- Update Linux sandbox to fallback to using PRoot for mounts ([#159](https://github.com/brioche-dev/brioche/pull/159))
    - This fallback makes it so Brioche can run without any extra setup on Ubuntu 24.04-- although with reduced performance. See ["PRoot fallback"](https://brioche.dev/help/proot-fallback) in the docs for more context and other options.
- Fix some LSP errors from converting between line / column numbers and positions ([#134](https://github.com/brioche-dev/brioche/pull/134))

### Internals

- Add `project.bri` for building Brioche with Brioche! This new build will be used to provide portable builds for non-glibc machines.
- This release includes the initial groundwork for AArch64 support on Linux (a.k.a ARM64). Brioche itself can now run on `aarch64-linux`, but this work hasn't landed in the `brioche-packages` repo yet, and getting it merged so packages can work with both `aarch64-linux` and `x86_64-linux` is still blocked on future feature work.

## [v0.1.3] - 2024-09-27

### Fixed

- Fix Brioche LSP erasing locked download and git ref hashes ([#130](https://github.com/brioche-dev/brioche/pull/130))
- Fix "failed to move temporary project from registry" error. This occurred due to a race condition when a project tried to be fetched more than once from the registry, e.g. from a dependency shared by multiple other dependencies, like `std` ([#131](https://github.com/brioche-dev/brioche/pull/131))
- Fix (very annoying!) "Request textDocument/diagnostic failed" error from LSP whenever a `.bri` file was first opened ([#132](https://github.com/brioche-dev/brioche/pull/132))

## [v0.1.2] - 2024-09-26

### Standard library

> **NOTE**: These features require both the latest version of Brioche and an appropriate version of the `std` package. Consult the [std changelog](https://github.com/brioche-dev/brioche-packages/blob/main/packages/std/CHANGELOG.md) for more details

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

[Unreleased]: https://github.com/brioche-dev/brioche/compare/v0.1.5...HEAD
[v0.1.5]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.5
[v0.1.4]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.4
[v0.1.3]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.3
[v0.1.2]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.2
[v0.1.1]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.1
[v0.1.0]: https://github.com/brioche-dev/brioche/releases/tag/v0.1.0
