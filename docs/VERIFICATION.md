# Verification record

This file records observed checks for the initial private rewrite delivery on 2026-07-12 and the
stream, title, GOOL, entity, SLST and first-presentation vertical slices on 2026-07-13. It does not
turn subsystem tests into a claim of retail gameplay parity.

## Reference characterization

The Bandicoot source tree was treated as read-only at
`7f05e5febd63e603f243c089c8b9918211c7b991`. An external archive build passed 17 native C and two
JavaScript tests plus its Emscripten build. Its Chrome title sequence reached numeric states
`10 → 7 → 8 → 5` and then Intro `0x38`. The source working tree, including pre-existing untracked
forensics files, was unchanged after characterization.

## Legally local data

The opt-in local-data test and browser pass used the user's own NTSC-U image without copying it into
this repository. Detection reported Mode 2/2352, 88 streams, 44 exact pairs, and 229,312,048 logical
stream bytes. Every filename and declared extent was matched against the extracted S0–S3 set.

Read-only scene characterization covered 1,223 ZDAT entries, 1,735 paths, 520 WGEO entries and
1,726 SLST entries containing 138,038 items. All 43 playable LDAT spawn zones/paths resolved. The
static scene builder produced world commands for 40 starts; Title, Hog Wild and Whole Hog use
zero-world dummy starts with external SLST placeholders. Exhaustive mutable-SLST characterization
covered 1,726 resolved paths, 136,312 visibility states, 134,586 adjacent transitions, 269,172
forward/backward inverse round trips and 89,666,970 validated polygon references, with fingerprint
`0x1400935c08cfe148`.

All 4,292 retail ZDAT entities and 16,363 signed entity path points parsed; every entity was in
group three. The data contained 52 main-object candidates, 624 valid executable/subtype bindings
and seven bindings that the retail program loader also rejects, with fingerprint
`0x71524c62fcbf6ddb`. N. Sanity Beach's progress-zero baseline remained 4 worlds and 681 visible
polygons. The observed first-presented state at path point two/draw count one produced 679 visible
polygons from both the extracted pair and directly from the raw BIN.

The real N. Sanity Crash program was then executed through its first retail host boundaries. Tests
verified absolute global call word `0x8609806e` to global PC 110, return at global PC 131, the exact
optional-pointer word `0x16be0e1f`, and the first two child-spawn yields with argument cleanup,
without treating any serialized word as a native pointer.

## Browser checks actually performed

A generated release Wasm build was served on `127.0.0.1` and exercised with agent-browser 0.27.0
using its Chrome engine:

- Wasm bootstrap reached `running`; `__consoleErrors` stayed empty and WebGL reported error zero.
- Raw 632,083,536-byte BIN discovery mounted all 88 streams and exposed all 43 bootable pairs.
- The 88 extracted NSD/NSF files independently mounted all 44 pairs; direct boot of level `0x09`
  parsed 80 pages and 231 entries.
- Title/publisher progression reached main menu; Intro `0x38` and return were observed.
- Options changed SFX volume, and password and empty-card load screens were entered.
- Direct gameplay boot, keyboard movement/jump input, pause, mute, fullscreen, and WebAudio resume
  activity were exercised. Automatic resume restoration was observed after reload.
- A 393×852 responsive viewport rendered the complete touch pad, including shoulders, L3/R3,
  Start and Select. Touch mapping itself is covered by native tests; multitouch on physical mobile
  hardware was not performed.
- Unsupported input was rejected with an actionable error after a browser-discovered fix.
- The final network trace contained only same-origin HTML, CSS, bootstrap JavaScript, generated
  wasm-bindgen JavaScript and Wasm. No asset upload/request occurred, and there were no console or
  page errors.

The 2026-07-13 release build was then exercised in a fresh agent-browser 0.27.0 Chrome session:

- A 632,083,536-byte raw BIN was selected through the real file input. Client-side discovery
  produced 88 files, all 44 pairs, 43 boot targets, and 229,312,048 logical bytes.
