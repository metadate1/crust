# Compatibility and known limits

`crust` is a runnable Rust/Wasm interoperability implementation. It is not a complete replacement
for the retail executable and does not claim full-playthrough parity. The distinction below is
intentional: a subsystem can be parsed and unit-tested without being connected to the live browser
gameplay path.

## What is connected in the browser

- Local raw NTSC-U Mode 2/2352 BIN and cooked ISO discovery through bounded Blob range reads.
- Local extracted NSD/NSF selection; 88 S0–S3 files form all 44 known pairs without upload.
- Bounds-checked validation of the selected pair's NSD, every NSF page, and every entry before boot,
  followed by the same validation and an atomic pair swap at each destination transition.
- Retail 432×144 LDAT loading-image decode and GPU upload where the stream provides one; the live
  stage consumes `crust-renderer` ordering-table commands through the WebGL2 backend.
- Retail image-backed publisher/Naughty Dog/main-menu title frames composed from validated
  MDAT/IPAL/IMAG graphs and presented by the live WebGL2 stage.
- Retail world snapshots for the 40 world-bearing playable LDAT starts: ZDAT spawn-zone/path, checked
  stateful SLST visibility, WGEO packed vertices/polygons, TPAG/CLUT texture decode and animation,
  fixed-point camera projection and retail world ordering depth are connected to WebGL2. Streams
  with a loading image use the observed tick-two path point/draw count before gameplay is shown.
  Subsequent frames use the validated camera graph's exact zone/path/signed-8.8 progress and the
  retail-frame pre-increment texture-animation count. Paused frames retain the camera and shader
  state but continue rebuilding and presenting world/object snapshots while draw count stays fixed.
- Source-derived camera modes 0/1/3, tapped auto-camera skipping and path/zone crossing drive the
  live scene. Modes 5/6 consume the hosted main object's transform, camera zoom, held input and
  prior frame stamp through checked `CamFollow` projection, neighbor selection and smoothing.
- Displayed current-zone neighbors are decoded into owned ZDAT entity descriptors when a pair is
  mounted. In title, gameplay, bonus, boss, level-complete, intro and ending flow states, the
  browser spawns their group-three entities into the checked retail arena and executes that arena
  at the cooperative 30 Hz boundary. Before the first zone scan, every gameplay, boss, bonus, and
  map mount creates native executable-four life, fruit, and pickup roots beneath logical root one
  and publishes their checked references to globals 7, 6, and 14. The four native exclusions—title,
  level complete, intro, and ending—create none.
- The NSF program host binds initial and requested GOOL states, synchronously applies characterized
  child-spawn effects, maintains typed arena/VM links, and advances the implemented state-change
  and animation-select/wait path using frame/draw counters. Animation host effects `0x83` and `0x84`
  synchronously refresh the object's persistent local bound before execution continues; `0x83`
  applies the status-B `0x18` gate and native range/force test, while `0x84` is unconditional.
  Initial and global-call frames share the bounded process/register array at the parsed `init_sp`,
  and state links consult the validated target descriptor flags. Rebind runs captured once and
  target transition blocks synchronously, including nested calls/returns and hosted spawns. Checked
  aligned code/storage/entry references,
  paging cases one through six, every `0x85` transform-vector selector and every `0x8e` solid/color
  selector are active, including their source-defined no-op cases. `SZON` performs the exact reverse
  current-header neighbor scan with inclusive wrapped Q24.8 rectangles and updates the linked
  object's zone only on a match. Normal, once, transition, event-service and interrupt code all
  complete typed audio calls synchronously before the following instruction. Execution,
  checked-error and quarantined-object counts are exposed through the engineering log/debug
  surface.
