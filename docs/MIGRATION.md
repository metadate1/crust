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
- Every bonus spawn zone is save-restricted. A normal bonus entry carries its parent snapshot;
  only a fresh direct boot seeds a one-shot same-level death/restart snapshot. Direct-boot bonus
  completion still needs an explicit host destination and is not treated as a certified round trip.
  Each LevelUpdate publishes the destination zone graphics flags to GOOL global 30 before spawning;
  legal bonus zones use `0x2002`, which selects the authored WARP LoadState branch.
- Audio is 44.1 kHz, 24 logical voices, with music on voice zero.
- The virtual card has 15 slots and an exact 128-byte little-endian payload. Its checksum starts at
  `0x12345678`, adds each byte with the checksum field zero, then rotates left three bits. Rescan
  publication is an authored acknowledgement: `CHECKING` remains latched until CardC clears
  `FLAG_6`, and the staged part table becomes visible on the following update.
- Existing storage keys and schema versions remain unchanged.
- Image-backed title states resolve `5MapP`, `7MapP`, `8MapP`, and `aMapP` MDAT graphs; each IMAG
  column contains 16×16 indexed tiles and selects a CLUT item through the MDAT IPAL table.
- GOOL programs are retained as owned code/state/EID tables. Serialized PCs are validated within
  the 14-bit code space and never converted to host pointers. External/global code addresses and
  return frames are explicit values; global calls remove their validated argument span on return.
  Child-spawn opcodes stop at a synchronous host boundary, then continue in the same hosted
  interpreter invocation before the next instruction. They pop arguments for every signed count
  and expose `0x91`'s bounded reclaim permission rather than an alternate parent. Opcode `0x14`
  preserves native input-before-output address translation while storing a checked process-local
  handle rather than a pointer; opcode `0x81` preserves the native switch's one-cycle no-op.
- Retail GOOL animation item five is retained as owned bytes. All five descriptor families are
  bounds-checked at unaligned byte offsets, and vertex descriptors resolve TGEO plus exact
  SVTX/CVTX frames through the mounted pair only. Type-three fonts consume the exact glyph count
  in their header length byte. The first 63 slots retain the conventional `0x20..=0x5e` table,
  the backdrop texture aliases slot 63, and the longer tables safely expose authored controller
  icons such as CardC's `c`, `s`, `t`, and `x`. State-change yields resolve the requested state
  through NSD/NSF metadata, bind a new checked state program, and resume with explicit wrapping
  frame/draw stamps; serialized state PCs and animation offsets never become native pointers.
  Toxic Waste's LEA-created `BaraC` type-zero descriptor is decoded from the live same-object
  process words, draws no geometry, and retains native's standard non-vertex collision bound.
  Process-local descriptor types one through five are rejected until their complete variable
  payload and rendering behavior are represented.
- ZDAT runtime pointer slots are treated as opaque serialization fields. Zone/world/path EIDs,
  SLST polygon IDs and WGEO word/vertex indices remain validated offsets and values; the Rust scene
  builder never writes host addresses back into source bytes.
- The island-map WGEO item-three list preserves the source's unusual aliasing contract explicitly:
  its `type` halfword is record zero, the group cursor spans worlds, and globals 73/75 resolve all
  64 link groups. Per-frame polygon copies receive the resulting animation masks instead of
  reproducing the C runtime's mutation of mounted WGEO memory; a scene sidecar retains the last
  writes until the graph or mounted pair changes.
- ZDAT entities retain the exact 20-byte header and six-byte signed path points. The 304 entity
  spawn flags, 96 ordinary objects, dedicated main slot and eight roots are fixed-size Rust state;
  generations reject stale object references after despawn.
- GOOL `0x85` implements every source selector with checked process vectors and typed hosts: path
  orientation, projection, velocity aiming, the source no-op hole, scaled/unscaled transforms,
  model-vertex lookup and camera-relative audio coordinates. `0x8e` likewise implements static and
  object solid response, every directional surface variant, entity-color scaling and its source
  no-op hole. GOP translation, stack consumption, aliases and wrapping fixed-point arithmetic stay
  explicit even in selectors whose native observable work is only operand translation.
