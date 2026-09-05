# CRUST — a Rust/WebAssembly runtime for Crash Bandicoot

CRUST is an independent, source-available Rust and WebAssembly compatibility runtime for the
original 1996 PlayStation release of *Crash Bandicoot*. It runs in a web browser and reads game
data from a disc image that the user selects on their own device.

CRUST does not include the game, a BIOS, extracted game files, or a download link for them. The
browser does not upload or keep the selected game file. See [Privacy](PRIVACY.md) for the exact
data boundary.

CRUST is an independent research project. It is not affiliated with or endorsed by Sony,
Naughty Dog, or the owners of *Crash Bandicoot*.

> **License status:** This is source-available research code, not open-source software. Public
> access does not grant permission to reuse or redistribute the project. Read
> [LICENSE.md](LICENSE.md) and [RIGHTS_AND_LICENSES.md](RIGHTS_AND_LICENSES.md) before using the
> source.

## What works

The current browser runtime can:

- read a user-selected NTSC-U disc image or extracted NSD/NSF stream pairs;
- parse all 44 retail stream pairs;
- run the title screen, menus, map, ordinary levels, bosses, bonuses, and ending flow;
- render retail worlds, objects, sprites, text, textures, loading images, and visual effects;
- execute the retail GOOL object programs in a checked Rust runtime;
- simulate input, collision, cameras, checkpoints, saves, music, and sound effects; and
- preserve card and options data in the browser's local storage.

The strongest current browser test follows one exact 89-phase route from Title back to Title. It
runs 146,501 source frames, reaches Title LID `0x19`, skips no replay frame, and reports no checked
browser, WebGL, runtime, execution, object, zone, spawn, or post-selection network failure.

This proves that the tested campaign route works on the current browser build. It does **not**
prove perfect PlayStation emulation or complete parity for every secret route, demo, damaged save,
input device, SPU edge case, or CD-drive behavior.

For precise coverage and known limits, read:

- [Compatibility](docs/COMPATIBILITY.md) — what works and what remains;
- [Verification record](docs/VERIFICATION.md) — dated test evidence; and
- [Architecture](docs/ARCHITECTURE.md) — how the runtime is built.

## Run locally

You need:

- Node.js `22.16.0`;
- Rust `1.97.0` with `rustfmt`, `clippy`, and the `wasm32-unknown-unknown` target; and
- `wasm-bindgen-cli 0.2.126`.

Install the pinned tools, build the browser application, and start the local server:

```bash
rustup toolchain install 1.97.0 --profile minimal --component clippy,rustfmt \
  --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm run dev
```

Open <http://127.0.0.1:4174>. Select your own supported `.bin` or `.iso` disc image. You can also
select matching `.NSD` and `.NSF` files that you extracted from your own disc.

The selected file stays on your device. Your browser gives CRUST local access to it for the current
session; the file is not sent to a server.

### Extract streams for native tests

Native tests use an extracted stream directory. The output path must not exist before extraction:

```bash
cargo run --locked -p crust-formats --bin extract-streams -- \
  /path/to/your-disc.bin "$(pwd)/local-data/streams"
```

The extractor validates and stages all 88 known streams before it claims the output directory. Do
not use the directory until the command succeeds. If the process is interrupted after publication
starts, inspect and remove the incomplete output before trying again. See
[Development](docs/DEVELOPMENT.md) for the full recovery procedure.

## Controls

| PlayStation control | Keyboard |
| --- | --- |
| D-pad | Arrow keys or W/A/S/D |
| Cross | Z or Space |
| Square | X or mouse button |
| Circle | C |
| Triangle | V |
| L1 / R1 | `[` / `]` |
| L2 / R2 | Q / E |
| L3 / R3 | K / L |
| Start | Enter |
| Select | Shift |

The default display is the retail-style 4:3 view at native resolution. Wider aspect ratios,
higher internal resolutions, smooth presentation, and extended-world rendering are optional
display settings. They do not change the 30 Hz simulation.

## Develop and test

Run the default checks:

```bash
npm run fmt
npm run lint
npm run lint:wasm
npm run lint:wasm:browser-harness
npm test
npm run build
npm run verify:dist
npm run build:browser-harness
npm run verify:browser-harness
```

Tests that use retail data are ignored by default. They require files from your own disc and must
run locally. The repository and CI contain no retail game data.

Before a public source release, also run:

```bash
bash scripts/check-public-release.sh --remote origin
```

This command checks the working tree and reachable Git history for prohibited game files, large
blobs, unexpected binary media, and common credential patterns. It is a safety check, not a legal
opinion.

See [Development](docs/DEVELOPMENT.md) for all build, browser-harness, campaign-replay, emulator,
and legally local test commands.

## Workspace

- `crust-formats` parses disc, stream, scene, model, animation, replay, and executable data.
- `crust-sim` runs the deterministic 30 Hz game simulation and checked GOOL object runtime.
- `crust-renderer` converts retail graphics data into safe rendering commands.
- `crust-audio` decodes and mixes retail music and sound effects.
- `crust-platform` maps input and stores versioned browser records.
- `crust-web` connects local browser files, WebGL2, WebAudio, storage, and the application loop.

Every crate forbids unsafe Rust. Disc offsets and object references are checked before use; they
are not treated as native pointers.

## Documentation

Start with the [documentation guide](docs/README.md). Important files include:

- [Architecture](docs/ARCHITECTURE.md)
- [Compatibility](docs/COMPATIBILITY.md)
- [Development and verification commands](docs/DEVELOPMENT.md)
- [Verification record](docs/VERIFICATION.md)
- [Browser campaign replay contract](docs/BROWSER_CAMPAIGN_REPLAY.md)
- [Migration evidence](docs/MIGRATION.md)

## Project and data rights

Tracked files contain code, tests, documentation, and four original CRUST interface images. They
do not contain a retail disc image, BIOS, game executable, extracted stream, retail screenshot,
recording, music, texture, model, save state, or other game-derived asset.

Users are responsible for obtaining and using their own game data lawfully. Do not open an issue
or pull request that contains game data or copyrighted game media.

Read these files before publishing, redistributing, or contributing:

- [License notice](LICENSE.md)
- [Rights and licenses](RIGHTS_AND_LICENSES.md)
- [Copyright and provenance notice](NOTICE.md)
- [Third-party dependency notices](THIRD_PARTY_NOTICES.md)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)
