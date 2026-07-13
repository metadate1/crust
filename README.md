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
input, WebGL2, WebAudio, and the native engine subsystems are implemented. The live browser host
now retains and remounts each validated destination stream pair, decodes retail LDAT loading
images, composes the image-backed retail title states from MDAT/IPAL/IMAG entries, and drives the
renderer command backend. The former data-independent diagnostic landscape/player geometry has
been removed. For 40 of 43 playable starts, gameplay now presents a bounds-checked progress-zero
ZDAT/SLST/WGEO world snapshot with decoded TPAG textures and retail camera/depth math. This is not
yet a moving retail world: entity-driven camera updates, objects, animation, effects and gameplay
coupling remain incomplete, and audio remains synthesized. See
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
continuing against stale data. Image-backed title entries are now materialized, and retail GOOL
entry/state graphs can be validated and bound natively, but gameplay entries are not yet
instantiated into the live object graph.

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
  scene metadata and tagged-reference validation.
- `crust-sim` — deterministic 30 Hz clock, checked GOOL program binding/word machine, level/title
  flow, collision, camera, paging, demos, and retail card payload/state handshakes.
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

- `c1.virtual-memory-card.v1` — 15 retail-format slots; diagnostic completion updates the loaded
  slot, falling back to slot zero.
- `c1.browser-resume.v1` — one checksummed automatic resume record.

Selected game files are not persisted and must be selected again after reload.
