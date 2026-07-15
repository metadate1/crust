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
5. The browser schedules mounted retail-object execution and presentation cooperatively at 30 Hz
   and drops excessive lag instead of replaying an unbounded catch-up burst. Its high-level flow
   value is a passive mount/presentation mirror; gameplay, title, completion, bonus, boss, intro and
   ending progression remain owned by mounted retail GOOL. Each tick publishes card and pad state,
   consumes any prior-frame level request, then performs spawn, camera, a frame-start texture-page
   snapshot, and GOOL in source order. A
   title tick continues through `TitleUpdate`, any synchronous `TitleLoadState`, and `GLUpdate`
   before the passive mirror observes the loaded screen. A one-frame swap latch preserves native's
   immediate opaque overlay even when the loaded GOOL requests another fade in that same update.
   A presented gameplay tick selects the exact `RetailCameraRuntime` zone/path/progress, freezes
   world texture/filter membership, executes GOOL, captures owned display records at each object's
   post-update/pre-child boundary in retail preorder, and atomically builds one pair-scoped
   world/object command stream.
   Automatic camera modes use pad taps; follow modes consume the hosted main object's typed
   transform, zoom, held pad and prior frame stamp. Camera LevelUpdate effects apply ordered
   zone/pager transitions before the following spawn scan. When authored display bit `0x10000`
   selects `CamDeath`, the runtime resolves global 36's live generation-paired object, its current
   vertex animation/frame and global 49's vertex through the mounted asset host, transforms that
   vertex in fixed point, and advances the six persistent death-camera words plus global 10's
   nine-frame alignment counter. The resulting non-path pose is shared by GOOL projection and
   WebGL while the ordinary path heading remains available to player physics. GOOL save/load effects capture and
   restore owned snapshots synchronously; cross-pair requests broadcast `LEVEL_END` before an
   asynchronous validated remount freezes the remaining tick. The remount carry retains native's
   process-lifetime texture/GOOL `draw_count`; the presentation mirror and first destination scene
   are seeded from that same counter, while the distinct `LevelRestart` path resets it to zero.
   During attract playback, the PBAK
   adapter installs its checked player/camera snapshot, RNG, bounds and spawn words, then replaces
   live controller input with each recorded 32-bit pad word and following-frame tick cadence.
   Full progression and the remaining SPU effects/voice-arbitration behavior remain later host
   boundaries.
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

