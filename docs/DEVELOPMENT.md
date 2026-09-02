# Development and verification

This document contains the full maintainer workflow. For a short setup, start with the project
[README](../README.md). Commands that use retail data require files from your own disc and remain
ignored by default.

## Tool versions

- Rust `1.97.0` (pinned by `rust-toolchain.toml`)
- `wasm-bindgen-cli 0.2.126` (must match the crate exactly)
- Node.js `22.16.0` (the exact runtime declared by `package.json` and installed in CI)

Important Rust dependencies use exact versions in the workspace manifest and `Cargo.lock` is
committed. Release builds use fat LTO, one codegen unit, stripped symbols and aborting panics.

## Checks

Run formatting, native and Wasm Clippy, native tests, native release, and both production and
browser-harness Wasm builds before committing or pushing a release candidate:

```bash
npm run fmt
npm run lint
npm run lint:wasm
npm run lint:wasm:browser-harness
npm test
npm run build:wasm
npm run build:wasm:browser-harness
cargo build --workspace --release --locked
npm run build
npm run verify:dist
npm run build:browser-harness
npm run verify:browser-harness
```

The two browser-harness commands compile `crust-web` with the explicit, off-by-default
`browser-test-harness` feature. CI checks both production and harness Wasm with warnings denied,
then builds and verifies both generated distributions. All Cargo dependency resolution in these
gates is locked to the committed `Cargo.lock`.

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
the ordinary animation-frame loop. Instead, `window.__crustTest.step(heldMask)` runs one host
callback through the same `App::frame` and asynchronous pair-remount path used by production. Most
callbacks complete one 34 ms source sample. A callback that only advances asynchronous destination
or pager work—including a transient physical-page wait during retail PBAK startup—reports a
zero-step callback; the runner retains the same pad sample until that source frame completes. The
step hook accepts only a 16-bit pad mask. The feature build also exposes narrowly scoped boundary
helpers for the isolated Title-attract, direct-bonus, and virtual-card audits documented below.
They are absent from the normal `npm run build` artifact, which does not contain or expose
`window.__crustTest`.

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

Storage-specific browser regressions can opt into one local versioned record with
`--seed-card /path/to/card.json` and/or `--seed-resume /path/to/resume.json`. The runner accepts
only the exact v1 envelope, canonical base64 for each 128-byte retail payload, 15 card slots, and a
16 KiB maximum input. It installs the validated text before navigation and then verifies the exact
requested key/value without printing payload contents. With neither option the temporary profile
must still begin with both Crust storage keys absent.

A directly selected Tawna Bonus 1 has an owned-assets browser return audit:

```bash
node scripts/browser-harness-smoke.mjs \
  --asset /path/to/legally-owned-disc.bin \
  --audit-direct-bonus-return
```

The harness joins at the parsed WillC state-32 boundary already proven by the separate WarpC
`0x1600 [0]` proximity-gate regression. From there it runs the production state binder and key
ceremony, supplies CardC's ordinary physical confirmation edges, observes the real `LoadState`,
requires the direct-boot classifier and `LEVEL_END`, waits for the asynchronous Title pair mount,
and asserts Title state 5 (Main Menu). The audit fails on browser console/network/WebGL/runtime or
object faults; it does not claim to steer Crash physically through the bonus portal.

Cooked 2,048-byte ISO discovery has a separate non-proprietary import check:

```bash
npm run verify:browser-harness:cooked-iso
```

The command generates an ignored temporary 40-sector ISO9660 image whose 88 canonical records use
one-byte zero payloads, selects its `.iso` through the production file input, and stops before
launch. It requires the `ISO 2048` mount, all 44 pairs, the exact bounded `Blob.slice()` sequence,
and no network request after file selection. This proves browser classification and catalog
discovery without bundling game data; because its payloads are deliberately invalid NSD/NSF
streams, it is not gameplay or cooked-retail-image evidence.

Longer local traces use a run-length JSON file passed with `--replay`:

```json
{
  "schema": 1,
  "bootLid": 25,
  "unlockAll": false,
  "segments": [
    { "frames": 90, "held": 0 },
    { "frames": 1, "held": 2048 },
    { "frames": 1, "held": 0, "expect": { "mountedLid": 25, "minFrame": 92 } },
    { "frames": 15, "held": 0, "while": { "currentLid": 45, "mountedLid": 45 } }
  ],
  "expect": { "currentLid": 25, "minRetailExecutions": 1 }
}
```

