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
  Graphics-flag `0x100` worlds apply the source's pre-transform ripple only to effect-marked WGEO
  vertices. Its 16-cell wave uses the native seed/advance/wrap/absolute-value rules and the exact
  level-specific rates. Pair-scoped state advances only for an unpaused submission containing
  visible ripple-world polygons. Pause or a world-hidden/empty submission freezes it independently
  of texture animation; a later draw-skip presentation gate still performs the source transform
  and advances the wave.
  Lightning, combined Dark and Dark2 are also live. Their level-specific tables, random targets,
  ruins colors, two-stamp thunder cooldown, doctor/Crash illumination choice and torch ramps are
  updated before camera work, then frozen into the current world submission. The zero-initialized
  process RNG-B is reconciled across shaders, PBAK selection and audio allocation. World dispatch
  retains native `far_color1` scratch across mounts; Dark2 intentionally consumes the preceding
  target, and hidden presentation frames still perform the real transform/state update. Dark2's
  non-null `doctor` global is resolved as a retained physical pool-slot reference: its final
  initialized translation survives reclamation, compact VM reuse in another slot cannot retarget
  it, and physical LIFO slot reuse can. A per-global write epoch distinguishes a later assignment
  that encodes the same 32-bit tagged VM word.
- Source-derived camera modes 0/1/3, tapped auto-camera skipping and path/zone crossing drive the
  live scene. Modes 5/6 consume the hosted main object's transform, camera zoom, held input and
  prior frame stamp through checked `CamFollow` projection, neighbor selection and smoothing. The
  authored global-65 `gem_stamp` now drives the exact `frames_elapsed - gem_stamp <= 15` neighbor
  gate instead of a browser constant. Authored `GOOL_FLAG_SPIN_DEATH` now runs the retail
  vertex-follow `CamDeath`: its live object/animation/model vertex is generation- and bounds-checked,
  its PC sqrt/atan tables, signed pitch threshold, nine-frame alignment, zoom/orbit seeks and
  `ticks_per_frame` rotation are integer-exact, and its pose feeds both object projection and the
  WebGL scene. `GOOL_FLAG_SPIN_ACCEL` uses the source `0x40000` bit.
- Displayed current-zone neighbors are decoded into owned ZDAT entity descriptors when a pair is
  mounted. In title, gameplay, bonus, boss, level-complete, intro and ending flow states, the
  browser spawns their group-three entities into the checked retail arena and executes that arena
  at the cooperative 30 Hz boundary. Before the first zone scan, every gameplay, boss, bonus, and
  map mount creates native executable-four life, fruit, and pickup roots beneath logical root one
  and publishes their checked references to globals 7, 6, and 14. The four native exclusions—title,
  level complete, intro, and ending—create none. Mount-time `LevelInitMisc(1)` also creates the
  applicable native root-four controller for levels `0x05`, `0x14`, `0x16`, `0x17`, `0x22`, and
  `0x2e`; Ripper Roo's executable-39/subtype-four controller is published to global 8.