- Static solid queries are refreshed from the current camera/native `cur_zone` neighborhood before
  object execution rather than remaining attached to each object's spawn zone. The per-object zone
  identity remains separately typed for object colors and zone migration. When that object zone is
  detached, its rectangle, graphics and water plane supply the source ceiling/zone fallback without
  adding its octree to current-neighbor geometry candidates. This separation carries the
  characterized N. Sanity route from `e0_9Z` through `a0_9Z`, `a1_9Z`–`a9_9Z`, `b0_9Z`, and into
  `b1_9Z` without a
  stale-zone fall. The strict Hog Wild window advances `0a_hZ → 0b_hZ → 0c_hZ`, preserves Crash's
  detached object-zone identity as `0c_hZ`, and repeats that camera path across the authored
  same-level restarts at frames 179 and 356.
- Authored title MDAT entities bind to the same arena/VM host as level objects. Their retail
  `title_state` word drives publisher, menu, options, password/load, map and game-over changes
  through the native fade boundary. The final WebGL pass uses the source's 16-level nonlinear
  black-overlay alpha table and exact pre-quantization counter step; blank and state-swap phases
  stay opaque without affecting gameplay rendering. Type-zero loads preserve the source's `0x3ff0`
  object-category tail (`0x22_3ff0`) and the following start/blank tick enables only the display and
  animate bits (`0x22_3ffc`). Each screen swap tears down old objects and performs the source
  flag-two `LevelUpdate` before spawning the next image-backed MDAT entities. The MDAT EID remains
  descriptor provenance, while each spawned object's zone, origin and colors come from the current
  ZDAT exactly like the source's type-17 rewrite, keeping it in current-neighbor TERM scope. An
  authored arena owns the 4:3 canvas. Before it is available the browser presents only loading/error
  diagnostics and external status; it does not advance a synthetic title, menu or gameplay flow.
- Camera `LevelUpdate` effects drive a source-ordered zone lifecycle. Departed active zones receive
  TERM in postorder, migrated objects survive, released subtrees clear typed VM/link/audio state,
  old load lists close before new lists open, newly adjacent zones activate, and their objects scan
  on the following cooperative frame. Display and animation masks latch with retail's one-frame
  timing.
- Misc 12/7 synchronously visits the current header's forward neighbor list without filtering or
  deduplication. Each neighbor uses the live eight-root postorder TERM traversal, including immunity,
  migration, non-title Crash survival and persistent typed `ObjectZoneContext` target/sentinel
  semantics. Object audio and typed tree/link ownership are cleaned synchronously; arena spawn
  flags remain authoritative until their VM mirror is refreshed at the next frame boundary. A null
  current zone is a no-op; duplicate EIDs are retained and later entries rescan the mutated live
  tree.
- Pair-scoped GOOL item-five descriptors and TGEO/SVTX/CVTX frames feed the live renderer. Current
  3D vertex objects use retail fixed-point transforms, lighting, culling and ordering and share one
  resident load-list-filtered TPAG cache/manifest with the world. Collidable animations populate
  the 96-entry frame arena on the native Crash-stamp schedule: matching-stamp objects register before
  GOOL/physics, while objects visited before Crash register after physics only inside the inclusive
  `±0x7d000` X/Z and `±0xaf000` Y window; rejected late objects set status-A invalid bit `0x8000`.
  The same-stamp tail also applies Crash's asymmetric accepted/priority collider links, hotspot
  `0x1000`, and target-collider clearing on a miss.
  Type-two sprites, type-five fragments and status-B 2D CVTX use the source ZXY sprite transform.
  Their signed half-size calculation preserves the source MIPS `SLLV`/`SRAV` low-five-bit shift
  semantics and wrapping 32-bit intermediate before checked GTE range rejection. The legal
  Jungle Rollers playback covers the `FruiC` raw-shift sequence 24, 26, 28, 31, 34 … 297 without a
  renderer halt; saturated results are culled at the same validity boundary rather than clamped by
  host shift rules.
  Type-four text uses bounded `sp[-2]` argument aliases, the default or dynamic fixed-63 type-three
  font, retail formatting/control commands, per-corner color modulation and ordered glyph/backdrop
  quads. Standalone type-three font descriptors remain resource-only, as in the source. ZDAT object
  shader modes two and three are connected with their separate SVTX/CVTX ramps and far-object
  cutoffs. Mode four consumes a source-order player translation (or a checked live pause-object
  translation) and the Lights Out/Fumbling `dark_dist` ramp advanced at the unpaused pre-camera
  boundary; its five renderer-BSS words survive stream remounts. Graphics flag `0x1000` replaces
  only the GOOL-object camera with the source Q24.8 fixed position, triangular Y bob and fixed
  pitch; the world keeps the authored path camera.