The runner already yields to asynchronous authored stream requests and checks the remounted
destination before continuing. An optional `"while"` condition limits a segment to the named
`currentLid` and/or `mountedLid`; once the authored transition changes that pair, the runner skips
the segment's remaining scheduled frames without advancing the simulation. This is useful for
bounded completion-screen pulse cycles whose exact exit frame varies by collected-box count.
Final or per-segment expectations may also pin `retailDrawCount`, `retailProcessDrawCount`,
`retailRandomSeed`, and `retailRandomSeedB` exactly. The accelerated harness advances every
software-audio bus by the fractional-carrying 44.1 kHz sample count for each fixed 34 ms frame;
production remains scheduled against `AudioContext.currentTime()`. This keeps sample completion,
voice allocation, shared RNG-B, and repeated campaign traces independent of the host machine's
execution speed.
Segments default to `"inputKind": "physical"` and accept only the console's 16-bit physical pad
mask. A legally local diagnostic reconstructed from PBAK may instead mark a segment
`"inputKind": "recorded"`; only the feature-gated harness then supplies its complete 32-bit `held`
word through the existing demo override while physical input remains zero. Never commit such local
recordings. The current ordinary-route browser replay uses a reviewed pad-mask timeline plus exact
level/checkpoint expectations for all 89 phases from publisher/title through Cortex, Ending, and
the return to Title. Strict discovery joins all 89 fresh exporter fragments into one unique path;
the current browser artifact consumes all 146,501 replay frames plus two declared transition-settle
frames without a skipped frame or synthetic handoff. The campaign-replay hook intentionally cannot
force a transition or mutate GOOL state, so secret, alternate-completion, fault-recovery, and final
release traces must meet the same standard rather than using test-only game-state shortcuts. The
separate attract-audit hook described below has one narrower purpose: it supplies a dormant
destination LID at an exact Title transition boundary, but never supplies a PBAK EID, snapshot,
RNG seed, or recorded input.

Opt-in survey exports can be assembled into one ignored campaign replay without copying their
input into repository source. Keep one route's exported fragments under an ignored/local path,
discover their unique exact order, then compose and run:

```bash
npm run discover:browser-campaign-replay -- \
  --fragments target/local-campaign/fragments \
  --output target/local-campaign/manifest.json

npm run compose:browser-campaign-replay -- \
  --manifest target/local-campaign/manifest.json \
  --output target/local-campaign/campaign.replay.json

npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --replay target/local-campaign/campaign.replay.json
```

Discovery fails if exporter-named captures are disconnected or admit multiple equally long exact
orders; it never generates controller input or bridges a missing edge. The composer adds
current/mounted-LID guards to every run and asserts an exact carry checkpoint after every phase.
It rejects missing RNG/draw/restart continuity, mismatched fragment metadata, nonlocal exports,
and handoffs that bypass the retail Title / Island Map mount. See
[Local browser campaign replay discovery and composition](BROWSER_CAMPAIGN_REPLAY.md) for the
manifest contract and safety rules.

Legally local PCSX-RR `.pxm` and PSXjin `.pjm` input movies can be parsed and converted into the
same ignored replay format. The importer validates the version-two movie header, standard pads,
player-two/control-byte inactivity, native frame bounds, and the documented pad layout. It samples
one 60 Hz native input from each two-frame group by default, preserves recorded opposing
directions, and run-length encodes the result:

```bash
npm run import:psx-movie-replay -- \
  --movie /path/to/movie.pxm --check

npm run import:psx-movie-replay -- \
  --movie /path/to/movie.pxm \
  --start-frame 1737 --end-frame 2400 \
  --boot-lid 0x09 \
  --prefix-replay target/local-campaign/publisher-opening/lid-19-draw-00000000-publisher-title-to-09.json \
  --expect-clean \
  --output target/local-native-movie/n-sanity-aligned.replay.json

npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --replay target/local-native-movie/n-sanity-aligned.replay.json
```

`--prefix-replay` requires a legally-local, noncanonical replay with an exact terminal expectation.
The importer moves that expectation onto the prefix's final segment, so the browser must prove the
destination mount before the first movie input. `--expect-clean` checks restart, LoadState, and
death-camera counters after every imported tick and stops at the first change.

The last example is deliberately a diagnostic, not an automatic parity verdict. A power-on
emulator movie inherits BIOS, publisher/title, RNG, process, checkpoint, card, and loading state.
The verified prefix preserves Crust's equivalent session carry but does not prove that every hidden
native value is identical. PCSX-RR movies also contain 60 Hz emulator samples whose actual
game-input poll/lag map must be observed rather than inferred indefinitely with a fixed 2:1 sample.
Use `--start-frame`, `--end-frame`, `--sample-index`, and `--native-step` only after establishing an
observable alignment boundary, and keep the movie, converted replay, captures, and logs ignored.
The importer never treats a TAS movie as game data or as a canonical Crust campaign fixture.

## Retail attract playback audit

The shipped Title `GamOC` program contains Intro plus seven attract destinations: `0x0c`, `0x12`,
`0x0a`, `0x0e`, `0x20`, `0x1d`, and `0x29`. It has no attract edge to Upstream `0x0f` or Temple
Ruins `0x1c`, even though those pairs contain valid `pb0fB` and `pb0sB` recordings. Consequently an
"all nine through untouched Title idle" test can never terminate. Verify authored behavior as
seven natural Title returns plus two isolated dormant mounts:

```bash
npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --frames 100000 \
  --audit-retail-pbaks

npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --frames 10000 \
  --audit-isolated-retail-pbak 0x0f

npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --frames 10000 \
  --audit-isolated-retail-pbak 0x1c
```

