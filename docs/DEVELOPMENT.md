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

`npm run dev` rebuilds the release Wasm distribution before it opens the server. Use
`npm run serve` only when deliberately reusing `dist/`: the server verifies the recorded source
fingerprint and every served bootstrap/JavaScript/Wasm artifact before listening. The browser
publishes that manifest as `window.__crustBuild` and requests generated JavaScript and Wasm with
its build ID. A source change, tampered artifact, or missing manifest therefore fails closed
instead of serving an old diagnostic runtime. The build is assembled in ignored `target/` storage
and published only after the source fingerprint is rechecked, preserving the last valid `dist/`
when compilation or binding generation fails.
`npm run verify:dist` performs the same verification without opening a listening socket.
The development server repeats verification for every request and returns `503` after source,
artifact or Git drift. Builds use a repository-local lock so concurrent publishers cannot nest or
discard staged distributions.

An off-by-default browser campaign harness can be built and served separately:

```bash
npm run build:browser-harness
npm run verify:browser-harness
npm run serve:browser-harness
```

It writes only to ignored `target/browser-test-dist/` and serves on
`http://127.0.0.1:4175`; it never replaces production `dist/`. This feature build does not start
the ordinary animation-frame loop. Instead, `window.__crustTest.step(heldMask)` advances exactly one
34 ms sample through the same `App::frame` and asynchronous pair-remount path used by production.
The hook accepts only a 16-bit pad mask and exposes no GOOL-state mutation or forced-transition
operation. It is intended for an ignored, legally local browser campaign test; the normal
`npm run build` artifact does not contain or expose `window.__crustTest`.

Run the deterministic Chrome smoke with one raw image or enough extracted pairs for the selected
boot stream:

```bash
npm run verify:browser-harness:smoke -- \
  --asset /path/to/S0000019.NSD \
  --asset /path/to/S0000019.NSF
```

The runner builds the isolated harness, starts its own loopback server, launches the system
Chrome/Chromium with a temporary profile, clears the virtual-card and resume storage before boot,
sets the real `#gameFiles` browser input to the local paths, and advances 120 zero-input frames at
34 ms each. It fails on bootstrap/runtime/GOOL/WebGL/console/network errors, non-GET or cross-origin
traffic, and missing simulation activity. Its screenshot is written under ignored
`target/browser-test-artifacts/`; game bytes are read in place and are never copied or uploaded.
Use repeated `--asset` arguments for a BIN/ISO or additional extracted pairs. `--chrome` overrides
the browser executable. Add `--unlock-all` to assert in the feature-only read-only debug snapshot
that both retail life globals equal `999 << 8`, the map access gate equals 99, and both key-path
bits are present.

Longer local traces use a run-length JSON file passed with `--replay`:

```json
{
  "schema": 1,
  "bootLid": 25,
  "unlockAll": false,
  "segments": [
    { "frames": 90, "held": 0 },
    { "frames": 1, "held": 2048 },
    { "frames": 1, "held": 0, "expect": { "mountedLid": 25, "minFrame": 92 } }
  ],
  "expect": { "currentLid": 25, "minRetailExecutions": 1 }
}
```

The runner already yields to asynchronous authored stream requests and checks the remounted
destination before continuing. A complete campaign proof still needs a captured, reviewed pad-mask
timeline plus level/checkpoint expectations for the whole retail route; the repository does not yet
contain that controller oracle. The browser hook intentionally cannot force a transition or mutate
GOOL state, so the missing trace cannot be replaced by test-only game-state shortcuts.

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