- The WebGL stage has a validated transactional scene-update path with shared immutable
  decoded-texture identity reuse, atomic replacement/removal and a command-only fast path. Distinct
  allocations are conservatively uploaded without cloning or scanning their pixel vectors. Pair
  mounts and cooperative presented frames use the same checked scene representation. A pair-scoped
  CPU builder also reuses the
  active zone/path's parsed ZDAT/SLST/WGEO graph, resident TPAG pages and decoded texture regions;
  zone/path changes rebuild that graph and destination-pair mounts create a fresh cache owner.
- Mounted retail GOOL owns publisher/title, menu, options, password/load, map, intro, ending,
  completion, bonus, boss and game-over progression. The Rust browser flow state mirrors mounted
  presentation and supports direct boot; it does not independently simulate those authored flows.
- Cooperative 30 Hz loop, keyboard, standard-gamepad polling, complete touch pad, pause, mute,
  fullscreen, responsive presentation, WebGL2 output, and WebAudio scheduling. Each simulation
  tick installs the complete retail `tapped`/`held` history consumed by GOOL opcode `0x1a`.
  Keyboard and touch press edges are latched until one simulation sample so a complete press
  between two 30 Hz frames is not lost; keyboard auto-repeat does not manufacture another edge,
  and window blur clears both held and pending input.
- Mounted type-19 PBAK entries are parsed with exact 304- and 511-spawn-word layouts and adapted to
  the live browser runtime. Attract playback restores the recorded camera/player transform, scale,
  bounds, spawn words, RNG, draw/tick timing and full 32-bit pad word; physical input interrupts it,
  and the final recorded pad frame remains observable before the source completion handshake. The
  executable-four/subtype-eight controller is created beneath root one with arguments 2,279 and
  19,993 before `LevelRestart`; its null object-zone identity survives that restart. A nonzero
  `island_cam_rot_x` then sends the checked caption object event `0xE00` with one zero argument and
  retains native input-lock state three; a zero target releases physical input. All nine legal
  recordings, totaling 10,966 frames, and all nine controller program bindings pass opt-in local
  corpus tests. Playback now advances at the typed pre-Crash boundary in the mutation-aware
  eight-root traversal. Root-one caption work therefore precedes `PadUpdatePbak`; the final pad
  word, nested event/rebind and state-three latch become visible before Crash and later roots run.
  A returning caption can release physical input at that same boundary, and the nested completion
  event observes native pre-`tapped` pad history. On the playback start frame, root one retains
  ordinary wall-clock timing and Crash changes only `ticks_per_frame` to the recording header's
  value. Later recorded frames expose `(ticks_cur_frame, ticks_per_frame) == (17, recorded TPF)`
  throughout the traversal without consuming the pad frame early; returning frames expose
  `(17, rounded wall TPF)`. A failed outer frame recovers only the caption event's captured effects
  rather than silently discarding them at the next frame boundary. Executables 4/5/29 retain their
  native null lifecycle zone; a child spawned by the PBAK caption uses the current camera ZDAT only
  for host environment/color initialization, matching the source `cur_zone` fallback without
  probing the `EID_NONE` sentinel.
- Live SFX/music volume and mono options applied independently. Mounted type-12 ADIO item-zero
  samples are decoded and cached locally, controlled by GOOL's 24-voice protocol and mixed into
  WebAudio. ZDAT MIDI references resolve mounted type-13 MIDI and type-14 INST entries; decoded VAB
  programs and SEP tracks use the software synth, thirty-tick zone fades and GOOL's secondary-track
  toggle without retaining proprietary bytes. No procedural oscillator fallback is present.