- The NSF program host binds initial and requested GOOL states, synchronously applies characterized
  child-spawn effects, maintains typed arena/VM links, and advances the implemented state-change
  and animation-select/wait path using frame/draw counters. Animation host effects `0x83` and `0x84`
  synchronously refresh the object's persistent local bound before execution continues; `0x83`
  applies the status-B `0x18` gate and native range/force test, while `0x84` is unconditional.
  Initial and global-call frames share the bounded process/register array at the parsed `init_sp`,
  and state links consult the validated target descriptor flags. Rebind runs captured once and
  target transition blocks synchronously, including nested calls/returns and hosted spawns. Checked
  aligned code/storage/entry references, every `0x85` transform-vector selector and every `0x8e`
  solid/color selector are active, including their source-defined no-op cases. Paging cases one/six
  open, case two closes, and case three probes through a typed synchronous browser Pager request;
  cases four/five remain local queries. Open exhaustion rolls back the VM's optimistic reference,
  resident replacement reports and re-arms the evicted page, native-idempotent closes preserve
  shared-page counts, probes do not mutate the Pager, and EID/page mismatches fail checked. `SZON`
  performs the exact reverse
  current-header neighbor scan with inclusive wrapped Q24.8 rectangles and updates the linked
  object's zone only on a match. Normal, once, transition, event-service and interrupt code all
  complete typed audio calls synchronously before the following instruction. Execution,
  checked-error and quarantined-object counts are exposed through the engineering log/debug
  surface. Opcode `0x14` (LEA) keeps native input-before-output address translation and represents
  logical storage with checked object handles and linked registers with physical-pool handles.
  Process-local animations use the same-object subset. Internal and register aliases support
  complete bounded descriptors for type-one vertex models, type-two sprites,
  type-four local text terms, and type-five fragments; type one also supplies its model to the local
  collision-bound path. Type-three font selections, type zero, and unknown type bytes retain their
  native no-draw behavior, with non-vertex bounds where applicable. Process text still resolves its
  font offset against global animation item five. Foreign-object aliases, external-state-table
  aliases, and the rotating constant region are rejected because their backing identity/lifetime is
  not represented by the current checked token. Opcode `0x81` is the native one-cycle no-op.
  Transform selector `0x85/0` treats a declared one-point entity path as stationary before indexing.
  This is the intentional safe meaning of the source's later one-point return: Title's island
  controller reaches progress `0x110`, where the original C has already indexed into the next
  entity's relocated pointer and therefore has no stable 32-bit result to preserve. Multi-point
  malformed progress remains a checked failure.
  Checked object-pointer values retain physical native pool-slot provenance across globals,
  pre-existing process links, registers and stack words, plus internal/external MOV storage. Retail
  reclaim captures missing provenance before removing the logical identity instead of clearing
  inbound links. Linked register loads and stores address either the live occupant or the retained
  free-slot process array, preserve nested pointer provenance, and retarget only when the same
  physical slot is reused. The ordinary 96-slot allocator reproduces the native ascending initial
  free chain, arbitrary-slot unlink, reclaim-time parent/sibling/child mutations, and LIFO reuse;
  binding is preflighted and committed transactionally. Authored writes that would corrupt those
  three allocator-owned words while a slot is free are rejected rather than reproducing unsafe
  malformed-list behavior. Reused slots inherit their process array before selective
  initialization resets the source-listed fields, including raw `sp`, `pc`, `fp`, `tp`, and `ep`,
  while untouched words persist. This covers Jaws of Darkness's `FruiC` state-12
  creator read of `0xffff3800` (`-51,200`) and Dr. N. Brio's live `BoxsC` creator links after their
  `BriOC` target is reclaimed.
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
  through the native fade boundary. Each browser frame preserves
  `GOOL → TitleUpdate → TitleLoadState → GLUpdate`; the runtime performs any screen load before
  the final authored title-state comparison, and the browser `GameFlow` is only a passive screen
  mirror. A swap-frame latch retains native's opaque `GLDrawOverlay(255)` if newly loaded GOOL
  synchronously requests another fade. The final WebGL pass uses the source's 16-level nonlinear
  black-overlay alpha table and exact pre-quantization counter step; blank and state-swap phases
  stay opaque without affecting gameplay rendering. Type-zero loads preserve the source's `0x3ff0`
  object-category tail (`0x22_3ff0`) and the following start/blank tick enables only the display and
  animate bits (`0x22_3ffc`). Each screen swap tears down old objects and performs the source
  flag-two `LevelUpdate` before spawning the next image-backed MDAT entities. The MDAT EID remains
  descriptor provenance, while each spawned object's zone, origin and colors come from the current
  ZDAT exactly like the source's type-17 rewrite, keeping it in current-neighbor TERM scope. An
  authored arena owns the 4:3 canvas. Before it is available the browser presents only loading/error
  diagnostics and external status; it does not advance a synthetic title, menu or gameplay flow.
- Island-map camera modes seven/eight consume GOOL globals 66/64 through a typed host boundary.
  Mode seven publishes its returned state before `LevelUpdate`; mode eight publishes after the
  synchronous `LevelUpdate`/TERM boundary. The legally local normal route reaches Main Menu ready
  at frame 10, loads Map at frame 20, reaches Map ready at frame 30, emits authored
  `next_lid = 0x09` on Cross at frame 31, completes clean `LEVEL_END`, and imports that session
  carry into N. Sanity Beach at draw count 31.
- Island-map state 15 consumes each WGEO item-three path list with the source's
  `len + type-as-record-zero` layout. The active group carries across worlds, globals 73/75 control
  the 64 group bits, and effective mask-seven/mask-zero writes are applied to frame-local polygon
  copies. A graph-scoped sidecar preserves the last masks through fade-out, while the parsed user
  stream remains immutable.
