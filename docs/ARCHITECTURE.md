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
crust-formats ───── crust-sim ── crust-audio ──┐
crust-renderer ────────────────────────────────┤
crust-platform ────────────────────────────────┤
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
   atomic retained-pair swap; simulation is stalled while the asynchronous read is in flight. A
   mounted type-19 PBAK entry is separately validated as one of the two observed pointer-free
   layouts before its snapshot or pad frames can reach simulation.
5. The browser schedules flow, retail-object execution and presentation cooperatively at 30 Hz and
   drops excessive lag instead of replaying an unbounded catch-up burst. Each tick publishes card
   and pad state, consumes any prior-frame level request, then performs spawn, camera and GOOL in
   source order. A presented gameplay tick selects the exact `RetailCameraRuntime`
   zone/path/progress, executes GOOL, snapshots the live
   arena in retail preorder, and atomically builds one pair-scoped world/object command stream.
   Automatic camera modes use pad taps; follow modes consume the hosted main object's typed
   transform, zoom, held pad and prior frame stamp. Camera LevelUpdate effects apply ordered
   zone/pager transitions before the following spawn scan. GOOL save/load effects capture and
   restore owned snapshots synchronously; cross-pair requests broadcast `LEVEL_END` before an
   asynchronous validated remount freezes the remaining tick. During attract playback, the PBAK
   adapter installs its checked player/camera snapshot, RNG, bounds and spawn words, then replaces
   live controller input with each recorded 32-bit pad word and following-frame tick cadence.
   Full progression and exact SPU synthesis remain later host boundaries.
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
through validated EID lookups. The title host keeps the MDAT EID as descriptor provenance while
binding each spawned type-17 object's zone/origin/colors to native `cur_zone`. Type-zero screen
loads preserve the `0x3ff0` category tail in display mask `0x22_3ff0`; the following authored
start/blank tick adds only display/animate (`0x0c`). A pointer-free active-scene builder starts from
LDAT and can then resolve any validated ZDAT zone/path selected by the camera. It initializes a
checked `SlstCursor`, parses each referenced WGEO, resolves TPAG/CLUT texture regions, applies
draw-count texture animation plus the exact fixed-point world camera and depth ordering, and emits
ordinary renderer commands.
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
preserve the source's head-insert ordering-table behavior. Type-two sprites and type-five fragments
share the checked ZXY sprite matrix. Type-four text safely formats bounded negative-stack
arguments, resolves default or dynamic fixed-63 type-three fonts, and emits ordered
glyph/backdrop quads; standalone type-three fonts remain resource-only. Status-B `0x200` CVTX uses
the same 2D matrix path. Retail sprite half-size calculation keeps the raw signed scale quotient,
masks MIPS variable shifts to five bits, and performs the source's wrapping 32-bit shift before the
checked GTE range/cull decision; it never applies a host-width shift or treats a saturating sprite as
a scene-wide fault. ZDAT object shader modes two and three feed source-specific SVTX light
ramps or CVTX shifts into that projection and can reject objects at their authored depth cutoffs.
The pure mode-four evaluator is also checked and tested, but the live builder does not invoke it
until render snapshots carry the current pause-object/player selection and `dark_dist`. For zones
with graphics flag `0x1000`, a fixed Q24.8 camera with the 128-frame triangular Y bob and fixed
pitch is substituted only for object projection; the ordinary world camera is unchanged.

`GlStage` can transactionally update an installed retail scene. It validates command texture
references, compares immutable decoded-texture allocation identities, prepares all new/replacement
GPU textures before committing, removes stale handles, and has a command-only fast path when the
pair-scoped cache returns the same allocation. A distinct allocation is conservatively uploaded
even if its bytes happen to match, avoiding per-frame pixel-vector clones and byte scans. Automatic
`CamUpdate` and live main-object `CamFollow` path/zone changes select the canvas scene; pause freezes
the last successfully installed snapshot. Three-dimensional vertex object models and their current
animation transforms, screen-aligned sprites/fragments, text/font quads and 2D CVTX objects are
reflected in that snapshot. Ordered zone teardown and dynamic display masks affect post-GOOL
snapshots; mid-frame paging-driven texture replacement remains incomplete. The final title pass
applies the native 16-band nonlinear black-overlay alpha after the source counter step. When a
healthy authored arena is presenting, the browser host hides fallback menus and diagnostic overlays
from the 4:3 canvas while keeping state/warning text in the external monitor panel.

## Simulation and GOOL