- Retail `next_lid` writes are consumed at the following 30 Hz boundary before spawn/camera/GOOL.
  The runtime broadcasts `LEVEL_END` to all eight roots in child-before-parent order, retains the
  requested destination unless a final `-2` selects the saved level, carries process-lifetime
  scalar state across a fresh pair-owned runtime and clears every pointer global. Bonus returns
  mount the validated saved zone/path/progress, protect the pre-restart Crash spawn, then restore
  saved spawn words, camera and object state before the normal scan.
- The native 3,592-halfword encountered-object registry is modeled independently from the active
  304-word spawn table. Misc-ten selectors four/five maintain its zero terminator and reusable
  holes; each stream mount clears the active table and reapplies bit eight for matching retained
  tags. Exact synchronous `LevelResetGlobals(1)` resets the documented scalar words and encounter
  registry without clearing savestate, live objects or the active table. Retail card restore writes
  initial lives, runs that reset, then restores the 128-byte payload fields and derived map/unlock
  words; the main-menu resume handshake uses the same protected ordering. Misc 12/11 performs its
  reset before the next GOOL instruction, including a same-handler `SaveState`.
- Versioned automatic resume storage and a 15-slot virtual-card model using the checksummed
  128-byte payload.

## Exact remaining parity gaps

- The browser retail runtime now instantiates displayed group-three zone entities and integrates
  the bounded arena and VM through explicit typed mappings, but it is not the complete object graph
  or GOOL host. Initial ZDAT objects now receive checked zone/path transforms, scale, rotation/mode
  flags, player/object colors and the characterized scalar process defaults; runtime children
  inherit the parent transform and receive zone colors. Entity-pointer words deliberately remain
  outside raw registers. Executable-`0x22` crate scans now retain owned entity locations and
  generation-checked object handles, reproduce strict vertical-stack adjacency, bidirectional
  misc-A links, blocked-crate height compaction, activation/restart resets and the native stagger
  counter without exposing raw pointers. The legally local `a3_9Z` entity 23/24 pair is covered by
  an opt-in golden. Remaining process globals and several host effects remain absent or partial.
  Initial Crash saves, checkpoint
  captures, in-stream restarts and cross-pair session carry are connected. Camera zone
  crossings now execute the ordered lifecycle, dynamic pager references, TERM/migration teardown
  and next-frame adjacent-zone scan. Intra-object once/transition/event dispatch is synchronous;
  checked paging metadata is synchronous and does not claim retail asynchronous I/O timing.
  Unsupported execution
  boundaries quarantine only the individual object rather than skipping a pre-incremented PC.
  The previously recorded legally local 300-frame N. Sanity trace crossed the former ShadC
  executable 29/state-one animation-bound boundary using validated frame bounds instead of the
  source branch's uninitialized C locals. The current Crash-stamp pre/post-physics schedule and
  same-stamp collision tail require a fresh corpus run before that result is claimed for this change
  set. All `0x85` and `0x8e` selectors have checked source-defined behavior; broader object behavior,
  collision response, dynamic lighting and full level progression remain incomplete.
- Misc 12/7 requester continuation is guarded by both the arena generation and the VM machine's
  monotonic object incarnation. If a TERM handler kills the active requester and synchronously
  reuses either slot, the old invocation unwinds as terminated and cannot advance, mutate, or
  unwind the replacement object's stack.
- Cross-level transitions are initiated by authored GOOL and use the native `LEVEL_END` and
  session-carry/remount order. The host keeps all selected file handles, validates and atomically
  swaps every requested pair, and rebuilds pair-scoped camera, lifecycle, pager, renderer and audio
  owners. A same-level `LoadState` issued from inside a `LEVEL_END` handler still stops at a checked
  boundary because continuing that exact handler requires a resumable nested browser restart
  transaction. A legally local scan of all 44 retail pairs found zero authored occurrences of that
  nested case. Complete asynchronous page residency timing also remains a gap.