The same builder consumes immutable, ordered `RetailDisplayRecord`s captured after each object's
update and before child traversal. It resolves unaligned renderable animation references through
the mounted pair's GOOL item five, accepts the 3D SVTX/CVTX vertex path, loads the exact TGEO/frame
variant, and applies retail object-local YXZ rotation, scale, lighting, face culling and depth. One
exact Pager slot snapshot freezes frame-start world requests and the source load-list filter. Before
each object is projected, its recorded live `(EID, generation, page)` map is replayed into the same
cache, preserving mid-frame paging changes without re-reading the final arena. Commands share one
texture manifest, and reversed object insertion is placed ahead of the already compensated world
stream to preserve the source's head-insert ordering-table behavior. On title level `0x19`, state 15 enables
a checked WGEO item-three path program. Its group cursor persists across ZDAT world order, globals
73 and 75 select groups 0–31 and 32–63, and frame-local polygon copies receive the effective
animation masks so mounted stream storage remains immutable. A graph-scoped sidecar retains the
last native-equivalent writes during map fade-out and is replaced at the zone/path or pair boundary.
Type-two sprites and type-five fragments
share the checked ZXY sprite matrix. Type-four text safely formats bounded negative-stack
arguments, resolves default or dynamic header-length-bounded type-three fonts (including
controller-icon records beyond the C declaration's first 63 slots), and emits ordered
glyph/backdrop quads; standalone type-three fonts remain resource-only. Status-B `0x200` CVTX uses
the same 2D matrix path. Retail sprite half-size calculation keeps the raw signed scale quotient,
masks MIPS variable shifts to five bits, and performs the source's wrapping 32-bit shift before the
checked GTE range/cull decision; it never applies a host-width shift or treats a saturating sprite as
a scene-wide fault. ZDAT object shader modes two and three feed source-specific SVTX light ramps or
CVTX shifts into that projection and can reject objects at their authored depth cutoffs. Mode four
is also live: renderer-BSS darkness state survives stream remounts, the player (or a checked live
pause object) is sampled at each object's post-update/pre-child display boundary, and the builder
passes that translation plus the pre-camera `dark_dist` value to the fixed-point evaluator. The
browser pause path materializes the native root-seven subtype-four controller, preserves its
paused-only update override and blink timing, and submits its five authored `WillT` fragment
quads. Modes two through four execute their native shader side effects at that same post-update,
pre-child boundary. The gate excludes the main object, display-mask `0x10000`, status-B `0x400`,
and near-plane failures unless `0x40000` overrides the latter; status-B `0x200` bypasses CVTX but
not SVTX. Derived colors are committed to the live VM before child execution and copied separately
into the render snapshot. A subsequent status-B `0x100000` zone-color reset changes the VM colors
seen by children without discarding those effective render colors; a null object-zone handle falls
back to the checked current zone.
For zones with graphics flag `0x1000`, a fixed Q24.8 camera with the 128-frame triangular Y bob and
fixed pitch is substituted only for object projection; the ordinary world camera is unchanged.
Simulation and WebGL use the same fixed-point matrix and GOOL `frames_elapsed` stamp, carried
separately from texture-animation `draw_count` while authored display frames are hidden.
Worlds whose ZDAT graphics flags include `0x100` take a separate source-faithful ripple path.
Before the world matrix, only WGEO vertices marked as effects add one of 16 wave magnitudes selected
by `((x + y) / 8) & 0xf`. The wave reconstructs `ShaderParamsUpdateRipple`'s signed seed, per-frame
advance, positive-period wrap and absolute-value publication; Upstream/Ripper Roo/Up the Creek use
speed 10 and period 127, Tawna bonus rooms one/two use speed 4 and period 127, and the default is
speed 1 and period 23. The builder owns pair-scoped mutable wave state. It advances only for an
unpaused submission containing visible ripple-world polygons. Pause or a world-hidden/empty
submission freezes it independently of texture `draw_count`; a later draw-skip presentation gate
still performs the source transform and advances the wave.

The remaining world modes share a process-lifetime `RetailLevelShaderState`. The browser advances
it after the native pause gate but before `CamUpdate`, snapshots its clear/effect channels and
Dark2 parameters for that frame, then re-reads the post-camera zone flags for rendering. This
preserves a transition frame where the old zone updates the globals and the new zone consumes
them. Dispatch priority is `Dark2`, combined Dark, Fog, Ripple, Lightning, then Plain. Lightning
selects clear/effect channels by each WGEO effect bit; combined Dark applies Lightning before its
fog pass; Dark2 uses projected depth, camera-space world translation, live doctor-or-Crash
illumination, and ambient/distance ramps. Renderer `far_color1` scratch is a separate
process-lifetime value because Dark2 intentionally retains the target written by an earlier
plain/fog/ripple/lightning/dark dispatch. Mount previews use disposable builder/scratch state;
actual hidden and visible frames both perform the transform so presentation skips cannot create or
erase shader/ripple steps.

Native RNG-B is owned with that process state but reconciled at every browser host boundary that
can allocate audio. Source order is therefore explicit: pause/spawn handlers, pre-camera shader and
optional thunder voice, GOOL audio, `LEVEL_END`, `PbakChoose`, and destination import all observe
one zero-initialized 32-bit stream. A thunder cue carries only checked ADIO EID/pitch/delay/volume
values; the audio crate resolves local PCM and creates an ownerless delayed-key voice.

`GlStage` can transactionally update an installed retail scene. It validates command texture
references, compares immutable decoded-texture allocation identities, prepares all new/replacement
GPU textures before committing, removes stale handles, and has a command-only fast path when the
pair-scoped cache returns the same allocation. A distinct allocation is conservatively uploaded
even if its bytes happen to match, avoiding per-frame pixel-vector clones and byte scans. Automatic
`CamUpdate` and live main-object `CamFollow` path/zone changes select the canvas scene; pause freezes
the last successfully installed snapshot. Three-dimensional vertex object models and their current
animation transforms, screen-aligned sprites/fragments, text/font quads and 2D CVTX objects are
reflected in that snapshot. World visibility retains the global-nine mask sampled before GOOL,
while every object carries the potentially different live mask, animation, transform, process,
text-argument/font, effective-color and texture-slot state consumed at its post-update display
boundary. A child that writes through an authored parent link or later teardown therefore cannot
retroactively alter or retract its parent's already-rendered state. Same-slot `A → B → A` paging
keeps decoded A regions frozen, decodes uncached regions from the currently recorded mapping, and
reuses A's frozen internal generation when that EID returns to its exact slot. The final title pass
applies the native 16-band nonlinear black-overlay alpha after the source counter step. A healthy
authored arena owns the 4:3 canvas. Until one is available, the browser shows only loading/error
diagnostics and keeps state/warning text in the external monitor panel; it does not substitute a
synthetic title, menu or gameplay scene.

## Simulation and GOOL

The hosted presentation path records the 30 Hz order `card/pad → pending level transition → spawn
→ camera → frame-start texture snapshot → GOOL plus ordered display-record capture → combined
world/object scene → presentation` plus draw-skip and draw-count
timing. Title frames insert the authoritative `TitleUpdate → TitleLoadState → GLUpdate` boundary
after GOOL; the high-level `GameFlow` value only mirrors the screen loaded there. `RetailCameraRuntime`
owns the live path handle, signed-8.8 progress and persistent follow offsets/zoom/speed. Its follow
input reads GOOL global 65 directly for the source `frames_elapsed - gem_stamp <= 15` neighbor gate.
ZDAT entities decode into owned
descriptors and signed path points. A fixed 96-slot object pool, dedicated main-object slot, eight
logical roots, 304 active spawn words and generational handles reproduce the bounded spawn-tree
shape without host pointers. A separate 3,592-halfword process-lifetime registry retains
encountered `(level, object)` tags. Misc-ten selectors four/five maintain its zero-terminated,
one-as-hole representation and fall through to the corresponding active-table bit update; a new
pair clears the active table and derives bit eight only from tags for that destination level.

Pointer-valued GOOL words retain their exact 32-bit tags, while a sidecar records the native
physical pool slot independently from compact VM identity. Globals capture provenance when written;
rewriting the same raw tag advances its write epoch and captures the current slot. Before retail
reclaim, the machine captures provenance for every pre-existing pointer-shaped process link,
register, stack word, and internal/external data-table word that does not already have an older
identity. MOV, stack, and linked-register copies carry that sidecar. Existing provenance wins when
the same compact tag has already become a dangling pointer, so compact-handle reuse in another slot
cannot silently retarget it.

The 96 ordinary slots begin as native's ascending `free_objects` chain. Binding preflights handle,
slot, occupancy, and free-list membership before installation is committed; unlinking an arbitrary
slot reconnects its predecessor. Reclaim captures process storage, overwrites the freed object's
parent with a distinct checked `&free_objects` tag, points its sibling at the previous free-list
head, clears its child link, and pushes the slot at the head. The separately allocated main slot is
never inserted into that ordinary chain and clears its three intrusive-tree links on release.
Linked process-register reads and writes select the current occupant when the slot is live and the
retired register array when it is free; nested pointer writes retain their own provenance, and a
later occupant of the same physical slot source-faithfully retargets the old link. Translation
writes also update the retained translation view used by hosted lighting.

A replacement first inherits the slot's retained process array, then `GoolObjectInit` applies its
selective in-place writes. Raw `sp`, `pc`, `fp`, `tp`, and `ep` words and the other source-listed
fields reset; process words not written by initialization remain intact. The Dark2 `doctor`, Jaws
of Darkness `fruit_hud`/creator chain, and Dr. N. Brio `BoxsC` creator links all use this same model.
No stale Rust reference is kept or dereferenced. Writes that would mutate an ordinary free slot's
three allocator-owned link words are rejected transactionally instead of reproducing native
malformed-list behavior. Event-service argv scopes, mapped-state changes, direct/broadcast send
requests, and child-spawn argument vectors copy a parallel physical-slot sidecar. Sidecars are
validated before mutation, survive EARG/stack copies, and are applied to a child only after its
reused slot storage is seeded. Still outside this slice are address-taking through a free-slot
link, retained non-process colors/bounds/animation, and byte-exact dedicated-main allocation and
reinitialization behavior.

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
operand-selected animation changes use explicit frame/draw counters. Host effects `0x83` and
`0x84` synchronously refresh the persistent local animation bound before the interpreter proceeds;
`0x83` honors the status-B `0x18` gate plus its range/force condition, while `0x84` is unconditional.
Checked tagged storage and global-code references replace pointer-producing opcodes `0x26` and
`0x18`. Paging opcode `0x8b` cases one/six open, case two closes, and case three probes through a
typed synchronous host request before the interpreter continues; cases four/five inspect VM-local
availability/resolution. Unavailable opens roll back their optimistic reference and resolution,
successful resident replacements re-arm the evicted page, and inconsistent EID/page responses are
rejected. This models source ordering without claiming retail asynchronous I/O timing. GOOL opcode `0x1a`
reads the complete five-word pad history. State rebind executes captured once code before the state
stamp and the target transition block after it, synchronously preserving nested calls, returns and
host effects. The complete `0x85` selector family covers path orientation, perspective, velocity
aiming, source no-op case three, scaled/unscaled object transforms, checked model-vertex lookup and
the camera-relative audio transform. The complete `0x8e` family covers static/object solid
response, all directional surface variants, entity-color scaling and source no-op case seven.
Opcode `0x14` (LEA) translates its input address before its output address, preserves null and stack
side effects, and stores a checked process-local handle rather than a pointer. Same-object internal
and register aliases are decoded from their live words on each VM read, then copied as a fully
owned, bounded descriptor into the display snapshot. Type one carries its model EID into the
pair-scoped vertex/bound resolver; types two, four, and five use the existing sprite, text, and
fragment paths; type three consumes its header but remains a resource-only no-draw selection.
Process text retains local NUL-delimited terms while resolving its font word offset against global
item five. Type zero and unknown bytes follow the native transform-switch default with no draw and
the standard non-vertex bound, including the observed Toxic Waste `BaraC` use. A foreign-object
storage reference, an external-state-table alias, and the VM's rotating constant region are rejected
because their backing identity/lifetimes cannot yet be represented without retargeting the native
pointer. Opcode `0x81` follows the native switch's missing-case behavior as an intentional one-cycle
no-op.

The standalone VM retains a bounded effect buffer for checked host handshakes. A retail frame drains
that buffer after each visited object's update and display boundary, before descending to children,
into the frame-owned ordered observation list. Every synchronous event recipient also starts a new
bounded effect transaction, including nested broadcast recipients, while the frame observation list
retains the exact traversal order. This preserves native preorder chronology and prevents either a
deep 96-object subtree or a wide broadcast from falsely exhausting a whole-frame VM queue.

A `RETURN` with no call frame remains a normal halt for synthetic `VmObject::new` fixtures. A
program parsed from retail GOOL instead reports `InvalidInitialReturn`, matching the zero saved
frame pointer in the native initial frame. The preorder host consumes that signal before display or
child traversal and releases the complete subtree through the no-signal kill path: no TERM handler
runs, audio ownership is cleared, and retail process links follow the pool teardown above while the
dedicated main object retains its non-Title protection. This distinction prevents state-level
returns such as Ending's credits children from parking indefinitely while preserving the small
synthetic-VM API's historical halt contract.
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
mutations. The NSF host resolves a collidable object's current unaligned vertex animation/frame
into a pair-scoped bound source. The 96-entry frame arena follows the native Crash-stamp schedule:
objects already carrying Crash's register-frame stamp bind before GOOL and physics, while objects
visited before Crash receive a late post-physics registration only within the inclusive
`±0x7d000` X/Z and `±0xaf000` Y window. Rejected late objects set status-A invalid bit `0x8000`.
The same-stamp `GoolObjectBound` tail then applies Crash's asymmetric object-collision bookkeeping,
including accepted/priority collider links, hotspot `0x1000`, and target-collider clearing on a
miss. Solid helper queries consume typed `0xa300_0000` references, live status/size, source padding,
first-hit/highest/tie ordering and parent sizing.

Stopped-by-solid movement also preserves `PlotObjWalls`' two source modes. A flag-zero replot may
add side geometry but cannot mutate collision state. A flag-one pass walks animation-derived frame
bounds in registration order and runs the shared checked `GoolCollide` resolver for every broad
object-bound overlap, even when the candidate did not contribute a side wall. The mover's collider
changes immediately for later candidates and motion phases; reciprocal candidate links and hotspot
bits retain the same ordered effect prefix before any nested synchronous handler. This is what
allows Box7 and CrabC to observe their native next-traversal collision links without a C pointer
compatibility layer.

The mover's current collider is a validated snapshot of all live process fields read by solid
motion, not a lookup into the bounded candidate array. It remains authoritative when omitted from
that frame's candidates and is re-resolved after a synchronous handler replaces link six. Object
hotspot insets retain raw endpoint ordering as a separate bound type; an inset may reverse an axis,
and collision/wall code applies the source's direct face comparisons without constructing a
normalized `Bounds3`.

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

Selection itself mirrors `PbakChoose`: trailing-`B` type-19 names are counted from the NSD page
table, one RNG-B draw chooses `pb` + index + destination-level character + `B`, and count zero does
not advance. The seed mutation enters `RetailSessionCarry` before destination construction;
`PbakStart` later seeds only gameplay RNG-A. The browser's absolute shader clock continues through
asynchronous pair validation just as native `GetTicksElapsed` continues through synchronous
`NSKill`/`NSInit`, but authored pause excludes its full interval. An armed recording suppresses
scene replacement and retains the prior loading/scene framebuffer until Crash starts playback.

`RetailRuntime` is the typed bridge between the arena and VM. It maps generational arena handles to
VM handles, scans displayed neighbor zones for group-three entities, binds initial GOOL programs
from NSD/NSF entries, executes the mutation-aware spawn tree, and synchronously binds runtime
children (including bounded `0x91` reclaim selection). A state-change halt is resolved through the
stream host; rebind, once and transition code complete at that same host boundary, and normal code
continues in the same native update. Paging, event services, interrupts and audio calls use the
same typed synchronous request boundary. The browser's paging host validates requested EID/page
identity against its Pager, returns any resident eviction to the VM, and leaves an unavailable open
unapplied. The browser creates this runtime
when a pair is mounted and runs it at 30 Hz in title, gameplay, bonus, boss, level-complete, intro
and ending flow states. The host initializes the characterized ZDAT
zone/path transform, rotation/mode flags, scale, colors and scalar process defaults without placing
native entity pointers in the register file. Type-17 title entities retain their MDAT descriptor
provenance, while native `cur_zone` supplies the arena object zone, origin and colors; children
inherit typed parent state. Any checked
execution failure quarantines that exact generational object identity, preventing a pre-incremented
program counter from resuming past an unsupported operation while healthy siblings continue.
Box special cases, some host effects, full progression and several dynamic object-rendering modes
remain outside this bridge.

After the mount-time life/fruit/pickup roots, the bridge applies the object-creating
`LevelInitMisc(1)` branches under logical root four: level `0x05` uses executable 9/subtype 4,
`0x14` and `0x16` use 23/6, Ripper Roo (`0x17`) uses 39/4, and `0x22`/`0x2e` use 53/13. The Ripper
controller is also published as a generation-checked tagged reference in `ambiance_obj` (global 8).
Other levels create no controller; same-level `LevelInitMisc(0)` does not duplicate one.

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
All five bonus spawn zones carry the source save-restricted flag. A normal session mount retains
the parent-level snapshot for authored `-2` return. A fresh direct boot has no native parent
snapshot, so only that advertised host entry path arms a one-shot same-level restart snapshot for
death recovery; session-carried bonus entry never enables the fallback. Completing a directly
booted bonus with that synthetic snapshot still needs an explicit host return policy and is not
claimed as a complete bonus round trip. `set_level_state_context` publishes the current zone's
graphics flags to GOOL global 30 at initial mount, zone changes, remounts, and hard restarts before
the next spawn/update pass. Legal bonus zones publish `0x2002`; WillC's WARP state tests bit
`0x2000` before selecting its LoadState return path. Different-level `LoadState` preserves the
native source-frame tail: it emits the ordered browser remount handshake without stopping the
current interpreter, later preorder objects, or the display latch. Its synchronous host boundary
clears bonus global 60 before that continuation and annotates the effect with the then-current saved
level. A later SaveState may change the eventual `-2` destination, but cannot retroactively turn
the earlier request into a same-level structural restart. Same-level `LoadState` remains a checked
stop because its deferred restart structurally replaces the active object forest.

The Tawna entry and return tests preserve the native transaction boundaries rather than inventing
a host shortcut. Three authentic Jungle Rollers token crates are started at their authored player
`HIT` handler; `BoxsC` spawns subtype-13 `FruiC`, `FruiC` routes the token and entity ID to `DispC`,
and the third token makes `DispC` save the parent `0x0c` state before Crash's counter increment.
The HUD later sends completion event `0x2700`, resets the master fade, sends status
`0x0f00 [0x500]`, and emits destination `0x24`; the different-level `LEVEL_END` carry retains that
snapshot. In Tawna Bonus, the parsed WarpC transition accepts only when portal status bit `0x20` is
clear, the signed quantized X/Z Euclidean distance is below `0x28000`, the Y delta is in
`[-0x20800, 0)`, and Crash is grounded without the atop-object bit `0x200000`. Acceptance sends
direct event `0x1600 [0]` to WillC state 32, after which the CardC/`LoadState` path performs the
protected `-2` return. The regressions fix objects and positions at these exact program boundaries;
they do not steer one uninterrupted browser route.

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
also follows the exact signed 25-tick `MidiResetFadeStep` ramp. Each sampled VAB voice decodes its
two ADSR register words into a fixed-point Q15 generator advanced once per 44.1 kHz sample. Attack,
decay, sustain, release, linear/exponential modes, slowdown strictly above the `0x6000` attack
threshold, rate counters,
all-one frozen rates, key-on/off, and phase targets follow the hardware integer rules; conversion to
floating-point happens only at the final mix gain. Remaining gaps include Gaussian interpolation,
SPU reverb/effects, noise and FM/modulation, vibrato/portamento, pressure and unsupported generic
controllers, and hardware-equivalent priority across one shared 24-voice SFX/music pool. The music
sequencer currently owns a separate bounded software-voice pool. There is no procedural sine
fallback: browser sound comes only from mounted ADIO SFX and the mounted retail music synthesizer.

## Persistence

The platform crate parses and emits the existing JSON schema rather than inventing a new save.
Manual-card slots preserve damaged opaque records instead of silently formatting them. Invalid
automatic resume records are rejected so the browser host can move them to a timestamped quarantine
key. Newer schema versions are left untouched. The simulation-side card restore does not clear
savestate or active spawn words: those are not part of the 128-byte payload and native
`LevelResetGlobals(1)` does not own them. The 3,592-halfword encounter registry is reset, as it is
owned by that native transaction. A virtual-card rescan stages all fifteen physical slots without
publishing them, keeps `CHECKING` visible until authored CardC clears `FLAG_6`, then publishes parts
and count on the following cooperative update. The browser persistence merge preserves timestamps
for unchanged slots and refreshes only the slot or format operation that actually changed.
