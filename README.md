# Brioche

Brioche is a package manager and build tool for building and running your most complex software projects.

```ts
import * as std from "std";
import { cargoBuild } from "rust";

export default () => {
  // Build a Rust project
  const server = cargoBuild({
    crate: Brioche.glob("src", "Cargo.*"),
  });

  // Put it in a container image
  return std.ociContainerImage({
    recipe: server,
    entrypoint: ["/bin/server"],
  });
}
```

## Features

- **Caching** - All build artifacts are saved and re-used between builds, saving you time when only a part of your project changes. You can even share the cache between machines using an S3-compatible storage provider
- **Lockfiles** - All of your dependencies are automatically saved in a lockfile, making your builds super reliable for everyone on your team
- **TypeScript** - Build scripts are written in TypeScript, giving you the flexibility of a familiar full programming language (with great type checking and editor completions!)
- **Cross-ecosystem** - Build your project regardless of language, and easily mix different languages and ecosystems in one project
- **Cross-compilation** (work in progress) - Easily build your project from any platform, to any platform
- **Dev environments** (work in progress) - Set up a suite of tools to help onboard new team members quickly

## Installation

Run this in your terminal for quick installation:

```sh
curl --proto '=https' --tlsv1.2 -sSfL 'https://brioche.dev/install.sh' | sh
```

...or check out the official docs on [Installation](https://brioche.dev/docs/installation) for more installation options.

To install from source, simply check out this repo and run `cargo install --locked --path crates/brioche`. This will install Brioche into `~/.cargo/bin`. You can also run it as a normal Rust project using `cargo run`.

## Packages

See the [brioche-packages](https://github.com/brioche-dev/brioche-packages) repo for Brioche packages