- The spinning-death camera no longer stops at a display-mask boundary. A checked resolver turns
  the live tagged object, animation frame and vertex index into an owned world-space focus; a pure
  fixed-point core retains the source death-camera words and count writeback. Browser rendering and
  GOOL transform-vector projection consume the same explicit pose without retaining a C pointer.
- Misc primary nine (`SZON`) never stores a relocated ZDAT pointer. The stream host resolves the
  current header, scans neighbor EIDs in reverse serialized order, parses rectangles lazily and
  applies the source's inclusive wrapping Q24.8 containment test. No match leaves the linked
  object's typed zone handle unchanged; a null point selects the current zone.
- Type-17 title MDAT remains the owned source of each entity descriptor, but it is not an object
  zone. After the title `LevelUpdate`, the browser mirrors `GoolObjectSpawn` by assigning native
  `cur_zone` as the arena/VM zone and resolving origin and colors from that ZDAT. This also makes
  those objects visible to current-header neighbor TERM traversal. The mounted retail object graph
  owns title/menu/gameplay progression. Browser title frames preserve
  `GOOL → TitleUpdate → TitleLoadState → GLUpdate`; `RetailRuntime` owns the fade and screen swap,
  while `GameFlow` passively mirrors the loaded screen. When the authored graph cannot present a
  screen, the browser shows loading/error diagnostics and does not run a data-independent fallback
  flow.
- Misc 12/7 retains the source's distinct forward current-header walk with no display filter,
  sorting or deduplication. Each listed EID drives a live roots-zero-through-seven postorder TERM
  traversal, so handler mutations, immunity flags, migrations and non-title Crash survival remain
  observable. Persistent typed `ObjectZoneContext` runtime state carries the transition target or
  hard-restart sentinel without storing a native pointer. Browser object-audio and typed tree/link
  ownership are cleaned synchronously. The arena's spawn flag clears at teardown and remains
  authoritative until the VM mirror refreshes at the next frame boundary. A null current zone is a
  no-op; duplicate EIDs are preserved and every later entry traverses the tree as mutated by earlier
  TERM handlers. Request continuation is guarded by both the arena generation and the VM object's
  monotonic incarnation, so killing and reusing either slot cannot resume the replaced invocation.
- SLST visibility is reconstructed from the nearest raw endpoint with the retail midpoint tie-break.
  Every adjacent delta is bounds-checked in both directions, and a failed seek rolls back both the
  point index and ordered visibility list.
- Direct-loading presentation uses the observed two-step draw skip: tick one executes but is
  discarded, while tick two presents gameplay with path progress `0x200` and draw count one.
- A level request is latched during GOOL and consumed at the start of the next cooperative frame.
  The requested signed value is retained while `LEVEL_END` visits all eight roots in postorder;
  only a final `-2` selects the saved level. Remount carry owns globals, RNG, savestate, the native
  process-lifetime `draw_count`, other counters and
  the 3,592-halfword encountered-object registry, while every object identity, pointer global,
  pair-backed cache and active 304-word spawn table is rebuilt for the destination. Bonus return
  substitutes the saved zone/path/progress during destination initialization and protects the one
  pre-restart Crash spawn exactly as native `next_lid = -2` does.
- Native raw pointers into the static object pool are represented by validated tagged words plus
  physical arena-slot storage identity. In the characterized Jaws chain, provenance follows the
  global read through process stack/register words into a newly written link; retired slots retain
  their initialized process words rather than only a transform sidecar. A retained Dark2 doctor
  pointer and Jaws of Darkness's global-six `fruit_hud` pointer therefore survive compact VM-handle
  reuse in a different slot. Jaws `FruiC`
  state 12 reads the reclaimed creator's exact `translation.x` value `0xffff3800` (`-51,200`). A
  later object in the same physical slot retargets the native pointer, while global write epochs
  distinguish reassignment of an identical 32-bit tag. Pre-existing inbound links are still cleared
  by checked object teardown and remain a broader native-lifetime parity gap.
- `LevelSaveState` and `LevelRestart` use an owned fixed-layout snapshot containing only fields the
  source actually copies. The browser preflights pager/lifecycle/camera work before irreversible
  RESPAWN/TERM delivery, then publishes restored spawn words, player transform, counters and box
  count. A same-level load nested inside `LEVEL_END` remains an explicit resumable-host boundary;
  Rust does not silently skip the remainder of that handler. A legally local scan of all 44 retail
  pairs found zero authored occurrences of this nested case.
