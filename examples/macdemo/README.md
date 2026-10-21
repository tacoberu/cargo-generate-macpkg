# macdemo

Minimal project for `cargo generate-macpkg`. From this directory:

```
rustup target add aarch64-apple-darwin
cargo zigbuild --release --target aarch64-apple-darwin       # or `cargo build` on a Mac
cargo generate-macpkg --target aarch64-apple-darwin
```

The package `target/generate-macpkg/macdemo-0.1.0-arm64.pkg` installs `/usr/local/bin/macdemo`
and runs `macpkg/scripts/postinstall` afterwards.
