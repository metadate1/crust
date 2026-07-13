# crust

`crust` is a browser-only Rust/Wasm interoperability rewrite of the C1 game-engine lineage. It
loads data from a legally owned Crash Bandicoot NTSC-U disc locally, recognizes the S0–S3 stream
set, and runs without uploading or bundling game data.

> **Private repository.** No public distribution license is granted. C1's upstream licensing is
> unresolved and the original game rights require separate review. Keep source and builds private;
> see [NOTICE.md](NOTICE.md).

## Implementation status

This repository is a working, tested Rust/Wasm compatibility foundation, but it is **not yet a
retail-equivalent game runtime**. Disc extraction, all 44 stream pairs, checked NSD/NSF parsing,
the 30 Hz state machine, menu/options/password/load/map shells, direct boot, local persistence,
input, WebGL2, WebAudio, and substantial native engine subsystems are implemented. The live browser
host retains and remounts each validated destination stream pair, decodes retail LDAT loading
images, composes image-backed retail title states from MDAT/IPAL/IMAG entries, and drives the
renderer command backend. The former data-independent diagnostic landscape/player geometry has
been removed. For 40 of 43 playable starts, gameplay presents a bounds-checked initial
ZDAT/SLST/WGEO path-point snapshot with decoded TPAG textures and retail camera/depth math. The
loading-image path follows the observed two-tick presentation gate and uses the first presented
path point and texture-animation count; N. Sanity Beach resolves to 679 visible polygons at that
boundary.

The browser now owns a checked retail object runtime for gameplay, bonus, boss and ending states.
At the cooperative 30 Hz boundary it scans displayed current-zone neighbors, spawns group-three
ZDAT entities into the bounded arena, binds their GOOL programs from the mounted NSF, applies hosted
child-spawn effects, and preserves typed arena/VM links. Entity objects now receive their
zone-relative path position, rotation/mode flags, scale, process defaults, player/object color
matrix and typed parent/player links; runtime children inherit their parent's transform.
State-change yields rebind the requested retail state before the next execution, including the
characterized animation-select and animation-wait path. Initial/call frames share the bounded
process word array at the parsed `init_sp`, state links apply target-state guards, and checked
failures quarantine only the affected object instead of skipping an unsupported instruction on the
next frame. The WebGL stage also has a transactional scene-update path that reuses exact-content
textures and can replace commands without needless texture uploads. This is still not a moving
retail world: GOOL mutations do not yet drive the installed scene, camera, collision, input globals,
gameplay progression, retail audio, saves, or transitions. The displayed retail world remains a
static spawn snapshot and the current audio remains synthesized. See
[compatibility](docs/COMPATIBILITY.md) for the exact gaps and [verification](docs/VERIFICATION.md)
for checks actually performed.

## Run locally

Requirements are Node.js 20+, Rust 1.97.0 through `rustup`, and the matching `wasm-bindgen` CLI:

```bash
rustup toolchain install 1.97.0 --profile minimal --component clippy,rustfmt \
  --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm run build
npm run dev
```

Open <http://127.0.0.1:4174>. Choose either the raw `.bin`/`.iso`, or the `.NSD` and `.NSF`
files from the disc's S0–S3 directories. The browser reads Blob ranges locally. The content
security policy blocks cross-origin connections, and no runtime API uploads asset bytes.

The 44 retail pairs are recognized. Cave (`0x04`) is mounted as a shared index/archive but is not
a boot target; the other 43 pairs are selectable. Partial stream sets containing at least one
complete pair are accepted. Each cross-level transition now validates and mounts its destination
pair on demand; a missing destination pauses the simulation with an actionable error instead of
continuing against stale data. Image-backed title entries are materialized, and retail GOOL
entry/state graphs can be validated and bound natively. Zone entities and their 304-slot spawn
flags are instantiated into a checked 96-object arena and run by the live browser at 30 Hz. This
execution slice is observable through the engineering log/debug counters, but it does not yet move
the rendered world or constitute a playable retail level.

## Controls

| PlayStation input | Keyboard | Standard gamepad |
|---|---:|---:|
| D-pad | Arrow keys | D-pad / left stick |
| Cross / jump | `Z` | A / Cross |
| Square / spin | `X` | X / Square |
| Circle | `C` | B / Circle |
| Triangle | `V` | Y / Triangle |
| L1 / R1 | `A` / `S` | LB / RB |
| L2 / R2 | `Q` / `W` | LT / RT |
| L3 / R3 | `K` / `L` | Stick clicks |
| Start / Select | `Enter` / `Space` | Start / Back |

The complete pad is also available through multi-touch controls on coarse-pointer devices.
Fullscreen, pause, and mute are in the stage toolbar.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
cargo build --release --target wasm32-unknown-unknown -p crust-web
```

`npm run build` additionally generates the unavoidable JavaScript/Wasm loader into ignored
`dist/pkg/`. All authored engine, parsing, simulation, rendering, audio, input, and persistence
logic is Rust. Static HTML/CSS and the small Wasm bootstrap are the only hand-authored web glue.

## Workspace

- `crust-formats` — endian-explicit ISO9660, raw-sector, NSD/NSF, page, entry, EID, GOOL program,
  ZDAT entity/path, stateful SLST visibility, scene metadata and tagged-reference validation.
- `crust-sim` — deterministic 30 Hz clock/presentation contract, checked GOOL program
  binding/word machine, hosted retail entity runtime with state rebinding, bounded object arena,
  level/title flow, collision, camera, paging, demos, and retail card payload/state handshakes.
- `crust-renderer` — PSX texture/TPAG/UV decoding and cache, projection, ordering, clipping, blend
  passes, title composition and WebGL2-ready commands.
- `crust-audio` — SPU ADPCM, sample cache/mixer, sequence events and a 44.1 kHz software synth.
- `crust-platform` — keyboard/gamepad/touch mapping and versioned browser persistence envelopes.
- `crust-web` — Blob-backed local imports, WebGL2/WebAudio presentation, browser storage and the
  cooperative application loop.

Every crate forbids unsafe Rust. On-disk 32-bit offsets and tags remain typed values or validated
handles; they are never reinterpreted as host pointers. See [Architecture](docs/ARCHITECTURE.md),
[migration evidence](docs/MIGRATION.md), and [compatibility](docs/COMPATIBILITY.md).

## Data and legal boundary

The repository intentionally contains no game asset, disc sector, executable, art, audio, texture,
or stream. `*.bin`, `*.iso`, `*.nsd`, `*.nsf`, local data directories, build output, fuzz corpora,
storage exports, caches, and captures are ignored. The two supported persistent records contain
only the retail 128-byte progression/options payload:

- `c1.virtual-memory-card.v1` — 15 slots containing checksummed retail-format payloads.
- `c1.browser-resume.v1` — one checksummed automatic resume record.

Selected game files are not persisted and must be selected again after reload.