- Camera `LevelUpdate` effects drive a source-ordered zone lifecycle. Departed active zones receive
  TERM in postorder under the old texture protection, migrated objects survive, and released
  subtrees clear typed VM/link/audio state. On a normal transition, destination protection is
  installed before old-list closes and new-list opens. A hard restart instead keeps old protection
  through RESPAWN, TERM, and old-list closes, then switches immediately before the first restored
  open. Each Pager delta is mirrored into the VM, newly adjacent zones activate, and their objects
  scan on the following cooperative frame. Display and animation masks latch with retail's
  one-frame timing.
- Misc 12/7 synchronously visits the current header's forward neighbor list without filtering or
  deduplication. Each neighbor uses the live eight-root postorder TERM traversal, including immunity,
  migration, non-title Crash survival and persistent typed `ObjectZoneContext` target/sentinel
  semantics. Object audio and typed tree/link ownership are cleaned synchronously; arena spawn
  flags remain authoritative until their VM mirror is refreshed at the next frame boundary. A null
  current zone is a no-op; duplicate EIDs are retained and later entries rescan the mutated live
  tree.
- Parsed retail `RETURN` at the initial frame is a lifecycle result rather than a permanent halted
  object or a VM fault. The preorder runtime releases that subtree before display/child traversal
  through the native no-signal path, so TERM is not dispatched; main-object protection outside
  Title is unchanged. Synthetic VM fixtures retain their ordinary top-level `Halted` result. This
  distinction keeps Ending's recurring credits objects bounded instead of exhausting the arena.
- Pair-scoped GOOL item-five descriptors and TGEO/SVTX/CVTX frames feed the live renderer. Current
  3D vertex objects use retail fixed-point transforms, lighting, culling and ordering and share one
  resident load-list-filtered TPAG cache/manifest with the world. Collidable animations populate
  the 96-entry frame arena on the native Crash-stamp schedule: matching-stamp objects register before
  GOOL/physics, while objects visited before Crash register after physics only inside the inclusive
  `±0x7d000` X/Z and `±0xaf000` Y window; rejected late objects set status-A invalid bit `0x8000`.
  The same-stamp tail also applies Crash's asymmetric accepted/priority collider links, hotspot
  `0x1000`, and target-collider clearing on a miss. A mover's current collider metadata is read from
  the validated live link independently of this bounded candidate array and refreshed after a
  synchronous handler; later floor/wall decisions therefore still observe a retained collider
  omitted from the current frame snapshot. Hotspot insets preserve raw, potentially inverted
  `p1`/`p2` axes and use the source's direct face comparisons rather than normalizing them. The
  legally local Rolling Stones active-input regression exercises that inverted-axis case for 1,800
  clean frames.
  Type-two sprites, type-five fragments and status-B 2D CVTX use the source ZXY sprite transform.
  Their signed half-size calculation preserves the source MIPS `SLLV`/`SRAV` low-five-bit shift
  semantics and wrapping 32-bit intermediate before checked GTE range rejection. The legal
  Jungle Rollers playback covers the `FruiC` raw-shift sequence 24, 26, 28, 31, 34 … 297 without a
  renderer halt; saturated results are culled at the same validity boundary rather than clamped by
  host shift rules.
  Type-four text uses bounded `sp[-2]` argument aliases, the default or dynamic header-length-bounded
  type-three font, retail formatting/control commands, the extended controller-icon records that
  follow the C declaration's first 63 slots, per-corner color modulation and ordered glyph/backdrop
  quads. Standalone type-three font descriptors remain resource-only, as in the source. ZDAT object
  shader modes two and three are connected with their separate SVTX/CVTX ramps and far-object
  cutoffs. Mode four consumes a source-order player translation (or a checked live pause-object
  translation) and the Lights Out/Fumbling `dark_dist` ramp advanced at the unpaused pre-camera
  boundary; its five renderer-BSS words survive stream remounts. All three modes run at each
  post-update/pre-child vertex-display boundary, honor the native main-object, display-mask
  `0x10000`, status-B `0x400`, near-plane/`0x40000`, and CVTX-only `0x200` gates, and commit their
  derived colors to the live VM before child traversal. Render snapshots retain the effective
  colors independently; status-B `0x100000` can therefore restore the live object/player-zone
  colors without changing the already selected rendering. Graphics flag `0x1000` replaces
  only the GOOL-object camera with the source Q24.8 fixed position, triangular Y bob and fixed
  pitch; the world keeps the authored path camera. Simulation and rendering are cross-checked at
  the camera-space-point level, and the bob follows `frames_elapsed` independently from texture
  `draw_count` through hidden/loading display frames. Null-zone root objects inherit from the
  current ZDAT rather than attempting to resolve an absent EID.
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
  `PbakChoose` counts trailing-`B` type-19 names in the NSD table, advances shared RNG-B for every
  nonempty choice (including count one), constructs the source `pb`/index/level/`B` identifier and
  retains that seed through the destination mount. The recorded absolute clock follows the newly
  current frame after `PadUpdatePbak`; asynchronous mount time remains part of the ordinary native
  clock, while authored pause is excluded. Armed playback preserves the prior loading/scene
  framebuffer until Crash starts it.