- `crust-renderer` implements texture decode, cache keys, projection, ordering and blend-command
  rules. Its WebGL2 command backend is connected to the live stage and presents decoded loading
  images, four image-backed retail title states and camera-selected worlds. Title, Hog Wild
  and Whole Hog begin in zero-world dummy zones whose SLST references are deliberately external to
  their current stream, so they have no standalone snapshot. N. Sanity Beach now matches the
  observed first presented path point, draw count and 679-polygon visibility list. Automatic and
  hosted-main-object follow cameras drive subsequent path/zone scene commands. GOOL 3D
  SVTX/CVTX object models and current animation transforms are coupled to the same scene and texture
  manifest. Type-two sprites, type-five fragments, type-four text/font glyphs and status-B 2D CVTX
  are coupled to the same ordered command/texture path. Mid-frame paging-driven texture changes are
  not yet coupled to rendering. Post-update object snapshots do honor dynamic teardown and the
  current display mask.
  Twenty-two starts use fog/ripple/lightning/dark variants whose world-level dynamic vertex/color
  effects remain incomplete. Object shader modes two and three and their source far-object
  rejection are live, as is the object-only `0x1000` fixed-camera substitution. Mode four is wired
  through immutable snapshots carrying source-order player/pause selection and `dark_dist`; a legal
  all-pair boot trace rendered 1,800 mode-four vertices into 2,880 object primitives. Browser START
  now creates the native executable-four/subtype-four root-seven controller, publishes the tagged
  pause reference, freezes ordinary GOOL/camera/shader/draw-count work, and resumes through the
  source `0xC00` clock-rewind/cleanup handshake while spawn, scene presentation, display latching
  and audio continue. The captured N. Sanity pause panel did not show readable authored `WillT`
  glyphs, so pause-menu text remains a presentation gap. Mode four's derived light matrix and
  ambient color are not yet written back into mutable GOOL object colors, so subsequent GOOL color
  reads can differ.
  Rendering snapshots
  the complete arena after GOOL, while the source interleaves each object's simulation and drawing
  during preorder traversal. The current builder avoids reparsing an unchanged active graph,
  bounds its parsed object-frame cache to 256 entries and records decoded-texture cache hits, but
  the projection and command list are still regenerated every presented gameplay frame. No
  low-end/mobile frame-time or long-soak parity is claimed without measurement in those browsers.
- `crust-audio` implements SPU ADPCM, loop semantics, caching, the retail 24-voice allocation and
  control state machine, sequence events and a software synth. GOOL SFX now resolve type-12 ADIO
  entries from the mounted local NSF and reach WebAudio with owner cleanup. Retail INST/VAB/SEP/MIDI
  music is connected, including volume, pan, expression, sustain, pitch bend, program selection,
  zone fades and dual-track toggles. Exact SPU ADSR timing, vibrato, portamento, pressure/generic
  controllers, spatial reverb and hardware-identical voice priority remain gaps. The newest music
  path was compile-, model- and legally-local-disc tested but not yet manually auditioned.
