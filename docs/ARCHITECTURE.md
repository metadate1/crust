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
5. The browser schedules flow, retail-object execution and presentation cooperatively at 30 Hz and
   drops excessive lag instead of replaying an unbounded catch-up burst. A presented gameplay tick
   selects the exact `RetailCameraRuntime` zone/path/progress, executes GOOL, snapshots the live
   arena in retail preorder, and atomically builds one pair-scoped world/object command stream.
   Automatic camera modes use pad taps; follow modes consume the hosted main object's typed
   transform, zoom, held pad and prior frame stamp. The current union assumes that GOOL does not
   replace page residency midway through the tick; complete paging, progression and retail audio
   remain later host boundaries.
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
through validated EID lookups. A pointer-free active-scene builder starts from LDAT and can then
resolve any validated ZDAT zone/path selected by the camera. It initializes a checked `SlstCursor`,
parses each referenced WGEO, resolves TPAG/CLUT texture regions, applies draw-count texture animation
plus the exact fixed-point world camera and depth ordering, and emits ordinary renderer commands.
The live stage installs the observed post-loading snapshot on each pair transition and subsequently
uses camera-owned signed-8.8 progress plus the retail-frame pre-increment draw count. The
pair-scoped builder retains the active zone/path's parsed ZDAT header, rectangle, path, mutable SLST
cursor and WGEO geometry. Its eight TPAG mappings and bounded decoded-texture cache persist across
camera-only frames; changing zone/path replaces that graph, and mounting a destination pair creates
a new owner. Build-local texture handles keep scene commands deterministic even when decoded pixels
come from the persistent cache. Diagnostic counters expose graph builds/reuses, page installs and
texture hits/misses, but are not a substitute for browser frame-time measurement. A simulation-owned
two-tick gate keeps the loading image visible until the first gameplay presentation; the loading
overlay no longer uses a browser-frame timeout.

The same builder consumes immutable post-GOOL `RetailRenderObject` snapshots. It resolves unaligned
animation references only through the mounted pair's GOOL item five, accepts the 3D SVTX/CVTX
vertex path, loads the exact TGEO/frame variant, and applies retail object-local YXZ rotation,
scale, lighting, face culling and depth. World and object texture requests are collected before a
single cache freeze and filtered through the current ZDAT load list. Commands share one texture
manifest, and reversed object insertion is placed ahead of the already compensated world stream to
preserve the source's head-insert ordering-table behavior. Sprite, font, text, fragment and 2D CVTX
paths are explicit counted skips rather than being projected with the wrong transform.

`GlStage` can transactionally update an installed retail scene. It validates command texture
references, compares immutable decoded-texture allocation identities, prepares all new/replacement
GPU textures before committing, removes stale handles, and has a command-only fast path when the
pair-scoped cache returns the same allocation. A distinct allocation is conservatively uploaded
even if its bytes happen to match, avoiding per-frame pixel-vector clones and byte scans. Automatic
`CamUpdate` and live main-object `CamFollow` path/zone changes select the canvas scene; pause freezes
the last successfully installed snapshot. Three-dimensional vertex object models and their current
animation transforms are reflected in that snapshot; zone-object lifetime changes, dynamic global
display masks and mid-frame paging-driven texture changes are not yet complete.

## Simulation and GOOL

The hosted presentation path records the 30 Hz order `spawn → camera → GOOL → combined
world/object scene → presentation` plus draw-skip and draw-count timing. `RetailCameraRuntime` owns
the live path handle,
signed-8.8 progress and persistent follow offsets/zoom/speed. ZDAT entities decode into owned
descriptors and signed path points. A fixed 96-slot object pool, dedicated main-object slot, eight
logical roots, 304 spawn flags and generational handles reproduce the bounded spawn-tree shape
without host pointers.

The GOOL VM distinguishes external and shared/global code segments with checked `CodeAddress`
values. Logical code PCs, storage indices and entry slots are encoded with zero low bits and
validated independently from EIDs; animation references intentionally retain byte granularity. It
implements absolute global calls with typed frames and argument cleanup, returns,
optional/null pointer input semantics, scalar process operations, state-change yields and the
child-spawn host effect needed by the characterized Crash boot sequence. Its bounded 32-bit process
array is also the stack backing store: `init_sp`, frame-relative operands, object-register operands,
initial frames and global-call frames therefore observe the retail overlap without native pointers.
The complete validated state-descriptor table supplies target flags for guarded state links. The
ordinary runner stops at a synchronous host boundary; `run_with_host_effects` applies the callback
before the following instruction while preserving the same interpreter invocation. Retail
animation data is retained separately from code. Checked tagged animation references, packed and
operand-selected animation changes (`0x83`/`0x84`) use explicit frame/draw counters.
Checked tagged storage and global-code references replace pointer-producing opcodes `0x26` and
`0x18`. Paging opcode `0x8b` cases one through six use checked page/entry residency and reference
metadata without pretending that the browser has retail asynchronous paging. GOOL opcode `0x1a`
reads the complete five-word pad history. State rebind executes captured once code before the state
stamp and the target transition block after it, synchronously preserving nested calls, returns and
host effects. The implemented data-backed math includes `0x85` path orientation, `0x8e` entity
colors and the source-defined solid suboperation-one/three paths reached by N. Sanity. The NSF host
resolves a collidable object's current unaligned vertex animation/frame into a pair-scoped bound
source. Before each eligible object executes, the runtime registers its transformed bound in exact
preorder inside a 96-entry frame arena; solid helper queries consume typed `0xa300_0000` object
references, live status/size, source padding, first-hit/highest/tie ordering and parent sizing. A
second post-physics animation-stamp recomputation remains outside this slice.

`RetailRuntime` is the typed bridge between the arena and VM. It maps generational arena handles to
VM handles, scans displayed neighbor zones for group-three entities, binds initial GOOL programs
from NSD/NSF entries, executes the mutation-aware spawn tree, and synchronously binds runtime
children (including bounded `0x91` reclaim selection). A state-change halt is resolved through
`NsfProgramHost`; rebind, once and transition code complete at that same host boundary, while the
new state's ordinary code resumes on a later object execution. The browser creates this runtime
when a pair is mounted and runs it at 30 Hz in
gameplay, bonus, boss and ending flow states. The host initializes the characterized ZDAT
zone/path transform, rotation/mode flags, scale, colors and scalar process defaults without placing
native entity pointers in the register file; children inherit typed parent state. Any checked
execution failure quarantines that exact generational object identity, preventing a pre-incremented
program counter from resuming past an unsupported operation while healthy siblings continue.
MDAT/box special cases, `0x85` suboperations one through seven, solid suboperations
zero/two/four/five, event-service returns, most host effects, the late post-physics bound refresh,
complete zone lifetime/collision response and non-vertex object rendering remain outside this
bridge.

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