- Live SFX/music volume and mono options applied independently. Mounted type-12 ADIO item-zero
  samples are decoded and cached locally, controlled by GOOL's 24-voice protocol and mixed into
  WebAudio. ZDAT MIDI references resolve mounted type-13 MIDI and type-14 INST entries; decoded VAB
  programs and SEP tracks use the software synth, thirty-tick zone fades and GOOL's secondary-track
  toggle without retaining proprietary bytes. Sampled VAB tones now apply their retail ADSR1/ADSR2
  words through the exact fixed-point 44.1 kHz attack/decay/sustain/release state machine before
  mixing. No procedural oscillator fallback is present.
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

- Pool-backed linked-register addresses are represented with a validated aligned 32-bit tag carrying
  the physical slot and full register index. They remain readable/writable through the free interval,
  retarget on exact physical-slot reuse, and never follow unrelated compact-handle reuse. Event and
  child-spawn argument vectors retain the same validated provenance through EARG, mapped state
  rebind, synchronous send, and reused-slot child initialization. Complete vector writes are
  preflighted so a rejected allocator-link mutation cannot partially update retained storage. The
  96-slot ordinary free list is modeled; slot 96 represents the separately allocated player from
  initialization onward and link five stays non-null through Title teardown. Retained local-bound
  bytes on ordinary-slot reuse and the dedicated allocation's extra 0x100 stack-tail bytes remain
  unmodeled, but a legal-corpus audit has not observed either boundary being consumed. Colors and
  animation are overwritten/reset on successful source initialization and are not retained-state
  gaps. Synthetic non-retail VM teardown deliberately retains its older checked inbound-link
  clearing contract.
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
  and next-frame adjacent-zone scan. Intra-object once/transition/event dispatch and GOOL paging
  open/close/probe requests are synchronously coupled to the browser Pager and VM reference state;
  this does not claim retail asynchronous I/O timing.
  Unsupported execution
  boundaries quarantine only the individual object rather than skipping a pre-incremented PC.
  The previously recorded legally local 300-frame N. Sanity trace crossed the former ShadC
  executable 29/state-one animation-bound boundary using validated frame bounds instead of the
  source branch's uninitialized C locals. The current Crash-stamp pre/post-physics schedule and
  same-stamp collision tail pass the legally local corpus suite. All `0x85` and `0x8e` selectors
  have checked source-defined behavior; broader object behavior, collision response, pixel-level
  rendering equivalence and full level progression remain incomplete. LEA-created process
  animation descriptors
  of all five defined types are now parsed: vertex, sprite, text, and fragment descriptors use the
  ordinary checked bound/render paths, while font selections remain resource-only no-draw objects.
  Only same-object internal and register aliases have representable lifetimes. External-state,
  immediate-constant, and foreign-object aliases remain checked failures. The all-pair direct-LEA
  census found 30 static no-draw data aliases (18 type `0x73`, 12 type `0xef`) and one dynamic
  type-zero `BaraC` alias; it
  found no naturally selected process-local type-one-through-five descriptor. Those five paths are
  therefore covered with copied retail descriptors and malformed-input tests, not claimed as an
  observed retail progression route.
- Misc 12/7 requester continuation is guarded by both the arena generation and the VM machine's
  monotonic object incarnation. If a TERM handler kills the active requester and synchronously
  reuses either slot, the old invocation unwinds as terminated and cannot advance, mutate, or
  unwind the replacement object's stack.