- The data-backed flow mounted `0x19 → 0x38 → 0x19 → 0x03 → 0x2D → 0x19 → 0x03`. The debug surface
  confirmed that current and retained stream IDs converged after every asynchronous swap. Observed
  destination counts included 113/558 pages/entries for `0x19`, 41/149 for `0x38`, 77/304 for
  `0x03`, and 15/57 for `0x2D`.
- The host decoded and uploaded 432×144 retail loading images for `0x19` and `0x03`. The opt-in
  local renderer characterization decoded all 39 loading images present among bootable streams.
- Title cards, main menu, Options and Map were navigated with keyboard input. Live options changed
  SFX and music from 255 to 239 and toggled mono; a triggered SFX produced measured mixer peak 767.
- Keyboard completed the diagnostic movement goal once. A second run held the on-screen touch Up
  control, reached completion in 2.24 seconds, cleared its held visual state, and mounted `0x2D`.
  Pause/resume and mute/unmute changed live runtime state and were restored before continuing.
- A 390×844 viewport had zero horizontal overflow and displayed the complete touch controller. A
  diagnostic completion created card slot zero. After reload, both versioned storage records
  remained, while the file input was empty and mounted-pair count returned to zero.
- WebGL reported error zero throughout. Console and page-error logs were empty. The network record
  contained only same-origin HTML, CSS, bootstrap JavaScript, generated wasm-bindgen JavaScript and
  Wasm; no game-data request or upload occurred.
- The exact final Wasm artifact was re-smoked with only the extracted `0x19` pair. It mounted
  113 pages/558 entries with WebGL error zero; when the title flow requested absent `0x38`, the
  simulation stopped as `BLOCKED`, retained `0x19`, and displayed the missing local filename rather
  than advancing or presenting the destination against stale assets.

The later first-presentation build was reloaded at `http://127.0.0.1:4174/` in the visible Codex in-app
browser. Its DOM contained the complete loader, two local file inputs and one canvas; there was no
framework error overlay and the captured warning/error console was empty. The served canvas backing
size was 1024×768 and the HTTP response used `Cache-Control: no-store`. Browser automation in
this environment could not populate the operating-system file chooser, so this specific build's
raw-BIN import and WebGL scene presentation are not claimed as browser-exercised. The same raw BIN
was exercised directly by the opt-in Rust disc-to-scene test described above. A user can select it
through the visible local-file control without changing the no-upload architecture.

Screenshots and game data remained outside Git. See `COMPATIBILITY.md` for features that were not
exercised or are not yet connected to the live browser runtime.

## Final automated results

- `cargo fmt --all -- --check`: passed.
- workspace Clippy with `-D warnings`: passed.
- locked native workspace suite: 223 asset-free tests passed, zero failed; 14 legally local tests
  remain ignored by default.
- all 14 opt-in local tests passed with `C1_DISC_IMAGE` and `C1_STREAM_DIR`: raw-disc/catalog,
  all-pair parsing, entity/program binding, GOOL graph/boot execution, exhaustive SLST traversal,
  scene formats, 40 standalone snapshots, 39 loading images, 1,427 representative texture
  references and all four image-backed title states. The entity and scene-format tests also passed
  using the raw BIN alone.
- locked optimized native workspace build: passed.
- locked optimized `wasm32-unknown-unknown` web build: passed.
- generated web release: passed; Wasm payload was 485,081 bytes (SHA-256
  `51930742ab573f51774d6e0eb77abd6d547666bf9f97da38bc043b112c048333`).

## Reproducible commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets --locked
C1_DISC_IMAGE=/path/to/disc.bin C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-formats --test local_disc -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-renderer --test local_loading_images -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin C1_STREAM_DIR=/path/to/streams \
  cargo test --workspace --all-targets --locked -- --ignored --nocapture
cargo build --workspace --release --locked
cargo build --release --locked --target wasm32-unknown-unknown -p crust-web
npm run build
```

The delivery summary identifies the exact published commit.
