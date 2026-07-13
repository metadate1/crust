# Architecture

## Safety boundary

All crates use `#![forbid(unsafe_code)]`. Parsing begins with immutable byte slices and checked
little-/big-endian readers. Disk offsets remain `Offset`, `PageId`, `EntryHandle`, `Eid`, or
explicit tagged-word enums. Runtime collections use indices with generation checks; no file word
becomes a native pointer and no C layout is transmuted.

This intentionally differs from the reference C implementation, which relocated 32-bit offsets
in place and depended on wasm32 pointers, compiler bitfields, aliasing exceptions, signed shifts,
and pointer-shaped negative status codes. Rust errors are enums and wrapping/saturating arithmetic
is written explicitly where it is part of the observed contract.

## Dependency direction

```text
crust-formats ──┬── crust-sim ───────┐
                └── crust-audio ─────┤
crust-renderer ──────────────────────┤
crust-platform ──────────────────────┤
                                     └── crust-web
```

The browser crate owns only browser concerns. Format validation, simulation, render-command
generation, mixing, input mapping and storage schemas remain native-testable.

## Local data flow

1. The browser receives `File`/`Blob` handles from a user gesture.
2. Disc imports read only the ISO sectors and extents needed for discovery. Raw 2352-byte Mode 2
   Form 1 sectors are validated and exposed as 2048-byte logical sectors.
3. S0–S3 records are normalized to `s0000000.nsd`/`.nsf`; unknown files are ignored and duplicate
   canonical names are rejected.
4. Before execution, NSD metadata and NSF pages are independently validated. Page entries and item
   ranges become immutable handle-based views. Destination pairs pass the same validation before an
   atomic retained-pair swap; simulation is stalled while the asynchronous read is in flight.
5. The 30 Hz simulation emits renderer/audio state. The browser schedules cooperatively and drops
   excessive lag instead of replaying an unbounded catch-up burst.
6. User game bytes are released on reload and are never serialized. Only checksummed 128-byte
   progression/options records enter `localStorage`.

## Rendering

The renderer library keeps a logical 512×240 PSX-style viewport and produces ordered triangles.
Texture pages are 64 KiB and decoded in 4-bit indexed, 8-bit indexed or BGR555/STP modes. Cache
keys include page generation, palette, region and blend mode. A bounded generation-aware cache
prevents stale textures after paging. The live browser stage now submits commands through the same
backend, which uploads decoded RGBA to WebGL2 and maps the four PSX blend conventions to explicit
adjacent passes. It also decodes and briefly presents the retained pair's retail LDAT loading image.
Image-backed title states are reconstructed from MDAT columns, IMAG indexed tiles and IPAL CLUTs
through validated EID lookups. A pointer-free spawn-snapshot builder follows LDAT's spawn zone into
ZDAT path/rectangle data, initializes a checked `SlstCursor` at the requested path point, parses
each referenced WGEO, resolves TPAG/CLUT texture regions, applies draw-count texture animation plus
the exact fixed-point world camera and depth ordering, and emits ordinary renderer commands. The
live stage installs the observed post-loading snapshot on each pair transition. A simulation-owned
two-tick gate keeps the loading image visible until that first gameplay presentation; the loading
overlay no longer uses a browser-frame timeout. The installed world does not yet advance after that
boundary or integrate entities, GOOL objects, effects and animation.

## Simulation and GOOL

The retail-frame model records the 30 Hz order `spawn → camera → texture begin → world → GOOL →
presentation`, signed 8.8 path progress, draw-skip state and draw-count timing. ZDAT entities decode
into owned descriptors and signed path points. A fixed 96-slot object pool, dedicated main-object
slot, eight logical roots, 304 spawn flags and generational handles reproduce the bounded spawn-tree
shape without host pointers.

The GOOL VM distinguishes external and shared/global code segments with checked `CodeAddress`
values. It implements absolute global calls with typed frames and argument cleanup, returns,
optional/null pointer input semantics, state-change yields and the child-spawn host effect needed by
the characterized Crash boot sequence. The ordinary runner stops at that synchronous boundary;
`run_with_host_effects` applies the callback before the following instruction while preserving the
same interpreter invocation. The arena has a separate runtime-child path with bounded `0x91`
reclaim selection, dedicated-player activation/reset and no runtime-child entity spawn ID. The
arena's generational handle and the VM's current index handle remain separate, so effects are not
yet coupled into the live browser object frame.

## Audio

The audio library is deterministic signed stereo at 44.1 kHz. Sixteen-byte SPU ADPCM blocks decode to 28
samples with saturated predictor history and loop-marker semantics. A bounded sample cache feeds
24 logical voices; voice zero is reserved for music. Sequence events drive a 64-voice software
synth. The browser output is unlocked only after a user gesture; mute does not stop simulation, and
SFX/music volume plus mono are applied as independent output policy. Its current sequence and
effects remain original generated signals rather than retail banks.

## Persistence

The platform crate parses and emits the existing JSON schema rather than inventing a new save.
Manual-card slots preserve damaged opaque records instead of silently formatting them. Invalid
automatic resume records are rejected so the browser host can move them to a timestamped quarantine
key. Newer schema versions are left untouched.
