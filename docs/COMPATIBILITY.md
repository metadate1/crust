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
  retail-frame pre-increment texture-animation count; pause freezes the last presented snapshot.
- Source-derived camera modes 0/1/3, tapped auto-camera skipping and path/zone crossing drive the
  live scene. Modes 5/6 consume the hosted main object's transform, camera zoom, held input and
  prior frame stamp through checked `CamFollow` projection, neighbor selection and smoothing.
- Displayed current-zone neighbors are decoded into owned ZDAT entity descriptors when a pair is
  mounted. In gameplay, bonus, boss and ending flow states, the browser spawns their group-three
  entities into the checked retail arena and executes that arena at the cooperative 30 Hz boundary.
- The NSF program host binds initial and requested GOOL states, synchronously applies characterized
  child-spawn effects, maintains typed arena/VM links, and advances the implemented state-change
  and animation-select/wait path using frame/draw counters. Initial and global-call frames share the
  bounded process/register array at the parsed `init_sp`, and state links consult the validated
  target descriptor flags. Rebind runs captured once and target transition blocks synchronously,
  including nested calls/returns and hosted spawns. Checked aligned code/storage/entry references,
  paging cases one through six, path orientation and the characterized entity-color/solid query
  slices are active. Normal, once, transition, event-service and interrupt code all complete typed
  audio calls synchronously before the following instruction. Execution, checked-error and
  quarantined-object counts are exposed through the engineering log/debug surface.
- Camera `LevelUpdate` effects drive a source-ordered zone lifecycle. Departed active zones receive
  TERM in postorder, migrated objects survive, released subtrees clear typed VM/link/audio state,
  old load lists close before new lists open, newly adjacent zones activate, and their objects scan
  on the following cooperative frame. Display and animation masks latch with retail's one-frame
  timing.
- Pair-scoped GOOL item-five descriptors and TGEO/SVTX/CVTX frames feed the live renderer. Current
  3D vertex objects use retail fixed-point transforms, lighting, culling and ordering and share one
  resident load-list-filtered TPAG cache/manifest with the world. Eligible collidable animations
  also register exact-preorder transformed bounds in the 96-entry frame arena before execution.
- The WebGL stage has a validated transactional scene-update path with shared immutable
  decoded-texture identity reuse, atomic replacement/removal and a command-only fast path. Distinct
  allocations are conservatively uploaded without cloning or scanning their pixel vectors. Pair
  mounts and cooperative presented frames use the same checked scene representation. A pair-scoped
  CPU builder also reuses the
  active zone/path's parsed ZDAT/SLST/WGEO graph, resident TPAG pages and decoded texture regions;
  zone/path changes rebuild that graph and destination-pair mounts create a fresh cache owner.
- Rust title/publisher sequencing, main menu, options, password, load, map, intro, ending,
  completion, bonus, boss, game-over, and direct-boot state models.
- Cooperative 30 Hz loop, keyboard, standard-gamepad polling, complete touch pad, pause, mute,
  fullscreen, responsive presentation, WebGL2 output, and WebAudio scheduling. Each simulation
  tick installs the complete retail `tapped`/`held` history consumed by GOOL opcode `0x1a`.
- Live SFX/music volume and mono options applied independently. Mounted type-12 ADIO item-zero
  samples are decoded and cached locally, controlled by GOOL's 24-voice protocol and mixed into
  WebAudio; music remains the generated software sequence.
- Versioned automatic resume storage and a 15-slot virtual-card model using the checksummed
  128-byte payload.

## Exact remaining parity gaps

- The browser retail runtime now instantiates displayed group-three zone entities and integrates
  the bounded arena and VM through explicit typed mappings, but it is not the complete object graph
  or GOOL host. Initial ZDAT objects now receive checked zone/path transforms, scale, rotation/mode
  flags, player/object colors and the characterized scalar process defaults; runtime children
  inherit the parent transform and receive zone colors. Entity-pointer words deliberately remain
  outside raw registers, and MDAT positioning, box stacking/stall adjustment, save-state spawn
  hooks, remaining process globals and several host effects remain absent or partial. Camera zone
  crossings now execute the ordered lifecycle, dynamic pager references, TERM/migration teardown
  and next-frame adjacent-zone scan. Intra-object once/transition/event dispatch is synchronous;
  checked paging metadata is synchronous and does not claim retail asynchronous I/O timing.
  Unsupported execution
  boundaries quarantine only the individual object rather than skipping a pre-incremented PC.
  The legally local 300-frame N. Sanity trace now crosses the former ShadC executable 29/state one
  animation-bound boundary using validated frame bounds instead of the source branch's
  uninitialized C locals. Solid suboperations one and three implement their characterized helper
  paths; the late post-physics animation-stamp refresh, solid suboperations zero/two/four/five,
  transform-vector suboperation six and some lighting selectors remain typed gaps.