- Cross-level transitions are initiated by authored GOOL and use the native `LEVEL_END` and
  session-carry/remount order. The host keeps all selected file handles, validates and atomically
  swaps every requested pair, and rebuilds pair-scoped camera, lifecycle, pager, renderer and audio
  owners. Initial boot and committed remounts perform the source `CoreObjectsCreate` pad-history
  shift before root work; an armed state-three PBAK mount preserves prior history while suppressing
  its new held/tapped words. A same-level `LoadState` issued from inside a `LEVEL_END` handler still
  stops at a checked boundary because continuing that exact handler requires a resumable nested
  browser restart transaction. A legally local scan of all 44 retail pairs found zero authored
  occurrences of that nested case. Different-level loads capture their restart kind and clear bonus
  mode at the
  synchronous instruction boundary; later GOOL and LEVEL_END recipients can continue without a
  later SaveState reclassifying the earlier request. Complete asynchronous page residency timing
  also remains a gap.
- `crust-renderer` implements texture decode, cache keys, projection, ordering and blend-command
  rules. Its WebGL2 command backend is connected to the live stage and presents decoded loading
  images, four image-backed retail title states and camera-selected worlds. Title, Hog Wild
  and Whole Hog begin in zero-world dummy zones whose SLST references are deliberately external to
  their current stream, so they have no standalone snapshot. N. Sanity Beach now matches the
  observed first presented path point, draw count and 679-polygon visibility list. Automatic and
  hosted-main-object follow cameras drive subsequent path/zone scene commands. GOOL 3D
  SVTX/CVTX object models and current animation transforms are coupled to the same scene and texture
  manifest. Type-two sprites, type-five fragments, type-four text/font glyphs and status-B 2D CVTX
  are coupled to the same ordered command/texture path. Colored and textured quads split in the PC
  backend's exact `[0,1,3]`, `[3,2,0]` order. The pager models the eight usable native
  texture slots (physical slots 8–15), source-order free/stale/unprotected replacement,
  current-zone load-list protection and immutable generation snapshots; initial mount, restart and
  zone transition install destination protection before opening pages. GOOL paging effects execute
  synchronously against that Pager. The world and filter use one exact frame-start slot snapshot,
  while every post-update/pre-child object display record replays its live
  `(EID, generation, page)` map before command generation. Cached pre-eviction regions survive a
  same-slot `A → B → A` sequence, uncached regions decode from the live mapping, and returning A
  reuses its frozen internal generation. Dynamic teardown cannot retract an earlier display record.
  Twenty-two starts use fog/ripple/lightning/dark variants. Source-order flag dispatch now selects
  Dark2, combined fog/lightning, fog, ripple, lightning or plain transforms without allowing a
  lower-priority ripple bit to displace a fog/dark world. The source ripple displacement is live
  for selected ripple worlds, including its effect-vertex selection and submission-driven wave
  state. Fog uses the projected-depth cutoff, backdrop exemption, ZDAT far color and clamped
  fixed-point color interpolation. Lightning uses effect-bit channel selection and the complete
  per-level fixed/random sequence state. Combined Dark applies that result before fog without the
  pure-Fog backdrop exemption. Dark2 uses projected screen depth, camera-space world translation,
  persistent renderer target color and the live doctor/Crash illumination plus ambient/distance
  ramps. Object
  shader modes two and three and their source far-object
  rejection are live, as is the object-only `0x1000` fixed-camera substitution. Mode four is wired
  at the native display boundary with source-order player/pause selection and `dark_dist`; a legal
  all-pair boot trace exercised 1,800 mode-four vertex displays, rendered 2,880 object primitives,
  and verified 540 changed color results persisted in the live VM. Browser START
  now creates the native executable-four/subtype-four root-seven controller, publishes the tagged
  pause reference, freezes ordinary GOOL/camera/shader/draw-count work, and resumes through the
  source `0xC00` clock-rewind/cleanup handshake while spawn, scene presentation, display latching
  and audio continue. The N. Sanity pause panel is the authored type-five `WillT` fragment
  animation rather than type-four font text. Its five pieces render as
  `PAUSED / PUSH SELECT FOR MAP`, at the retail far ordering depth, with the authored 15-frame
  visible/15-frame hidden blink. This is covered by a legally local scene regression and an
  on-cycle WebGL browser capture. Geometry command construction remains deferred until after GOOL,
  but it consumes the ordered owned display-record list captured during preorder traversal rather
  than re-reading the complete final arena. Animation/frame, transform, render process flags, text
  font/arguments, effective colors, live display mask, VM side effects, and Pager slot state are
  captured at the native per-object boundary. A later child link write, teardown, or reparent cannot
  mutate or retract an already-rendered parent record. World geometry
  separately retains the pre-GOOL display mask, so an authored global-nine write during traversal
  cannot retroactively hide the world or be replaced by the end-of-frame next-mask latch. The
  current builder avoids reparsing an unchanged active graph,
  bounds its parsed object-frame cache to 256 entries and records decoded-texture cache hits, but
  the projection and command list are still regenerated every presented gameplay frame. No
  low-end/mobile frame-time or long-soak parity is claimed without measurement in those browsers.
