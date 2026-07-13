# Migration evidence

## Read-only baseline

The reference tree was characterized at
`7f05e5febd63e603f243c089c8b9918211c7b991` without changing its working tree. The user's existing
untracked `scripts/forensics/` work was present before and after characterization.

An external `git archive` copy produced these results:

- 17 native C tests and two JavaScript tests passed with sanitizers where configured.
- The Emscripten build completed, producing a 703,871-byte `c1.wasm` and 386,704-byte `c1.mjs`.
- The source extractor recognized the legally local raw image as Mode 2/2352 and found 88 streams,
  44 pairs, totaling 229,312,048 bytes.
- A Chrome run reached title states `10 → 7 → 8 → 5` and entered Intro `0x38` with active audio;
  that interrupted characterization run did not wait long enough to record Intro's return.

## Preserved contracts

- Pages are exactly `0x10000` bytes; entries begin with magic/EID/type/item count and 32-bit item
  offsets.
- Playable NSD metadata uses the `0x520` layout and an LDAT record; Cave `0x04` uses the older
  index-only form.
- EIDs use the `0-9a-zA-Z_!` alphabet, five six-bit characters and an odd low-bit tag.
- Page and entry references retain their exact 32-bit tags, but resolve through validated handles.
- Simulation is cooperative at 30 Hz and excessive lateness resets its deadline.
- The catalog contains 44 exact level pairs, with 43 boot targets.
- Title/menu states include main `5`, options `6`, publisher `8`, game over `12`, password/load
  `13/14`, and map `15`; initial publisher flow begins at `10`.
- A bonus return target uses the signed transition sentinel `-2`.
- Audio is 44.1 kHz, 24 logical voices, with music on voice zero.
- The virtual card has 15 slots and an exact 128-byte little-endian payload. Its checksum starts at
  `0x12345678`, adds each byte with the checksum field zero, then rotates left three bits.
- Existing storage keys and schema versions remain unchanged.
- Image-backed title states resolve `5MapP`, `7MapP`, `8MapP`, and `aMapP` MDAT graphs; each IMAG
  column contains 16×16 indexed tiles and selects a CLUT item through the MDAT IPAL table.
- GOOL programs are retained as owned code/state/EID tables. Serialized PCs are validated within
  the 14-bit code space and never converted to host pointers. External/global code addresses and
  return frames are explicit values; global calls remove their validated argument span on return.
  Child-spawn opcodes stop at a synchronous host boundary, then continue in the same hosted
  interpreter invocation before the next instruction. They pop arguments for every signed count
  and expose `0x91`'s bounded reclaim permission rather than an alternate parent.
- Retail GOOL animation item five is retained as owned bytes, and tagged animation references are
  validated against it before selection. State-change yields resolve the requested state through
  NSD/NSF metadata, bind a new checked state program, and resume with explicit wrapping frame/draw
  stamps; serialized state PCs and animation offsets never become native pointers.
- ZDAT runtime pointer slots are treated as opaque serialization fields. Zone/world/path EIDs,
  SLST polygon IDs and WGEO word/vertex indices remain validated offsets and values; the Rust scene
  builder never writes host addresses back into source bytes.
- ZDAT entities retain the exact 20-byte header and six-byte signed path points. The 304 entity
  spawn flags, 96 ordinary objects, dedicated main slot and eight roots are fixed-size Rust state;
  generations reject stale object references after despawn.
- SLST visibility is reconstructed from the nearest raw endpoint with the retail midpoint tie-break.
  Every adjacent delta is bounds-checked in both directions, and a failed seek rolls back both the
  point index and ordered visibility list.
- Direct-loading presentation uses the observed two-step draw skip: tick one executes but is
  discarded, while tick two presents gameplay with path progress `0x200` and draw count one.
- The live retail-object bridge scans displayed neighbor zones, uses separate typed generational
  arena and VM handles, traverses the mutation-aware spawn tree, and applies synchronous runtime
  child creation inside the same 30 Hz frame. Its effects remain data rather than unchecked host
  pointers and do not yet imply camera, collision, renderer, audio or progression integration.

## Deliberate corrections

The rewrite rejects out-of-range item offsets, cyclic/unbounded bucket walks, invalid GOOL stack
access, division by zero, oversized shift counts, unbounded collision result aggregation, bad
texture/CLUT ranges, malformed audio banks/sequences, and PBAK frame overruns. These were undefined
or insufficiently bounded in C and are not compatibility behavior.

No C runtime, C compatibility layer, copied header, or vendored C synthesizer remains in the
current application build. Upstream attribution is retained because its observable behavior and
format research informed this work.
