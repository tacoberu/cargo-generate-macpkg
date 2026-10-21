cargo-generate-macpkg
=====================

[**English**](README.md) | [Česky](README.cs.md)

A Cargo helper command that generates a macOS installer package (`.pkg`) from a Cargo project.
It needs no Xcode: the payload, the installer scripts and `PackageInfo` are written by the tool
itself, and on Linux the bill of materials and the archive are made by `mkbom` and `xar`.
Inspired by [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm).

## Installation

```
cargo install --path .
```

## Usage

Run in the Cargo project you want to package, after building the binaries:

```
cargo build --release --target aarch64-apple-darwin          # on a Mac
cargo generate-macpkg --target aarch64-apple-darwin
```

The result is `target/generate-macpkg/<name>-<version>-<arch>.pkg`, where `<arch>` is `arm64`,
`x86_64`, … (from `--target`; `any` without it). Use `-o` to change the output (a file or a
directory). `--target` and `--profile` must match how the binaries were built; the command
neither builds the binaries nor signs the package. In a workspace run it in the package
directory or select the package with `-p NAME_OR_PATH`.

How the package is put together (`--tool`):

| Tool | Where | Needs |
|---|---|---|
| `pkgbuild` | macOS (the default there) | `pkgbuild` from the Command Line Tools |
| `xar` | Linux (the default without `pkgbuild`) | `mkbom` from [bomutils](https://github.com/hogliux/bomutils) and `xar` (Fedora: `dnf install xar`) |

Paths of the programs can be given with `--pkgbuild`, `--mkbom` and `--xar`.

Cross-compiling the binaries from Linux works with
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) (`cargo install cargo-zigbuild`,
needs `zig`). The package is **not signed or notarized**; `productsign` and `notarytool` do that on a Mac.

## Configuration

Settings live in the `[package.metadata.generate-macpkg]` table of `Cargo.toml`.

| Option | Mandatory | Default |
|---|---|---|
| `identifier` | **yes** | reverse-DNS identifier, e.g. `org.example.tool` |
| `name` | no | `package.name` (file name of the package) |
| `version` | no | `package.version` |
| `install_location` | no | `/` |
| `assets` | **yes** | files put into the payload, see below |
| `scripts` | no | directory with the installer scripts `preinstall`, `postinstall` (all files are made executable) |

`assets` is an array of items with `source`, `dest` and `mode` (octal string, default `"0644"`).
`dest` is an absolute path on the target volume (`/usr/local/bin/tool`); if it ends with `/`, the
file name of `source` is appended. If `source` contains `*`, `dest` must end with `/`. The
`target/release/` prefix in `source` is replaced by the real build directory according to
`--target` and `--profile`. Parent directories are created (`root:wheel`, `0755`); files are owned by `root:wheel`.

```toml
[package.metadata.generate-macpkg]
identifier = "org.example.macdemo"
scripts = "macpkg/scripts"
assets = [
    { source = "target/release/macdemo", dest = "/usr/local/bin/macdemo", mode = "0755" },
    { source = "README.md", dest = "/usr/local/share/doc/macdemo/" },
]
```

A working example is in [`examples/macdemo`](examples/macdemo).

## Status

The payload (`cpio` archive), the scripts archive, `PackageInfo` and the calls of `pkgbuild`, `mkbom`
and `xar` are covered by tests (the programs themselves by stand-ins). A package put together
by real `xar` and `mkbom`, and its installation on a Mac, have **not been verified**.

## License

MIT, see [LICENSE](LICENSE).