The hosted presentation path records the 30 Hz order `card/pad → pending level transition → spawn
→ camera → GOOL → combined world/object scene → presentation` plus draw-skip and draw-count
timing. `RetailCameraRuntime` owns the live path handle, signed-8.8 progress and persistent follow
offsets/zoom/speed. ZDAT entities decode into owned
descriptors and signed path points. A fixed 96-slot object pool, dedicated main-object slot, eight
logical roots, 304 active spawn words and generational handles reproduce the bounded spawn-tree
shape without host pointers. A separate 3,592-halfword process-lifetime registry retains
encountered `(level, object)` tags. Misc-ten selectors four/five maintain its zero-terminated,
one-as-hole representation and fall through to the corresponding active-table bit update; a new
pair clears the active table and derives bit eight only from tags for that destination level.

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
host effects. The complete `0x85` selector family covers path orientation, perspective, velocity
aiming, source no-op case three, scaled/unscaled object transforms, checked model-vertex lookup and
the camera-relative audio transform. The complete `0x8e` family covers static/object solid
response, all directional surface variants, entity-color scaling and source no-op case seven.
`SZON` uses a typed host effect: it scans the current ZDAT header's neighbors in reverse serialized
order, tests inclusive Q24.8 rectangles with explicit wrapping arithmetic, and changes the linked
object's zone only when a match exists. Misc 12/7 deliberately uses the other header order: it
visits the forward serialized neighbor list without display filtering or deduplication. For each
EID, roots zero through seven are traversed in mutation-aware postorder; existing TERM immunity,
migration and non-title Crash rules apply through persistent typed `ObjectZoneContext` runtime
state, which carries either a transition target or hard-restart sentinel. Object audio and typed
tree/link ownership are cleaned synchronously with teardown. Arena spawn flags are authoritative
until their VM mirror is refreshed at the next frame boundary. A null current zone is a no-op;
duplicate EIDs remain in the walk, and each later EID rescans the tree after earlier handler
mutations. The NSF host resolves a collidable object's current unaligned
vertex animation/frame into a pair-scoped bound
source. Before each eligible object executes, the runtime registers its transformed bound in exact
preorder inside a 96-entry frame arena; solid helper queries consume typed `0xa300_0000` object
references, live status/size, source padding, first-hit/highest/tie ordering and parent sizing. A
second post-physics animation-stamp recomputation remains outside this slice.

Solid execution deliberately owns two environments. The current-camera/native `cur_zone`
environment supplies only its current-neighbor octrees to geometry queries and refreshes at each
camera-zone change. Each VM object separately retains its typed object-zone EID and object-zone
colors. If that EID becomes detached from the current neighborhood, its rectangle, graphics and
water plane remain available for the source ceiling/zone fallback without silently adding the
detached octree to geometry candidates. `StopAtZone` can move the typed object-zone identity when a
current neighbor contains the object.

The machine also owns the source process globals used by this pass. Smooth-stop history is shared
by interleaved objects, and the pointer-free `cur_zone_query` equivalent persists across objects,
frames and current-zone replacement until its strict collider bound escapes or `LevelInitMisc`
invalidates it. Collision-generated ceiling, outside-zone, water and surface events yield from the
solver at the native call sites. Before each nested GOOL handler, the runtime publishes the full
pre-physics state plus the ordered collision-effect prefix; afterward it refreshes handler
mutations before continuing the same pull pass.

The PBAK runtime adapter accepts the ordinary 304-spawn-word snapshot and the one observed
511-word layout only when its discarded tail is zero. It remaps serialized X/Y/Z rotation into the
VM's Y/X/Z register order, restores camera/path/progress and Crash state, and preserves the recorded
RNG, draw stamp, bounds and timing. `DemoPlayer` makes the final recorded pad word observable before
the finished/physical-interrupt handoff. Before the restart, the runtime creates the exact
executable-four/subtype-eight controller under root one with its two caption arguments. Its typed
null lifecycle zone survives zone termination while environment lookups use the checked current
ZDAT fallback. Children retain that null lifecycle identity too, but binding and process-color
initialization use the current camera ZDAT exactly where native `GoolObjectCreate` falls back to
`cur_zone`; no `EID_NONE` asset lookup is attempted. At completion, a tagged live `caption_obj`
handle receives synchronous event
`0xE00` when `island_cam_rot_x` is nonzero; otherwise playback simply releases physical input. The
browser advances pad/PBAK state through a typed traversal hook immediately before the live Crash
object under root six. Earlier root-one caption work has completed; the completion event/rebind,
state-three latch and final pad history are then visible to Crash and every later root in the same
frame. Because `PbakStart` follows `GLUpdate`, the start frame keeps ordinary wall timing through
root one and Crash retains the wall tick count while installing the recording header's TPF. On
later recorded frames, the preceding `GLUpdate` exposes `(ticks_cur_frame, ticks_per_frame) ==
(17, recorded TPF)` throughout the traversal; returning frames expose `(17, rounded wall TPF)`.
The non-advancing timing peek prevents the Crash hook from consuming a frame twice. If later
traversal fails after the synchronous caption event, its captured effect slice is recovered exactly
once because no completed `RuntimeFrame` exists to carry it.

