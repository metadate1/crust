# Development and verification

## Tool versions

- Rust `1.97.0` (pinned by `rust-toolchain.toml`)
- `wasm-bindgen-cli 0.2.126` (must match the crate exactly)
- Node.js `>=20`

Important Rust dependencies use exact versions in the workspace manifest and `Cargo.lock` is
committed. Release builds use fat LTO, one codegen unit, stripped symbols and aborting panics.

## Checks

Run formatting, Clippy, native tests, native release, and the Wasm build before publishing:

```bash
npm run fmt
npm run lint
npm test
cargo build --workspace --release
npm run build
```

Start `npm run dev`, open `http://127.0.0.1:4174` in a current Chrome-compatible browser, inspect
the console/network panel, and exercise both raw-disc and extracted-stream imports. Browser storage
is origin-specific; `localhost` and `127.0.0.1`, different ports, and different protocols do not
share card records.

## Local-data verification

Legally owned data may be placed under ignored `local-data/` or selected from anywhere on disk.
Never add fixtures cut from game streams. Synthetic pages and malformed byte arrays belong inline
in tests. If local golden hashes or screenshots are generated, keep them in ignored `artifacts/`.

The hosted retail-runtime and all-pair fractional camera checks can be run directly against a local
raw image without extracting or copying it into the repository:

```bash
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-sim --test local_retail_runtime --locked -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-web --lib --locked \
  builds_every_fractional_spawn_snapshot_directly_from_raw_disc -- --ignored --nocapture
```

These tests are ignored by default because they require user-supplied copyrighted data. Do not
commit their input, extracted streams, output captures, or locally derived golden payloads.

Before every commit, inspect `git status --short` and `git ls-files` for `.bin`, `.iso`, `.nsd`,
`.nsf`, `.wasm`, storage exports, secrets, browser profiles, screenshots, caches, and build output.