- `LevelResetGlobals(1)` preflights and writes the documented scalar words, then clears the native
  3,592-halfword encountered-object registry. It does not own live objects, savestate or the
  separate 304-word active spawn table. Misc-ten cases four/five maintain that registry's
  zero-terminated, one-as-hole encoding and deliberately fall through to the active-table bit
  update; a destination mount rebuilds active bit eight from retained tags for its own level. Misc
  12/11 runs this reset synchronously, so later instructions in the same handler see the new words.
- Retail card restore writes `init_life_count`, executes that exact globals reset, then restores the
  128-byte payload's progression/options words and derived map/unlock mirrors. The browser's
  main-menu reset wraps the same operation in the existing resume before/after hooks. Neither path
  clears the retained savestate or separate active spawn table.
- The live retail-object bridge scans displayed neighbor zones, uses separate typed generational
  arena and VM handles, traverses the mutation-aware spawn tree, and applies synchronous runtime
  child creation inside the same 30 Hz frame. Immutable post-GOOL snapshots now feed pair-scoped
  3D vertex-object, sprite, fragment, type-four text/font and status-B 2D-CVTX rendering. Animation
  bounds follow the Crash register-frame stamp: matching objects bind before GOOL/physics, earlier
  preorder objects bind after physics only inside the native inclusive range, and rejected late
  objects set the bounds-invalid status bit. Host effects `0x83`/`0x84` synchronously refresh the
  persistent local bound, and the same-stamp tail applies Crash's asymmetric collider links and
  hotspot bookkeeping. The current link-six collider's complete live process metadata is retained
  independently of the bounded candidate slice and refreshed after each synchronous handler, so a
  rejected candidate can still leave the prior collider authoritative for later floor/wall phases.
  Flag-enabled `PlotObjWalls` now uses that same checked collision resolver for every broad
  frame-bound overlap in source candidate order; flag-zero wall replots remain collision-read-only.
  The mover link mutates during the pass, while reciprocal candidate links and hotspot writes are
  retained as ordered typed effects. Hotspot insets preserve raw `p1`/`p2` ordering even when the
  inset inverts an axis; direct source face comparisons are applied without normalizing the bound.
  Camera crossings apply checked zone
  teardown/paging/activation, and typed
  ADIO requests feed the retail SFX voice engine. Zone MIDI/INST/VAB/SEP assets decode to owned
  PCM/sequencer data, with source-timed fades and typed GOOL track toggles. The browser has no
  procedural sine fallback. Effects remain data rather than unchecked host pointers; exact SPU
  reverb/modulation, every collision edge, pixel-level rendering equivalence and full progression
  are not implied. World Lightning, combined Dark and Dark2 state/rendering are now connected with
  their process-lifetime shader scratch, shared RNG-B ordering, thunder ADIO handshake and hidden-
  frame transform behavior.
- The mount-time `LevelInitMisc(1)` transaction creates source-mapped root-four controllers for
  levels `0x05`, `0x14`, `0x16`, `0x17`, `0x22`, and `0x2e`. Ripper Roo's 39/4 controller publishes
  a checked tagged `ambiance_obj` reference in global 8. Same-level `LevelInitMisc(0)` does not
  duplicate it. `CamFollow` also reads the live authored global-65 `gem_stamp` for the
  `frames_elapsed - gem_stamp <= 15` neighbor gate rather than substituting a host constant.

## Deliberate corrections

The rewrite rejects out-of-range item offsets, cyclic/unbounded bucket walks, invalid GOOL stack
access, division by zero, oversized shift counts, unbounded collision result aggregation, bad
texture/CLUT ranges, malformed audio banks/sequences, and PBAK frame overruns. During a mutable TERM
walk, a stale captured sibling generation ends that sibling chain instead of following a C
free-list/ABA link. These were undefined or insufficiently bounded in C and are not compatibility
behavior.

No C runtime, C compatibility layer, copied header, or vendored C synthesizer remains in the
current application build. Upstream attribution is retained because its observable behavior and
format research informed this work.
