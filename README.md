# unison-fsmonitor

[![CI](https://github.com/autozimu/unison-fsmonitor/actions/workflows/ci.yml/badge.svg)](https://github.com/autozimu/unison-fsmonitor/actions/workflows/ci.yml)

## Why

`unison` doesn't include `unison-fsmonitor` for macOS, thus `-repeat watch` option doesn't work out of the box. This utility fills the gap. This implementation was originally made for macOS but shall work on other platforms as well like Linux, Windows.

## Install

```sh
brew install autozimu/homebrew-formulas/unison-fsmonitor
```

Alternatively if you have [cargo](https://github.com/rust-lang/cargo) installed,

```sh
cargo install unison-fsmonitor
```

## Usage

Simply run unison with `-repeat watch` as argument or `repeat=watch` in config file.

## File watch limits 

You might need to update file watch limits in both hosts if watching limit reached. See <https://facebook.github.io/watchman/docs/install#system-specific-preparation> for more details.

## Debug

```
RUST_LOG=debug unison
```

## Releases

Releases are managed with `release-please` and built with GoReleaser. Use conventional commits such as `feat:`, `fix:`, and `feat!:` so changelog entries and version bumps are generated correctly.

## References

- Protocol <https://github.com/bcpierce00/unison/blob/af8669bb26f88e85bdc37cb1ff23d9bb0685a1e2/src/fswatch.ml>
- <https://github.com/bcpierce00/unison/blob/master/src/fsmonitor/watchercommon.ml>
- <https://github.com/hnsl/unox>