- `crust-audio` implements SPU ADPCM, loop semantics, caching, the retail 24-voice allocation and
  control state machine, sequence events and a software synth. GOOL SFX now resolve type-12 ADIO
  entries from the mounted local NSF and reach WebAudio with owner cleanup. Retail INST/VAB/SEP/MIDI
  music is connected, including volume, pan, expression, sustain, pitch bend, program selection,
  zone fades and dual-track toggles. Sampled voices now use exact fixed-point SPU ADSR timing for
  attack, decay, sustain, release, key-off, and linear/exponential rate modes. Each mount applies
  the exact `MidiInit` level/volume partition between music and SFX slots and restores the native
  full-scale inactive master fade before destination audio starts. Remaining gaps are
  Gaussian interpolation (the synth currently interpolates linearly), vibrato, portamento,
  pressure and unsupported generic controllers, spatial reverb/effects, SPU noise/FM, and
  hardware-identical priority/arbitration across a shared 24-voice SFX/music pool. The music
  sequencer currently has a separate 64-voice software limit. The newest music path was compile-,
  model- and legally-local-disc tested but not yet manually auditioned.
- Camera path selection is coupled to the hosted main object, and the checked GOOL solid query uses
  validated ZDAT octrees/colors plus ordered animation-derived frame bounds for the characterized
  legal branches. The former diagnostic movement and fixed-distance completion path is no longer
  used. A previously recorded strict legally-local 18,000-frame authored-input trace moved the
  camera and Crash through the complete N. Sanity `a1_9Z` through `b7_9Z` chain and emitted the
  authored Level Complete destination `0x2d` at frame 1,995 with no VM errors, faults, restart or
  terminal fall. An intermediate native-schedule run later stopped at the b5/b6 boundary; that is
  retained only as historical evidence, not a current progression gap. The corrected legally local
  2,100-frame controller route follows `b5_9Z:p4 → b5_9Z:p1 → b6_9Z:p0`, reaches
  `b7_9Z`'s `WarpC`, and emits `Transition(0x2d)` at frame 1,900. It records 18 zone transitions,
  42 observed paths, 65 successful spawns and 40,881 GOOL executions with zero restarts, falls, VM
  errors or faulted objects. The intermediate b5/b6 stop was caused by missing test-controller route
  actions at authored static cells; a later b7 stop came from steering `LEFT` around the live portal
  lane. Correcting those route actions required no camera or collision runtime change. The current
  timing and interaction sequence additionally includes the restored flag-enabled `PlotObjWalls`
  `GoolCollide` path for every overlapping frame bound. This
  deterministic route now feeds a separate legally local cross-pair test that starts from a fresh
  authored Title Map initialized through the card-payload restore path. Map requests N. Sanity
  Beach on frame 11. N. Sanity's checked
  `LEVEL_END` exports a session carry, Level Complete imports it and reaches authored Title
  `Transition(0x19)` on frame 513, and Title imports the second carry into its parsed graph, ZDAT
  entities, lifecycle, and map camera schedule. The post-completion Map unlocks level two, reaches
  `1b_pZ` path zero at progress `0x0b00`, and requests Jungle Rollers on frame 253. Process-lifetime
  `draw_count` remains
  phase-continuous through the three imported remounts and the fourth exported carry; same-level
  and bonus-return `LevelRestart` still reset it exactly where the source does. All four outgoing
  level-end broadcasts are clean. This deterministic first completion loop and next map handoff do
  not establish a browser playthrough or full retail parity.
  All 43 selectable starts also completed a strict 1,800-frame direction/button sweep with no
  checked runtime issue: 77,400 browser-ordered simulation frames in aggregate. Jaws of Darkness
  separately completed the same 1,800-frame focused window after its reclaimed-creator pointer was
  given physical pool-slot provenance. This bounded clean survey is not evidence that every route,
  transition, boss, bonus, or ending was reached. The five directly bootable bonus streams receive
  a one-shot same-level snapshot only when a fresh host boot encounters their source save-restricted
  spawn zone, making direct-
  boot death/restart deterministic. Normal parent-to-bonus session mounts retain the parent
  snapshot exactly. Authored `-2` completion after a synthetic direct boot still lacks a distinct
  host return destination, so this is not evidence of a complete bonus round trip. LevelUpdate now
  publishes the current zone graphics flags into GOOL global 30 before the next object pass. All
  five legal bonus spawn zones produce `0x2002`, and the exact Tawna-bonus WillC WARP program tests
  bit `0x2000` before its LoadState branch. A legally local cross-stream test carries the authentic
  Jungle Rollers (`0x0c`) snapshot into Tawna Bonus (`0x24`), delivers WarpC's exact `0x1600`/zero
  event, advances CardC's Cross prompt, observes `LoadState` at frame 301, and resolves sentinel
  `-2` back to `0x0c`. It then reproduces the protected destination spawn/restart and checks the
  restored Crash transform, camera location, box count, and every saved spawn word. Separate
  controlled regressions cover all three authentic `BoxsC` → `FruiC` → `DispC` Tawna-token routes,
  the third-token save/fade/status/`0x24` transition, and WarpC's parsed proximity/status gate at
  its exact quantized boundaries. Together they cover the control path, but they are not one
  uninterrupted pad-driven or browser playthrough.
  The authentic first-completion carry into Jungle Rollers retains source RNG-A and `draw_count`,
  which independently alter its hazard/animation phase; resetting either at mount would be source
  incompatible. The former fresh-boot controller entered Crash state 23 at frame 532 and restarted
  at frame 648. A phase-robust exact-carry regression now flings both early PlanC hazards and
  reaches checkpoint entity 46 at frame 1,117 with its exact translation, a saved pre-increment box
  count of `0x400`, and a live count of `0x500`. The same uninterrupted route proceeds through the
  remaining main-path zones, raises the live counted-box total to `0x1000`, enters the end `WarpC` at
  `0O_cZ` path zero/progress 17,836, and emits `Transition(0x2d)` at frame 2,546. It does so without a
  restart, death camera, below-zero or terminal fall, VM error, faulted object, or checked issue.
  The same carry completes the following Level Complete screen at frame 306, returns to Title, and
  takes the Map's Up/Cross route to The Great Gate `0x12` at frame 253. Map `LEVEL_END` preserves
  current level three, level count one, three unlocked levels, RNG `0x4a04f4bf`, and draw count
  5,782. The Great Gate accepts that carry and follows an exact carried retail-pad route through
  `a1_iZ`-`a9_iZ`, the wide pit, the `WalOC` rotating-log phases, and the first three arrow-crate
  bounces. Checkpoint crate 76 emits `SaveState` at frame 1,152 with the exact `0x900` pre-increment
  count and translation `[20991488, -8397312, 127744]`; the live count advances to `0xa00`. The same
  route proceeds through `b3_iZ`-`c7_iZ`, the snake, later logs and gaps, and five more boxes. It
  enters the normal end `WarpC` and emits `Transition(0x2d)` at frame 2,372 with live count `0xf00`,
  RNG `0x9ada2711`, and draw count 8,154. Across the Great Gate leg it records 97 successful spawns,
  39,979 clean executions, and 34 lifecycle zone transitions without a restart, death camera,
  terminal fall, VM error, faulted object, or checked issue. The yellow-gem alternate branch,
  box-complete gem evaluation, and browser playthrough remain open; this is exact native main-route
  integration coverage rather than a full player-facing claim. The ordinary carry completes the
  following Level Complete screen at frame 273 and returns to Title with RNG `0xa9067f4f`/draw
  8,427. The remounted Map takes Up/Cross to Boulders `0x0e` at frame 253 on `1c_pZ` path
  zero/progress `0x0f00`, retaining RNG `0xf2b6db12` and draw 8,680. Boulders imports that carry and
  executes one clean mounted frame with 15 successful spawns, 23 clean executions, zero
  restart/fault/error, RNG `0x82f84399`, and draw 8,681. This proves the next handoff, not Boulders
  gameplay.
  The first N. Sanity interaction sequence is now characterized from retail data: CrabC entity 14
  is defeated, BoxsC entity 7, entity 12 and seven later counted boxes break, checkpoint entity 19
  saves the source-ordered pre-increment count `0x900` before the live count reaches `0xa00`, and TurtC
  entity 39 drives the authored death camera and same-level checkpoint restart. Fresh runtime
  initialization also publishes native checkpoint sentinel `-1` directly to VM global 69.
  A fixed-34-tick paired reference-C trace confirms the early Box7, CrabC and Box12 contact order.
  Rust's contact-window direct events are `0x400`, then `0x1000`/`0x400`; CrabC's grounded-contact
  gate does not emit `0x300`. The earlier contrary C observation came from rAF-derived clock phase,
  not a collision special case. Broader collision and later
  Crash, boss, box, checkpoint, enemy, bonus and ending behavior remain open. A legally
  local 1,800-frame Ending lifecycle regression now reclaims state-returned `WinGC`
  credits children, reaches at least 64 authored credits-child spawns, reuses arena generations,
  peaks at 82 live objects, and reports no VM fault; this replaces the broken 97-slot saturation at
  frame 1,437 but does not complete or certify the ending. Hog Wild's previously
  recorded checked 360-frame idle trace delivered the authored fall-kill event to state 22,
  advanced `fade_counter` through the native `-2`/`-1` sentinels, emitted `LoadState`, and completed
  two same-level restarts with no VM error, faulted
  object, checked issue or retained terminal fall. A separate active Cortex Power regression covers
  117 consecutive vertex-follow death-camera frames and the count-nine alignment boundary. These
  characterize the observed death/restart paths;
  it does not establish steering, enemy, bonus or level-completion parity. Collision-generated
  ceiling, outside-zone, water and final-surface events now dispatch synchronously at their native
  `solid.c` call sites. Ordered status/link effects and the mover's current process fields are live
  before each nested handler; handler mutations are refreshed before the remaining collision work.
  Crash's state-four invincibility collision now likewise dispatches `0x0a00` synchronously from
  `GoolObjectColors` before physics. The checked host supplies a zero word for the source's
  `argc=1`/null-argv quirk, guards VM incarnations and runtime-handle generations, suppresses the old
  queued placeholder, and retains ignored handler failures as browser-visible diagnostics.
  Native process-global smooth-stop memory and the bounds-invalidated `cur_zone_query` cache are
  shared across objects and frames and reset together at `LevelInitMisc`.