- Cross-level transitions remain high-level Rust state transitions. The host keeps all selected
  file handles, validates and swaps every requested destination pair, and updates the active scene;
  it does not yet reproduce the complete retail page residency/transition handshake.
- `crust-renderer` implements texture decode, cache keys, projection, ordering and blend-command
  rules. Its WebGL2 command backend is connected to the live stage and presents decoded loading
  images, four image-backed retail title states and camera-selected worlds. Title, Hog Wild
  and Whole Hog begin in zero-world dummy zones whose SLST references are deliberately external to
  their current stream, so they have no standalone snapshot. N. Sanity Beach now matches the
  observed first presented path point, draw count and 679-polygon visibility list. Automatic and
  hosted-main-object follow cameras drive subsequent path/zone scene commands. GOOL 3D
  SVTX/CVTX object models and current animation transforms are coupled to the same scene and texture
  manifest. Sprite, font, text, fragment and 2D CVTX commands and mid-frame paging-driven texture
  changes are not yet coupled to rendering. Post-update object snapshots do honor dynamic teardown
  and the current display mask.
  Twenty-two starts use fog/ripple/lightning/dark variants whose dynamic vertex/color effects
  remain incomplete. Object shader modes two through four, their far-object rejection and the
  zone-graphics `0x1000` fixed-pitch camera substitution are also incomplete. Rendering snapshots
  the complete arena after GOOL, while the source interleaves each object's simulation and drawing
  during preorder traversal. The current builder avoids reparsing an unchanged active graph,
  bounds its parsed object-frame cache to 256 entries and records decoded-texture cache hits, but
  the projection and command list are still regenerated every presented gameplay frame. No
  low-end/mobile frame-time or long-soak parity is claimed without measurement in those browsers.
- `crust-audio` implements SPU ADPCM, loop semantics, caching, the retail 24-voice allocation and
  control state machine, sequence events and a software synth. GOOL SFX now resolve type-12 ADIO
  entries from the mounted local NSF and reach WebAudio with owner cleanup. Retail INST/VAB/SEP/MIDI
  program/envelope music, spatial panning and reverb DSP remain absent; generated music is still
  used. The newest SFX path was compile- and model-tested but not manually auditioned in-browser.
- Camera path selection is coupled to the hosted main object, and the checked GOOL solid query uses
  validated ZDAT octrees/colors plus ordered animation-derived frame bounds for the characterized
  legal branches. The former diagnostic movement and fixed-distance completion path is no longer
  used. Late bound refresh, remaining collision branches and progression remain incomplete; Crash,
  bosses, boxes, checkpoints, enemies, bonus entrances and ending conditions therefore do not yet
  form a playable progression path.
- The password UI applies a local deterministic progression rule, not the retail password codec.
  Browser card/resume storage is wired and restoration was exercised, but a full retail save/load
  handshake across all transitions has not been playthrough-certified. There is no connected
  retail save selection/completion handshake, and damaged-card handling remains exercised at the
  Rust model/storage boundary.
- One historical diagnostic level and its completion/title-map transition were completed in an
  earlier browser build before the placeholder geometry and movement path were removed. No
  retail-authored level, boss, bonus route, ending, death/checkpoint sequence, long soak, mobile
  audio session, or multiple physical gamepad matrix has been completed.

## Automated coverage

The workspace includes native tests for malformed readers, ISO fields and extents, stream names and
catalogs, NSD/NSF/page/entry bounds, tagged references, MDAT title composition, GOOL program/state
graphs, state rebinding, all five animation descriptor families, TGEO/SVTX/CVTX frame parsing,
animation references/waits and hosted entity/child execution, ZDAT entities, SLST
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