`RetailRuntime` is the typed bridge between the arena and VM. It maps generational arena handles to
VM handles, scans displayed neighbor zones for group-three entities, binds initial GOOL programs
from NSD/NSF entries, executes the mutation-aware spawn tree, and synchronously binds runtime
children (including bounded `0x91` reclaim selection). A state-change halt is resolved through the
stream host; rebind, once and transition code complete at that same host boundary, and normal code
continues in the same native update. Event services, interrupts and audio calls use the same typed
synchronous request boundary. The browser creates this runtime
when a pair is mounted and runs it at 30 Hz in title, gameplay, bonus, boss, level-complete, intro
and ending flow states. The host initializes the characterized ZDAT
zone/path transform, rotation/mode flags, scale, colors and scalar process defaults without placing
native entity pointers in the register file. Type-17 title entities retain their MDAT descriptor
provenance, while native `cur_zone` supplies the arena object zone, origin and colors; children
inherit typed parent state. Any checked
execution failure quarantines that exact generational object identity, preventing a pre-incremented
program counter from resuming past an unsupported operation while healthy siblings continue.
Box special cases, some host effects, the late post-physics bound refresh, full progression and
several dynamic object-rendering modes remain outside this bridge.

`LevelResetGlobals(1)` is a preflighted synchronous transaction over the source's documented
scalar words and the encountered-object registry. It deliberately preserves live objects, the
retained `level_state` savestate and the separate 304-word active spawn table. `CardRestorePayload`
first installs `init_life_count`, performs that exact reset, then restores only the payload's
progression/options words and their derived map/unlock mirrors. The main-menu automatic-resume
bridge applies the same protected before/reset/after ordering. `LevelInitMisc(0)` separately resets
the representable per-level latches, including box count and solid-smoothing memory, during the
same-level restart path. GOOL misc 12/11 applies the global reset synchronously before the next
instruction, so a `SaveState` later in the same handler observes checkpoint `-1` rather than a
stale browser mirror.

The different-level `LEVEL_END` path carries scalar state into a freshly mounted runtime. A
same-level `LoadState` issued from inside `LEVEL_END` remains a checked resumable-host boundary:
continuing the interrupted handler needs a nested restart continuation that is not yet represented.
A legally local scan of all 44 retail pairs found zero authored occurrences of that nested case.

## Audio

The audio library is deterministic signed stereo at 44.1 kHz. Sixteen-byte SPU ADPCM blocks decode
to 28 samples with saturated predictor history and loop-marker semantics. A bounded sample cache
feeds the retail 24-slot voice allocator, including template controls, stealing, delay/rekey,
ramp/glide and owner-wide teardown. The browser program host lazily resolves local type-12 ADIO item
zero, returns the exact synchronous voice result to GOOL, ticks voices at 30 Hz and merges their PCM
into WebAudio. The output unlocks only after a user gesture; mute does not stop simulation, and
SFX/music volume plus mono are independent. Zone MIDI EIDs resolve checked type-13/type-14 entries;
VAB waves become owned PCM sample banks and SEP events drive two independent sequencers. A
browser-independent owner applies source-timed thirty-tick zone fades, defers transitions while
GOOL selects the second track, and drops both banks at a level boundary. The WebAudio master gain
also follows the exact signed 25-tick `MidiResetFadeStep` ramp. Exact SPU ADSR,
vibrato/portamento, generic controllers and reverb remain future work.

## Persistence

The platform crate parses and emits the existing JSON schema rather than inventing a new save.
Manual-card slots preserve damaged opaque records instead of silently formatting them. Invalid
automatic resume records are rejected so the browser host can move them to a timestamped quarantine
key. Newer schema versions are left untouched. The simulation-side card restore does not clear
savestate or active spawn words: those are not part of the 128-byte payload and native
`LevelResetGlobals(1)` does not own them. The 3,592-halfword encounter registry is reset, as it is
owned by that native transaction.