- With a complete retail title stream, password input and validation belong to the mounted
  `0e_pZ` GOOL object graph; the reference C host contains no separate password codec. If that
  authored graph cannot be spawned, the browser remains on its loading/error presentation rather
  than applying a data-independent password rule. Browser card/resume
  storage and signed misc-15 operations are wired to the exact 15-slot,
  128-byte virtual-card model, including rescan/format/save/load handshakes and synchronous GOOL
  result globals. The staged rescan keeps `CHECKING` visible until authored CardC acknowledges
  `FLAG_6`, then publishes on the following update; the legally local empty-card Load screen now
  completes that real operation-two handshake, and the shared `0e` Password selection bypasses it
  as authored. These paths and damaged-card behavior are heavily model-tested, but a complete
  authored save/load playthrough across every title and level transition is not yet certified.
- One deterministic retail-authored N. Sanity route now reaches its real end warp and requests
  Level Complete. The Tawna bonus control path is covered at exact entry, portal, return, and
  remount boundaries, but no uninterrupted pad-driven/browser bonus round trip has been completed.
  No boss, complete ending, broad death/checkpoint
  sequence, long soak, mobile audio session, or multiple physical gamepad matrix has been completed.

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
storage envelopes, input, texture formats/cache/projection/blends, process-local animation payloads,
world-ripple timing, complete world Lightning/Dark/Dark2 state and fixed-point evaluation,
shared-RNG/PBAK timing, invalid-initial-return reclamation, ADPCM, fixed-point ADSR, sample mixing
and software synthesis. Legally local coverage additionally includes the authentic three-token
crate/pickup/HUD route, its bonus `LEVEL_END` carry, WarpC's proximity and quantization boundaries,
and the protected bonus `LoadState`/parent remount. Property tests exercise parser/state-machine
invariants where arbitrary input is useful.

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
