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
- Initial retail world snapshots for 40 of 43 playable LDAT starts: ZDAT spawn-zone/path, checked
  stateful SLST visibility, WGEO packed vertices/polygons, TPAG/CLUT texture decode and animation,
  fixed-point camera projection and retail world ordering depth are connected to WebGL2. Streams
  with a loading image use the observed tick-two path point/draw count before gameplay is shown.
- Displayed current-zone neighbors are decoded into owned ZDAT entity descriptors when a pair is
  mounted. In gameplay, bonus, boss and ending flow states, the browser spawns their group-three
  entities into the checked retail arena and executes that arena at the cooperative 30 Hz boundary.
- The NSF program host binds initial and requested GOOL states, synchronously applies characterized
  child-spawn effects, maintains typed arena/VM links, and advances the implemented state-change
  and animation-select/wait path using frame/draw counters. Initial and global-call frames share the
  bounded process/register array at the parsed `init_sp`, and state links consult the validated
  target descriptor flags. Execution, checked-error and quarantined-object counts are exposed
  through the engineering log/debug surface.
- The WebGL stage has a validated transactional scene-update path with exact decoded-texture reuse,
  atomic replacement/removal and a command-only fast path. Pair mounts use the same checked scene
  representation, although the retail object loop does not yet issue per-frame scene updates.
- Rust title/publisher sequencing, main menu, options, password, load, map, intro, ending,
  completion, bonus, boss, game-over, and direct-boot state models.
- Cooperative 30 Hz loop, keyboard, standard-gamepad polling, complete touch pad, pause, mute,
  fullscreen, responsive presentation, WebGL2 output, and WebAudio scheduling.
- Live SFX/music volume and mono options applied independently to the generated WebAudio buses.
- Versioned automatic resume storage and a 15-slot virtual-card model using the checksummed
  128-byte payload.

## Exact remaining parity gaps

- The browser retail runtime now instantiates displayed group-three zone entities and integrates
  the bounded arena and VM through explicit typed mappings, but it is not the complete object graph
  or GOOL host. Initial ZDAT objects now receive checked zone/path transforms, scale, rotation/mode
  flags, player/object colors and the characterized scalar process defaults; runtime children
  inherit the parent transform and receive zone colors. Entity-pointer words deliberately remain
  outside raw registers, and MDAT positioning, box stacking/stall adjustment, save-state spawn
  hooks, many opcodes, event/transition dispatch, remaining process globals, object host effects,
  main-player activation semantics and paging interactions remain absent or partial. Unsupported
  checked execution boundaries quarantine the individual object rather than skipping the failed
  instruction. The current legal N. Sanity trace stops four objects at `0x8e` suboperation six
  (entity-node color seeking) and Crash at `0x26` (tagged input references); a successful 30 Hz tick
  is not a claim that every spawned object advanced correctly.
- Cross-level transitions remain high-level Rust state transitions. The host keeps all selected
  file handles, validates and swaps every requested destination pair, and installs its initial
  snapshot, but it does not yet page entries or update that scene at runtime.
- `crust-renderer` implements texture decode, cache keys, projection, ordering and blend-command
  rules. Its WebGL2 command backend is connected to the live stage and presents decoded loading
  images, four image-backed retail title states and decoded initial worlds. Title, Hog Wild
  and Whole Hog begin in zero-world dummy zones whose SLST references are deliberately external to
  their current stream, so they have no standalone snapshot. N. Sanity Beach now matches the
  observed first presented path point, draw count and 679-polygon visibility list. Entity/GOOL
  mutations, `CamUpdate`, path progress and object models are not yet driving subsequent scene
  commands; the WebGL update API is ready but the canvas remains on the static initial snapshot.
  Twenty-two starts use fog/ripple/lightning/dark variants whose dynamic vertex/color effects
  remain incomplete.
- `crust-audio` implements SPU ADPCM, loop semantics, caching, 24-voice mixing, sequence events and
  a software synth. The live WebAudio path currently plays an original generated sequence and
  generated SFX; it does not parse and reproduce the retail VAB/SEP/MIDI program/envelope set.
- Collision, camera, player, demo, bonus/boss and completion rules are independently tested models,
  but they are not coupled to the browser's hosted retail objects. The former diagnostic movement
  and fixed-distance completion path is no longer used for retail gameplay. Keyboard/gamepad/touch
  snapshots are collected, but retail input globals are not yet written into GOOL process state;
  Crash, bosses, boxes, checkpoints, enemies, bonus entrances and ending conditions therefore do
  not form a playable progression path.
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
graphs, state rebinding, animation references/waits and hosted entity/child execution, ZDAT
entities, SLST endpoint/cursor/rollback behavior, WGEO scene graphs, all SLST delta/swap forms, the
fixed object arena/spawn tree, signed packed vertices, retail
TPAG/CLUT/UV references, fixed math, presentation order, scheduling, paging, GOOL execution,
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