The isolated feature-gated mount publishes `GAME_STATE_TITLE` and the destination LID at the
ordinary checked transition boundary. Production `PbakChoose` still counts the mounted pair's
type-19 entries, consumes RNG-B, constructs `pb0<level>B`, parses the user-owned recording, and
runs its complete caption/return handshake. The harness rejects missing or faulted captions,
non-acknowledged `0xE00`, malformed metadata, premature returns, runtime/GOOL/WebGL/network errors,
or an incomplete first occurrence. A legal repeated recording may remain armed only after all
required returns were already observed. Coverage is complete only after the final `LEVEL_END`
request has mounted a live Title pair again: the runtime must be running at current and mounted LID
`0x19`, with resident pages and entries and no pending browser-test destination request.

## Local-data verification

Legally owned data may be placed under ignored `local-data/` or selected from anywhere on disk.
Never add fixtures cut from game streams. Synthetic pages and malformed byte arrays belong inline
in tests. If local golden hashes or screenshots are generated, keep them in ignored `artifacts/`.

The two raw-disc `local_retail_runtime` checks and the all-pair fractional camera check can be run
without extracting or copying the image into the repository:

```bash
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  n_sanity_szon_resolves_last_serialized_neighbor_at_inclusive_origin \
  -- --ignored --exact --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  n_sanity_neighbors_spawn_and_crash_hosts_both_boot_children \
  -- --ignored --exact --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-web --lib --locked \
  builds_every_fractional_spawn_snapshot_directly_from_raw_disc -- --ignored --nocapture
```

Checks that use `C1_STREAM_DIR` need extracted files. The extractor accepts only an explicit disc
and output path, validates and stages all 88 known streams, atomically claims a new output
directory, and publishes canonical lowercase files with create-new semantics. It never replaces an
existing path. The claimed directory can be visible while publication finishes, so consume it only
after the command succeeds. A publication failure or interruption deliberately leaves the exact
claimed output path in place: inspect and remove it manually before retrying, since automatic
rollback could delete a replacement installed by another same-user process. Ordinary failures
remove the private sibling staging directory or return its cleanup error. Any termination that
bypasses Rust cleanup—including `SIGKILL`—or a power loss can leave
`.OUTPUT-NAME.crust-extract-PID-N`; confirm that PID is no longer running, inspect the path, and
remove that stale staging directory manually:

```bash
cargo run --locked -p crust-formats --bin extract-streams -- \
  /path/to/disc.bin local-data/streams
```

The current all-target inventory contains 1,359 tests: 1,099 default-active and 260 ignored. The
main current-fixture sweep explicitly excludes the replay exporter and nine opt-in historical
fixtures, so it executes 250 entries serially. One of those entries validates its current
Up-the-Creek/Ripper/Map prefix and deliberately stops before a separately opt-in legacy Lost City
tail. The raw-disc checks read the whole image, and several route tests need a 64 MiB thread stack.
Run it from a shell where no other behavior-changing `C1_*` variables are set; the command below
then supplies only the disc, stream, and stack inputs and explicitly clears the known opt-ins.
`--no-fail-fast` lets the inventory continue past any remaining current-fixture failure so the
complete current-fixture diagnostic result is reported:

```bash
env -u C1_BROWSER_REPLAY_EXPORT \
  -u C1_LEGACY_SLIPPERY_CARRY -u C1_LEGACY_SUNSET_CARRY \
  -u C1_LEGACY_LOST_RESTART_ROUTE -u C1_LEGACY_SYNTHETIC_CAMPAIGN \
  C1_DISC_IMAGE=/path/to/disc.bin \
  C1_STREAM_DIR="$(pwd)/local-data/streams" \
  RUST_MIN_STACK=67108864 \
  cargo test --workspace --all-targets --locked --no-fail-fast -- \
  --ignored --test-threads=1 \
  --skip legacy_ \
  --skip lost_city_completion_route_reaches_title_after_checkpoint_recovery \
  --skip exported_publisher_opening_composes_through_jungle_mount
```

The replay-export test opts into writing legally local replay fragments and must run alone because
ordinary surveys deliberately reject that export mode. Give every run a fresh ignored directory:

```bash
mkdir -p "$(pwd)/target/local-campaign"
C1_STREAM_DIR="$(pwd)/local-data/streams" \
  C1_BROWSER_REPLAY_EXPORT="$(mktemp -d "$(pwd)/target/local-campaign/publisher-opening.XXXXXX")" \
  RUST_MIN_STACK=67108864 \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  exported_publisher_opening_composes_through_jungle_mount -- \
  --ignored --exact --test-threads=1
```

These tests are ignored by default because they require user-supplied copyrighted data. Do not
commit their input, extracted streams, output captures, or locally derived golden payloads.

Before every commit, inspect `git status --short` and `git ls-files` for `.bin`, `.iso`, `.nsd`,
`.nsf`, `.wasm`, storage exports, secrets, browser profiles, screenshots, caches, and build output.
CI repeats this boundary check across the current index and every commit tree newly reachable from
the pushed or pull-request tip. Its full-history checkout catches files added and deleted within a
single update; it never prints matching file contents or pathnames.
