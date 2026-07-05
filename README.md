# fl

All-in-one bootstrap and dev-loop tooling for the [Freelancer](https://en.wikipedia.org/wiki/Freelancer_(video_game))
decompilation. `fl` fetches the toolchain and original binaries, splits the
libraries, generates the build files, and drives the compile/diff/match loop —
so a fresh checkout of a decomp project goes from clone to buildable with a
single command.

> The crate is named `flboot`; the shipped command is `fl`.

## Install

Grab a prebuilt binary from the [Releases](../../releases) page:

| Platform      | Asset                                |
| ------------- | ------------------------------------ |
| Windows x64   | `fl-x86_64-pc-windows-msvc.zip`      |
| Linux x64     | `fl-x86_64-unknown-linux-gnu.tar.gz` |

Unpack it and put `fl` (or `fl.exe`) on your `PATH`.

### From source

Requires a recent stable Rust toolchain (edition 2024).

```sh
cargo install --path .
# or, for a local build:
cargo build --release   # -> target/release/fl
```

## Platform notes

The decompilation targets MSVC 6.0, which is Windows-only. On Windows `fl`
invokes the compiler directly. On Linux it runs MSVC 6.0 under
[Wine](https://www.winehq.org/), so `wine` must be on your `PATH` for the
`build` command (`bootstrap` and the diff/report commands do not need it).

## Usage

`fl` is run from inside a decomp repository — it locates the repo root by
walking up to the enclosing `.git` directory, then reads its configuration from
`config/<config-id>/`.

```sh
# Fetch tools + original binaries, split libraries, generate build files:
fl bootstrap

# Compile one or more units with the exact flags ninja would use:
fl build x86math

# Regenerate a unit's target object from its delink/split config:
fl delink x86math

# Claim (rename) target symbols and re-delink:
fl claim x86math sub_6F71DE0=inv_sqrt

# Show original -> claimed symbol mappings reconstructed from git history:
fl claims

# Target-vs-ours disassembly diff for one function:
fl diff x86math inv_sqrt

# Target-only disassembly listing:
fl dis x86math inv_sqrt

# Match-percentage report:
fl progress
```

Run `fl --help` or `fl <command> --help` for the full option set.

### Configuration

By default `fl` uses the config id `052103_release_1149_Ipatch_ver1254`. Pass
`--config-id <id>` to select another, or `--config <path>` to point at a
specific `config.json`. A decomp repo is expected to provide, per config id:

```
config/tools.json                     # tool download manifest (shared)
config/<config-id>/config.json        # compiler, flags, progress categories
config/<config-id>/objects.json       # libraries -> source units
config/<config-id>/orig.json          # original binary hashes + archive URL
config/<config-id>/delink/*.json      # per-unit delink exports
config/<config-id>/splits/*.json      # per-unit split configs
```

## Development

```sh
cargo test                                      # run the test suite
cargo clippy --all-targets -- -D warnings       # lint (CI gate)
```

CI runs clippy and the test suite on Windows and Linux for every pull request.
Releases are cut automatically by [release-plz](https://release-plz.dev/): a
"release" PR keeps the version and `CHANGELOG.md` up to date from
[conventional commits](https://www.conventionalcommits.org/); merging it tags
the commit and publishes a GitHub Release with the `fl` binaries attached.

## License

Licensed under the [MIT License](LICENSE).
