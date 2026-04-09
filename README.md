# unison-fsmonitor

[![CI](https://github.com/aaronflorey/unison-fsmonitor/actions/workflows/ci.yml/badge.svg)](https://github.com/aaronflorey/unison-fsmonitor/actions/workflows/ci.yml)

## Why

`unison` does not ship `unison-fsmonitor` for macOS, so `-repeat watch` does not work out of the box. This utility fills that gap.

The project started on macOS, but release binaries are now published for macOS and Linux.

## Attribution

This repository continues the original `autozimu/unison-fsmonitor` work by Junfeng Li.

## Install

Recommended: install the published release with [`bin`](https://github.com/aaronflorey/bin).

```sh
bin install github.com/aaronflorey/unison-fsmonitor
```

If you already use [cargo](https://github.com/rust-lang/cargo), you can also install from source:

```sh
cargo install unison-fsmonitor
```

## Usage

Run `unison` with `-repeat watch`, or set `repeat = watch` in your config file.

## File Watch Limits

If you hit the watch limit, increase the file watch limits on both hosts. See <https://facebook.github.io/watchman/docs/install#system-specific-preparation> for platform-specific guidance.

## Debug

```sh
RUST_LOG=debug unison
```

## Releases

Releases are managed with `release-please` and published with GoReleaser.

Use conventional commits such as `feat:`, `fix:`, and `feat!:` so changelog entries and version bumps are generated correctly.

## References

- Protocol <https://github.com/bcpierce00/unison/blob/af8669bb26f88e85bdc37cb1ff23d9bb0685a1e2/src/fswatch.ml>
- <https://github.com/bcpierce00/unison/blob/master/src/fsmonitor/watchercommon.ml>
- <https://github.com/hnsl/unox>