- Camera path selection is coupled to the hosted main object, and the checked GOOL solid query uses
  validated ZDAT octrees/colors plus ordered animation-derived frame bounds for the characterized
  legal branches. The former diagnostic movement and fixed-distance completion path is no longer
  used. A previously recorded strict legally-local 18,000-frame authored-input trace moved the
  camera and Crash through the complete N. Sanity `a1_9Z` through `b7_9Z` chain and emitted the
  authored Level Complete destination `0x2d` at frame 1,995 with no VM errors, faults, restart or
  terminal fall. An intermediate native-schedule run later stopped at the b5/b6 boundary; that is
  retained only as historical evidence, not a current progression gap. The corrected legally local
  2,100-frame controller route follows `b5_9Z:p4 → b5_9Z:p1 → b6_9Z:p0`, reaches
  `b7_9Z`'s `WarpC`, and emits `Transition(0x2d)` at frame 1,906. It records 18 zone transitions,
  42 observed paths, 65 successful spawns and 32,808 GOOL executions with zero restarts, falls, VM
  errors or faulted objects. The intermediate b5/b6 stop was caused by missing test-controller route
  actions at authored static cells; a later b7 stop came from steering `LEFT` around the live portal
  lane. Correcting those route actions required no camera or collision runtime change. This
  deterministic route does not establish a browser playthrough or full retail parity.
  Broader collision and later Crash, boss, box, checkpoint, enemy, bonus and ending behavior remain
  open. Hog Wild's previously
  recorded checked 360-frame idle trace delivered the authored fall-kill event to state 22,
  advanced `fade_counter` through the native `-2`/`-1` sentinels, emitted `LoadState`, and completed
  two same-level restarts with no VM error, faulted
  object, checked issue or retained terminal fall. This characterizes the idle death/restart loop;
  it does not establish steering, enemy, bonus or level-completion parity. Collision-generated
  ceiling, outside-zone, water and final-surface events now dispatch synchronously at their native
  `solid.c` call sites. Ordered status/link effects and the mover's current process fields are live
  before each nested handler; handler mutations are refreshed before the remaining collision work.
  Native process-global smooth-stop memory and the bounds-invalidated `cur_zone_query` cache are
  shared across objects and frames and reset together at `LevelInitMisc`.
- With a complete retail title stream, password input and validation belong to the mounted
  `0e_pZ` GOOL object graph; the reference C host contains no separate password codec. If that
  authored graph cannot be spawned, the browser remains on its loading/error presentation rather
  than applying a data-independent password rule. Browser card/resume
  storage and signed misc-15 operations are wired to the exact 15-slot,
  128-byte virtual-card model, including rescan/format/save/load handshakes and synchronous GOOL
  result globals. These paths and damaged-card behavior are heavily model-tested, but a complete
  authored save/load playthrough across every title and level transition is not yet certified.
- One deterministic retail-authored N. Sanity route now reaches its real end warp and requests
  Level Complete. No boss, complete bonus round trip, ending, broad death/checkpoint sequence,
  long soak, mobile audio session, or multiple physical gamepad matrix has been completed.

## Automated coverage

The workspace includes native tests for malformed readers, ISO fields and extents, stream names and
catalogs, NSD/NSF/page/entry bounds, tagged references, MDAT title composition, GOOL program/state
graphs, state rebinding, all five animation descriptor families, TGEO/SVTX/CVTX frame parsing,
animation references/waits, PBAK layout/playback and hosted entity/child execution, complete
`0x85`/`0x8e` selector families, `SZON` neighbor resolution, misc 12/7 neighbor teardown,
level-global/card-restore ordering, encountered-object registry behavior, ZDAT entities, SLST
endpoint/cursor/rollback behavior, WGEO scene graphs, all SLST delta/swap forms, the fixed object
arena/spawn tree and frame bounds, signed packed vertices, retail TPAG/CLUT/UV references, world and
object fixed-point projection/lighting/culling, presentation order, scheduling, paging, GOOL execution,
collision, camera, title transitions, bonus returns, demo frames, card operations,
storage envelopes, input, texture formats/cache/projection/blends, ADPCM, sample mixing and software
synthesis. Property tests exercise parser/state-machine invariants where arbitrary input is useful.

Browser checks and the exact exercised flows are recorded in [VERIFICATION.md](VERIFICATION.md).
A passing native suite is never described as browser or retail parity.

## Later parity gates after integration is complete

- Completion of every normal level, boss, bonus route and death/checkpoint path.
- Every cross-level stream dependency and long transition chain.
- Pixel-level camera, animation, draw ordering, texture-cache and translucency comparison.
- Musical program/envelope equivalence for every retail sequence and all spatial SFX behavior.
- Password edge cases, every card-damage path, 100%/ending variants, and long demo sequences.
- Long-soak performance on low-memory mobile browsers and multiple gamepad models.

User data is never embedded to make an automated test pass. Golden data derived from a local disc
stays outside Git and is referenced only by opt-in local test paths.
