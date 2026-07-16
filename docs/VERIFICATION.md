# Verification record

This file records observed checks for the initial private rewrite delivery on 2026-07-12, the
stream, title, GOOL, entity, SLST, camera, cached-scene and hosted-runtime slices on 2026-07-13,
the title-overlay, PBAK, object-shader and current-zone collision slices on 2026-07-14, and the
later ADSR, world-ripple/dynamic-world, shared-RNG, process-animation and initial-return lifecycle
slices on 2026-07-15. It
does not turn subsystem tests into a claim of retail gameplay parity.

## Reference characterization

The Bandicoot source tree was treated as read-only at
`7f05e5febd63e603f243c089c8b9918211c7b991`. An external archive build passed 17 native C and two
JavaScript tests plus its Emscripten build. Its Chrome title sequence reached numeric states
`10 → 7 → 8 → 5` and then Intro `0x38`. The source working tree, including pre-existing untracked
forensics files, was unchanged after characterization.

## Legally local data

The opt-in local-data test and browser pass used the user's own NTSC-U image without copying it into
this repository. Detection reported Mode 2/2352, 88 streams, 44 exact pairs, and 229,312,048 logical
stream bytes. Every filename and declared extent was matched against the extracted S0–S3 set.

Read-only scene characterization covered 1,223 ZDAT entries, 1,735 paths, 520 WGEO entries and
1,726 SLST entries containing 138,038 items. All 43 playable LDAT spawn zones/paths resolved. The
static scene builder produced world commands for 40 starts; Title, Hog Wild and Whole Hog use
zero-world dummy starts with external SLST placeholders. Exhaustive mutable-SLST characterization
covered 1,726 resolved paths, 136,312 visibility states, 134,586 adjacent transitions, 269,172
forward/backward inverse round trips and 89,666,970 validated polygon references, with fingerprint
`0x1400935c08cfe148`.

All 4,292 retail ZDAT entities and 16,363 signed entity path points parsed; every entity was in
group three. The data contained 52 main-object candidates, 624 valid executable/subtype bindings
and seven bindings that the retail program loader also rejects, with fingerprint
`0x71524c62fcbf6ddb`. N. Sanity Beach's progress-zero baseline remained 4 worlds and 681 visible
polygons. The observed first-presented state at path point two/draw count one produced 679 visible
polygons from both the extracted pair and directly from the raw BIN.

The object-format sweep parsed 6,738 candidate GOOL animation payload offsets across 441 globals:
4,397 vertex, 1,813 sprite, 221 font, 62 text and 245 fragment descriptors. It found 1,391 TGEO
occurrences (281 EIDs, 328 exact variants and 55,950 polygons), 30,011 SVTX/CVTX frames and
validated all 29,611 pair-resident frames containing 42,983,073 vertex references. Four hundred
dormant frames consistently named one cross-pair EID and were retained as controlled unavailable
assets rather than resolved through another mounted pair.

The type-19 PBAK census found exactly nine recordings and 10,966 controller frames. Eight use the
304-spawn-word layout; the Upstream recording uses the observed 511-word layout and its extended
frame offset. The checked browser adapter prepared all nine, validated each recorded level/path,
accepted Upstream only because its extra active-spawn tail is zero, and preserved the one legal pad
word containing a bit above the 16-bit physical-controller range. A separate raw-BIN corpus test
also bound the native executable-four/subtype-eight caption controller in every one of those nine
pairs, retained its null lifecycle zone, and rejected no program or environment lookup. The exact
non-advancing timing sweep covered all 10,966 frames. It separately verified the start-frame split
(wall timing through root one, wall tick count plus header TPF at Crash), later recorded frames at
`(17, recorded TPF)`, and returning frames at `(17, rounded wall TPF)`.

The real N. Sanity Crash program was then executed through its first retail host boundaries. Tests
verified absolute global call word `0x8609806e` to global PC 110, return at global PC 131, the exact
optional-pointer word `0x16be0e1f`, and both initial child-spawn requests with synchronous callbacks
and argument cleanup in one 67-instruction invocation, without treating any serialized word as a
native pointer. A negative dynamic child count was also verified as a non-spawning argument-pop
rather than an overflow error.

## Hosted retail-runtime slices

The 2026-07-13 through 2026-07-15 opt-in tests used the same legally owned raw BIN in place and did
not copy any disc or stream bytes into the repository:

- The fractional camera/scene test discovered the complete retail catalog and successfully built a
  signed-8.8 progress snapshot for all 43 bootable pairs directly from the raw image. The three
  external-transition/dummy starts remained valid zero-world scenes rather than synthetic geometry.
- The N. Sanity runtime bridge scanned the displayed current-zone neighbors, attempted seven
  group-three entity spawns, bound all seven, and executed them through the shared typed arena/VM
  runtime. Crash synchronously hosted the characterized executable `5` and ShadC executable `29`
  children with their retail argument lists. Deterministic integration tests also verify
  zone-relative entity path position, rotation/mode flags, `0x1000` scale, subtype/PID/path/process
  defaults, player-vs-object color matrices and child transform inheritance.
- Parsed programs retain the complete checked item-four state table. State links apply the retail
  `status_c`/target-flags guard (including the `0x1002` invincibility augmentation), while initial
  and global-call frames share the process/register word array at `init_sp`. Code PCs, storage
  indices and entry slots use aligned checked tags; animation references intentionally remain byte
  offsets. Focused tests cover argument addressing, packed frames, frame-relative access and links.
- State rebind captures and clears the once pointer, runs its nested code synchronously before the
  state stamp, then runs the target external transition block after the stamp. Nested calls/returns,
  animation selection and hosted child spawns preserve this order; target state code resumes on a
  later object execution.
- Focused native tests prove `GoolObjectColors` delivers the category-`0x300` collider's authored
  `0x0a00` hit synchronously before physics. An event interrupt changes Crash's X velocity and the
  same 34-tick update moves him to X=544; its frame-relative argument is the checked zero word used
  in place of the source's `argc=1`/null pointer. The hosted path leaves no duplicate queued effect,
  malformed handlers enter `RuntimeInvincibilityEventFault` while physics still completes, and VM
  slot plus arena-generation reuse cannot mutate a replacement object.
- Paging opcode `0x8b` cases one through six reproduce the checked reference-count/query behavior
  with explicit page/entry metadata. Opcode `0x1a` reads the same five-word pad history installed by
  the browser. The legal trace also crossed `0x85` suboperation zero path orientation, `0x8e`
  suboperation six entity colors, and the source-defined suboperation-three and suboperation-one
  solid query branches using validated ZDAT octrees and colors.
- The NSF host resolves a collidable object's current unaligned vertex animation/frame into a
  pair-scoped bound source. Focused cases cover per-frame clearing, 96-bound capacity, Crash-stamp
  pre-GOOL registration, range-gated post-physics registration for objects visited before Crash,
  status-A invalidation on a late-range miss, and synchronous local-bound refresh through `0x83`
  and `0x84`. The same-stamp tail has focused coverage for Crash's asymmetric accepted/priority
  collider links, hotspot `0x1000`, and target-collider clearing on a miss. The previously recorded
  300-frame N. Sanity trace crossed the former ShadC executable 29/state-one boundary without
  reproducing the C branch's uninitialized locals. The current legally local scene/runtime goldens
  pass under the revised schedule.
- All 43 playable pairs built owned pointer-free camera graphs. Every non-title boot pair then ran
  300 automatic-camera ticks through one pair-scoped scene builder: 42 pairs and 12,600 exact
  camera-to-scene zone/path/point/draw identities passed with zero failures. N. Sanity's opening
  automatic chain crossed four paths in 192 ticks; a separate legal `CamFollow` golden projected
  its 43-point mode-five path and crossed to path five from a supplied retail player transform.
- A combined N. Sanity camera/GOOL/object-scene trace ran 300 frames from seven successful initial
  entity spawns and 14 live render-object snapshots. Its peak presented scene contained four
  worlds, five visible 3D objects, 621 world polygons and 568 submitted object polygons, with 63
  shared decoded textures, zero undeclared or skipped object texture references, 84 saturated
  object polygons and 444 face-culled polygons.
- The variable-layout type-three font sweep found 62 validated text/font pairs containing 1,257
  terms. All terms passed bounded four-argument formatting and projected safely into 4,372 glyph
  or backdrop quads, while 64 representative glyph textures decoded. This includes all 32 CardC
  controller-icon terms (`c`, `s`, `t`, and `x`) through their validated 90-record font rather than
  an unchecked C array overrun.
  Across 42 non-title idle boots, 531 live type-four text frames emitted 3,894 textured quads; the
  same trace also exercised 50,714 sprite and 177 fragment frames. No dynamic-font override became
  live in that idle window. A separate legally local Great Hall ending-route golden now executes
  the authentic `WinGC` program with its retail child arguments and verifies the first two display
  boundaries: the hidden first frame records the authored override and the visible frame preserves
  it with its `PAPU PAPU:` text state. The descriptor default resolves to `Fon0T` and that override
  resolves to `Op2pT`. This characterizes the real dynamic-font path without claiming that the idle
  boot naturally reaches the ending route.
- The exact title MDAT runtime test loaded the legal title pair directly from the raw image and
  confirmed type-17 source-vs-object-zone binding, source-ordered state changes and the type-zero
  display masks `0x22_3ff0` at load and `0x22_3ffc` when active. Focused tests cover the complete
  nonlinear overlay-alpha sequence, including opaque blank/swap phases and the pre-quantization
  counter step used by the WebGL pass.
- The previously recorded strict goal-directed N. Sanity survey exited at frame 1,995 through the
  authored `Transition(0x2d)` Level Complete warp. It recorded 74 successful spawns, 40,480 GOOL
  executions,
  18 zone transitions, four save handshakes, zero unexpected spawn failures, zero VM errors, zero
  faulted objects and no death restart, below-zero player position or terminal-fall velocity.
  State-aware forward/jump/spin/steering input carried Crash through
  `e0_9Z → a0_9Z → a1_9Z → … → b7_9Z`. The survey includes the three native root-one HUD
  controllers, so that completed route exercised the same process-lifetime object infrastructure as
  the browser mount. These counts remain a prior-artifact baseline. An intermediate
  revised-controller run stopped at the b5/b6 boundary, but that stop is not the current result;
  the corrected-input current run is recorded in Current change-set verification below.
- The mount-time core-object corpus test materialized executable-four subtypes 0, 1, and 5 from
  `DispC` in all 39 eligible legal pairs, verified native creation/preorder, null lifecycle-zone
  identity and exact tagged globals 7/6/14, and verified that title `0x19`, level complete `0x2d`,
  intro `0x38`, and ending `0x39` create none. The all-pair live renderer trace then exercised
  1,800 mode-four vertex displays and emitted 2,880 mode-four object primitives while Lights Out and
  Fumbling in the Dark consumed the live player reference and darkness distance. Of those displays,
  540 produced changed shader colors and the trace verified every result both in the effective
  render snapshot and persisted in the live VM. Focused tests
  verify that the reference is sampled separately around the root-six player update, that native
  target/step/current darkness survive level reinitialization, and that all five renderer-BSS words
  retain their exact first-tick behavior across a fresh stream runtime. Additional focused cases
  exercise modes two/three live writeback, post-update/pre-child color visibility, the
  main/display/status/CVTX/near-plane gates, the status-B `0x100000` split between restored VM
  colors and retained effective render colors, and native's null-object-zone fallback.
- All nine PBAK recordings completed full live simulation/render traces across their 10,966 Crash
  pad boundaries. Papu Papu's recording exercised an authored same-level death/restart before
  continuing to its final input handshake. These traces honor display-mask camera selection,
  apply camera-emitted zone TERM/lifecycle transitions and save handshakes, refresh
  live box/checkpoint globals, preserve the final recorded pad word, and return through the checked
  caption-controller path.
- A separate strict 360-frame Hog Wild idle trace completed with 713 GOOL executions, zero
  execution errors, zero faulted objects and no checked issues. Crash retained the typed detached
  object zone `0c_hZ`; its rectangle, graphics and water fallback remained available without
  adding detached octree geometry. The authored solid event `0x900` entered death state 22, the
  signed display fade reached `-2` then `-1`, and `LoadState` completed same-level restarts at
  frames 179 and 356. No below-zero or terminal fall remained. This verifies the idle
  death/restart loop, not steering or level-completion parity.
- An isolated legally-local Intro test held the shipped no-link terminal path for 64 GOOL frames
  without inventing a transition, confirmed the main controller stayed in state 15, injected its
  first fresh `PAD_START` tap, observed state 16, and received the authored `Transition(0x19)`
  within four frames.
- Focused renderer tests exercise mode-two dual color ramps and cutoff, mode-three SVTX fade and
  CVTX shift/cutoff, mode-four lighting and malformed-coordinate rejection. Web scene tests confirm
  all three modes are gated into live object rendering and that graphics flag `0x1000` substitutes
  the Q24.8 bobbing/fixed-pitch camera for objects only.
- The exact Jungle Rollers `pb0cB` integration trace runs all 1,348 recorded pad boundaries, builds
  every non-restart scene, and checks every contained object execution. Frames 189–210 cover two
  consecutive `FruiC` physical generations reusing compact VM slot 17, pin their exact
  scale/state/animation-stamp sequence, and confirm that no such child remains on frames 211–217.
  Separate renderer goldens cover raw sprite shifts 24, 26, 28, 31, 34, 246, 271 and 297 with their
  low-five-bit effective values.
  Separate runtime coverage verifies that the caption's executable-four/subtype-nine child keeps a
  null lifecycle zone while using the current ZDAT for environment/colors. The direct-mount
  fixture samples a zero island-camera target at the final boundary, returns `Released`, and leaves
  PBAK state zero. Separate finish-contract coverage verifies that a nonzero carried target
  completes the caption handoff and retains state three; the full local-scene trace rejects a
  checked caption-handler fault whenever that branch is used.

These are native, ignored-by-default local-data tests. They characterize the mounted retail data
and runtime boundary; they are not evidence of a browser playthrough or full GOOL parity.

## Browser checks actually performed

The checks below are dated evidence for the artifacts named in each paragraph. Earlier diagnostic
flows and oscillator-backed SFX are not evidence for the current authored-only browser flow or its
mounted-data-only audio path.

A generated release Wasm build was served on `127.0.0.1` and exercised with agent-browser 0.27.0
using its Chrome engine:

- Wasm bootstrap reached `running`; `__consoleErrors` stayed empty and WebGL reported error zero.
- Raw 632,083,536-byte BIN discovery mounted all 88 streams and exposed all 43 bootable pairs.
- The 88 extracted NSD/NSF files independently mounted all 44 pairs; direct boot of level `0x09`
  parsed 80 pages and 231 entries.
- Title/publisher progression reached main menu; Intro `0x38` and return were observed.
- Options changed SFX volume, and password and empty-card load screens were entered.
- Direct gameplay boot, keyboard movement/jump input, pause, mute, fullscreen, and WebAudio resume
  activity were exercised. Automatic resume restoration was observed after reload.
- A 393×852 responsive viewport rendered the complete touch pad, including shoulders, L3/R3,
  Start and Select. Touch mapping itself is covered by native tests; multitouch on physical mobile
  hardware was not performed.
- Unsupported input was rejected with an actionable error after a browser-discovered fix.
- The final network trace contained only same-origin HTML, CSS, bootstrap JavaScript, generated
  wasm-bindgen JavaScript and Wasm. No asset upload/request occurred, and there were no console or
  page errors.

The 2026-07-13 release build was then exercised in a fresh agent-browser 0.27.0 Chrome session:

- A 632,083,536-byte raw BIN was selected through the real file input. Client-side discovery
  produced 88 files, all 44 pairs, 43 boot targets, and 229,312,048 logical bytes.
- The data-backed flow mounted `0x19 → 0x38 → 0x19 → 0x03 → 0x2D → 0x19 → 0x03`. The debug surface
  confirmed that current and retained stream IDs converged after every asynchronous swap. Observed
  destination counts included 113/558 pages/entries for `0x19`, 41/149 for `0x38`, 77/304 for
  `0x03`, and 15/57 for `0x2D`.
- The host decoded and uploaded 432×144 retail loading images for `0x19` and `0x03`. The opt-in
  local renderer characterization decoded all 39 loading images present among bootable streams.
- Title cards, main menu, Options and Map were navigated with keyboard input. Live options changed
  SFX and music from 255 to 239 and toggled mono; the then-present diagnostic SFX path produced
  measured mixer peak 767. That path has since been removed and is not ADIO audition evidence.
- Keyboard completed the diagnostic movement goal once. A second run held the on-screen touch Up
  control, reached completion in 2.24 seconds, cleared its held visual state, and mounted `0x2D`.
  Pause/resume and mute/unmute changed live runtime state and were restored before continuing.
- A 390×844 viewport had zero horizontal overflow and displayed the complete touch controller. In
  that historical diagnostic build, completion created card slot zero. After reload, both
  versioned storage records remained, while the file input was empty and mounted-pair count returned
  to zero.
- WebGL reported error zero throughout. Console and page-error logs were empty. The network record
  contained only same-origin HTML, CSS, bootstrap JavaScript, generated wasm-bindgen JavaScript and
  Wasm; no game-data request or upload occurred.
- The exact final Wasm artifact was re-smoked with only the extracted `0x19` pair. It mounted
  113 pages/558 entries with WebGL error zero; when the title flow requested absent `0x38`, the
  simulation stopped as `BLOCKED`, retained `0x19`, and displayed the missing local filename rather
  than advancing or presenting the destination against stale assets.

The final camera/GOOL/cache artifact was rebuilt and reloaded at `http://127.0.0.1:4174/` in the
visible Codex in-app browser. Its DOM contained the complete local loader, disabled pre-mount runtime
controls and canvas; there was no framework error overlay and the captured console log was empty.
The response used `Cache-Control: no-store`, and the served Wasm hash matched the generated file.
Browser automation in this environment could not populate the operating-system file chooser, so
this exact artifact's raw-BIN import, hosted CamFollow and WebGL game scene are not claimed as
browser-exercised. The same BIN was exercised in place by all opt-in Rust tests described above. A
user can select it through the visible local-file control without changing the no-upload model.
The object/camera/scene path is connected in code and compiled to Wasm, but no completed retail
gameplay flow is claimed.

The 2026-07-13 object/bounds release was also loaded in a foreground Google Chrome session through
macOS computer control. The native chooser selected the user's exact 632,083,536-byte BIN; the page
recognized 88 files, all 44 pairs, 43 boot targets and 219 MiB of logical local stream data. That
real default-title launch exposed a one-point dummy-path bug: initial presentation requested path
point one and rejected the pair. A focused regression now clamps only the requested initial
presentation point to the validated final point. After rebuilding, reloading and reselecting the
same BIN, the Rust runtime started at title state 10, reached main menu state 5, accepted keyboard
Cross, entered island-map state 15 and reported the then-active synthesized-audio path. The game data stayed in
the browser tab and no repository file or browser storage record received disc bytes. This session
did not inspect DevTools network/console panes and is not described as a retail gameplay
playthrough.

Screenshots and game data remained outside Git. See `COMPATIBILITY.md` for features that were not
exercised or are not yet connected to the live browser runtime.

The 2026-07-13 lifecycle/audio build was then tested against the same legal BIN after adding
source-ordered zone transitions, synchronous event/audio host calls and local ADIO SFX. Both
`local_retail_runtime` goldens passed: N. Sanity's exact zone band/load-list transition and its
seven initial objects, Crash's executable 5/29 children, native handle reparenting, solid snapshot
boundary and first-frame checked GOOL executions. The release Wasm was rebuilt and reloaded in the
visible in-app browser; the loader reached its Rust-ready engineering-log state and the server
returned HTTP 200 with `Cache-Control: no-store`. The operating-system chooser still required the
user's click, so this newest retail-SFX build is not claimed as manually auditioned.

The 2026-07-14 current runtime slice was rebuilt, served with `Cache-Control: no-store`, and loaded in
a foreground Google Chrome session. The native chooser selected the user's supplied raw BIN in
place; the page reported 88 files, all 44 pairs, 43 playable pairs and 219 MiB of logical local
stream data. The Rust/Wasm runtime rendered the Naughty Dog card, the real main menu and Intro
`0x38`. Intro advanced through its authored camera chain and held the terminal no-neighbor frame
without the previous camera error while the monitor remained `RUNNING` at 30 Hz with synthesized
audio active. Disc bytes remained local to the browser tab. DevTools network/console panes were not
inspected in this foreground pass, so the visible engineering log and successful rendering are the
only browser error evidence claimed here.

The final 2026-07-14 renderer/PBAK build was then reloaded in a foreground Chrome 150 session and
the same 632,083,536-byte legal BIN was selected through the native file chooser. Client-side
discovery again reported 88 files, all 44 pairs, 43 playable pairs and 219 MiB, with no upload path.
The authored Naughty Dog/title sequence reached the real menu, timed out to Intro `0x38`, accepted
a fresh Return/Start edge back to title, and entered the Jungle Rollers `pb0cB` attract path at
`0x0C`. The final run remained `RUNNING` beyond both the caption-child and raw sprite-shift
regressions. At retail frame 1,601, the live debug surface reported zero execution errors, zero
faulted objects, no warning, no runtime error and WebGL error zero. The visible tab was left running
the real local-data scene with synthesized audio enabled.

After the pre-animation-bound core-object/dark-shader build, the release files were rebuilt behind the existing
`127.0.0.1:4174` local server and reloaded in the visible Codex in-app browser. The mount UI reached
`Awaiting local media` with the expected 43-target selector and no captured browser warning or
error. The BIN was not reselected in this final reload, so the foreground Chrome run above remains
that checkpoint's latest claimed end-to-end local-disc browser exercise.

The authored-only browser-flow and mounted-data-only audio changes, together with the revised
Crash-stamp bound/collision schedule, were rebuilt and reloaded in the visible in-app browser on
2026-07-14. The fresh release reached `Awaiting local media`, exposed the 43-target disabled selector
with zero mounted pairs, and produced no captured console warnings or errors. The operating-system
file chooser still cannot be populated by this automation surface, so the BIN was not reselected;
no current browser-audio or end-to-end gameplay result is claimed for this artifact.

## Recorded pre-animation-bound automated baseline

The results below belong to the dated artifact described above. They are retained as prior evidence,
not as current-change-set counts or hashes.

- `cargo fmt --all -- --check`: passed.
- workspace Clippy with `-D warnings`: passed.
- locked native workspace suite: 693 asset-free tests passed, zero failed; 49 legally local tests
  remain ignored by default (742 tests total across all targets).
- post-fix legal-data gates passed against the supplied raw BIN and read-only extracted streams:
  the complete opt-in workspace sweep passed 49 tests with zero failures, including the final
  Jungle Rollers PBAK scene regression;
  88 exact streams/44 pairs and 229,312,048 extracted bytes; all nine PBAK recordings/10,966
  controller frames; the exact title-MDAT runtime check; all 43 bootable pairs for 360 strict
  frames; the N. Sanity authored Level Complete route; the focused Hog Wild death/restart trace;
  and the Intro terminal-camera and all-nine PBAK caption bindings.
- the previously recorded 24-test opt-in sweep passed with `C1_DISC_IMAGE` and `C1_STREAM_DIR`:
  raw-disc/catalog,
  all-pair parsing, entity/program binding, GOOL graph/boot execution, exhaustive SLST traversal,
  animation descriptors, object-model formats, hosted N. Sanity execution and object projection,
  three camera goldens, scene formats, 12,600 camera-driven scenes, all 43 fractional boot
  snapshots, 40 standalone snapshots, 39 loading images, 1,427 representative texture references
  and all four image-backed title states. The new expanded two-test N. Sanity lifecycle/runtime
  target also passed after the current changes. The entity and scene-format tests passed using the
  raw BIN alone in the earlier sweep.
- locked optimized native workspace build: passed.
- locked optimized `wasm32-unknown-unknown` web build: passed.
- generated web release: passed; Wasm payload was 1,216,158 bytes (SHA-256
  `4c45cd45e9af827fa4d252d67fffbcfb9db7713e88e2b0492414db45cfbaa6ea`).

## 2026-07-14 change-set verification

The final checks below were run against this change set on 2026-07-14:

- `cargo fmt --all -- --check` passed.
- locked workspace Clippy across all native targets and an explicit `wasm32-unknown-unknown`
  `crust-web` Clippy pass both completed with warnings denied.
- the locked asset-free workspace suite passed all 749 default tests across 33 targets. Another 57
  legally local tests remain ignored by default, for 806 listed tests across all targets.
- the complete legally local ignored sweep used the supplied raw BIN and read-only extracted
  streams in place. All 57 selected tests passed with zero failures, including every
  raw-disc/catalog, all-pair parser, camera, title, audio, PBAK, renderer and runtime golden.
- the exact island-map WGEO item-three golden passed against title pair `0x19`: four lists contain
  42 group records and 368 polygon records with fingerprint `1c1c2ddfb2c7c7ab`. Focused scene
  tests prove groups carry across worlds, globals 73/75 select the 64 masks, the source WGEO remains
  immutable, and the last effective masks persist through map fade-out until the graph changes.
- title-runtime tests preserve `GOOL → TitleUpdate → TitleLoadState → GLUpdate` with a passive
  browser flow mirror. An explicit immediate-draw latch proves native's opaque overlay remains
  black both when fade-out reaches exact zero and when a newly loaded screen synchronously requests
  another fade-out in the same update.
- GOOL `0x14` tests and the legally local Toxic Waste `BaraC` golden prove input-before-output LEA
  translation and a checked same-object type-zero no-draw animation with its non-vertex collision
  bound. Opcode `0x81` is covered as the native interpreter's one-cycle no-op, and a 1,200-frame
  Ripper Roo run exercises its mount-time executable-39/subtype-four controller without faults.
- executable-`0x22` crate coverage now checks the native strict adjacency boundary, checked
  bidirectional misc-A links, skipped-lower-crate Y compaction, activation/restart reset, stagger
  calculation and stale-reference cleanup before VM-handle reuse. The opt-in local golden confirms
  that authored N. Sanity `a3_9Z` entities 23 and 24 are linked in both directions.
- the corrected legally local 2,100-frame N. Sanity invocation passes. Its controller follows
  `b5_9Z:p4 → b5_9Z:p1 → b6_9Z:p0`, reaches `b7_9Z`'s `WarpC`, and emits the authored
  `Transition(0x2d)` at frame 1,900. It records 18 zone transitions, 42 observed paths, 65
  successful spawns and 40,881 GOOL executions with zero restarts, below-zero or terminal falls,
  VM errors or faulted objects. The former b5/b6 stop was caused by missing test-controller route
  actions at authored static cells; a later b7 stop came from steering `LEFT` around the live portal
  lane. Correcting those inputs required no camera or collision runtime change. Restoring the
  source's flag-enabled `PlotObjWalls` collision calls accounts for the current timing and early
  interaction changes. This deterministic
  local test is not a browser playthrough or a claim of full retail parity.
- `authored_n_sanity_completion_title_vertical_flow_preserves_session_carry` passed against the
  legally local stream directory at this checkpoint. It reached N. Sanity's authored
  `Transition(0x2d)` at frame 1,900, finished the outgoing `LEVEL_END`, imported the resulting
  `RetailSessionCarry` into Level Complete, reached that pair's authored `Transition(0x19)` at
  frame 513, finished the second `LEVEL_END`, and imported its carry into Title. The destination
  contexts were seeded from carried globals 62, 69, and 102–104; both broadcasts reported zero
  checked handler failures, and Title's intentionally empty gameplay-core frame completed with no
  effect or fault. This proved the simulation's first three-pair completion handoff, not an
  end-user browser playthrough.
- after native null-zone lifetime correction, the legal Jungle Rollers PBAK scene test no longer
  follows a reclaimed `FruiC` incarnation. It pins arena generations nine and ten across their
  shared compact VM slot, exact authored scale/state/stamp sequence on frames 189–210, and absence
  after frame 210. The MIPS low-five-bit shift behavior remains covered independently.
- native pause unit tests cover the exact level/title/PBAK gate, root-seven
  executable-four/subtype-four creation, tagged global word 12, the category/type/live-process-subtype
  update allow-list, Crash-boundary hook invocation while ordinary updates are suppressed, frozen
  draw count, `0xC00` resume clock rewind and synthetic controller/audio cleanup, checked-fault
  diagnostics, nonfatal controller-create failure, and screen-load reset ordering. The end-to-end
  START pause/resume path and visible authored controller panel were exercised in the browser below;
  exact prior-pad latency and per-object paused execution are not claimed by that UI check. A
  legally local scene regression additionally proves that `DispC` state six selects the type-five
  `WillT` descriptor at byte offset 136, emits five far-depth fragment quads with no skips for the
  first 15 paused frames, hides them for the next 15, and repeats.
- native object-display tests cover the source preorder boundary: modes two through four write
  derived colors after parent update and before child execution, while the display snapshot keeps
  its effective colors independently of the status-B `0x100000` object/player-zone reset. The
  legally local all-pair regression exercised 1,800 mode-four vertex displays and 2,880 emitted
  primitives; all 540 changed shader results matched the fixed-point evaluator and persisted in
  the live VM. The snapshot now also latches animation/frame, transform, process flags, text
  arguments/font, darkness and the live object display mask. Focused linked-child coverage proves
  that a later child write cannot retroactively mutate its parent's render state, while an actual
  global-nine write proves that world geometry keeps the pre-GOOL mask and later objects consume
  the traversal-time mask.
- the object-only graphics-flag `0x1000` camera has a cross-crate fixed-point golden covering its
  direct pitch matrix, fixed/bobbing translation and camera-space point. A separate clock test
  proves GOOL `frames_elapsed` advances while texture `draw_count` is frozen; scene locations carry
  both values so hidden/loading frames cannot desynchronize shading from geometry.
- locked optimized native and `wasm32-unknown-unknown` workspace builds passed, as did the generated
  web release. The Wasm payload is 1,243,981 bytes with SHA-256
  `ed8d36dd0229ed44312980dea7f418495a6eaa3af5f05c6c992f0b213e48b2f4`.
- the no-store server returned HTTP 200 at `http://127.0.0.1:4174/`. A release candidate containing
  the route and pause integration was loaded in the visible in-app browser. Because that browser
  cannot automate a native file picker,
  an ephemeral loopback-only same-origin test route wrapped the supplied 632,083,536-byte BIN in a
  browser `File` and dispatched the ordinary local-input change event; it was removed by rebuilding
  the clean release afterward and is not part of the repository or production server. The importer
  recognized all 88 streams and 44/44 level pairs (219 MiB of selected stream extents). Authored
  title boot rendered the Naughty Dog publisher card and advanced by touch START to the island map.
  Direct N. Sanity boot rendered live world/object geometry at 30 Hz with synthesized audio active.
  Touch START and keyboard Enter both opened and resumed the native pause controller, with telemetry
  changing `RUNNING → PAUSED → RUNNING`; paused scene presentation continued and resume restored the
  world frame. A follow-up on-cycle capture showed the decoded `WillT` fragment panel clearly as
  `PAUSED / PUSH SELECT FOR MAP`; the earlier unreadable capture had landed in its authored
  15-frame hidden half-cycle. The visible-phase check reported no browser warnings or errors. No new
  console warning/error appeared after the successful same-origin mount and gameplay checks; the tab
  retained one earlier failed cross-origin bridge probe in its historical log, before the successful
  route was used. The clean final artifact was then rebuilt after the temporary route and follow-up
  pause field/error corrections; its served hash matches the hash above, the removed route returns
  HTTP 404, and the already-loaded gameplay tab was deliberately not reloaded so its in-memory local
  BIN selection remained visible.

  The final fixed-camera/object-shader artifact was exercised once more through the same ephemeral
  loopback-only bridge. It recognized 88 streams and 44/44 pairs, launched title `0x19`, rendered
  the retail tent scene and then the island map with the N. Sanity Beach card, and continued at
  30.00 Hz with mounted synthesized audio active. The UI pause and mute controls each changed live
  telemetry and were restored to `RUNNING`/`SYNTH ACTIVE`; the in-app browser warning/error log was
  empty. Keyboard injection was not repeated successfully in this final in-app-browser pass and is
  therefore not newly claimed for this artifact. After the tab had retained its local `File`, the
  temporary bridge was deleted, the ignored release directory was rebuilt from the checked-in
  sources, and the normal no-store server was restarted. Its root returns HTTP 200, the removed
  bridge returns HTTP 404, and the visible tab remains on the in-memory island-map scene. No game
  bytes entered Git, browser persistence, or the repository working tree.

  The traversal-snapshot/display-mask artifact was then rebuilt and loaded in a fresh in-app
  browser page. The supplied raw BIN again resolved 88 streams, 44/44 pairs and 219 MiB of selected
  stream extents. Title boot rendered the publisher screen at 30.00 Hz with synthesized audio, and
  its mute control changed telemetry to `MUTED` before being restored to `SYNTH ACTIVE`. A second
  fresh mount directly booted N. Sanity Beach, rendered its live world, Crash and crate geometry,
  and changed `RUNNING → PAUSED → RUNNING` through the native pause controller. The page reported
  no warning or error console entries. The loopback-only mount bridge was stopped and deleted after
  dispatching the ordinary local-input event; the normal no-store server was restored, its root
  returns HTTP 200, the removed route returns HTTP 404, and the visible tab remains on the in-memory
  N. Sanity scene. Keyboard, physical gamepad, fullscreen and touch presentation were not repeated
  in this pass.

  The authoritative-save and inline-invincibility-event artifact identified by SHA-256
  `28acb47b304456f613dc15c2a4843384cf67839d719bde48d9b35f80d1c578a3` was then exercised through
  the same local-only bridge. The supplied BIN again resolved 88
  streams, 44/44 pairs and 219 MiB of selected extents. Title `0x19` rendered its authored scene at
  30.00 Hz with synthesized audio active; a fresh direct `0x09` mount rendered N. Sanity Beach's
  world, Crash and crate geometry. UI pause and mute each changed telemetry and were restored to
  `RUNNING` and `SYNTH ACTIVE`. The browser warning/error log was empty. After the tab retained its
  local `File`, the bridge was stopped and deleted, the ordinary no-store server was restored, its
  root returned HTTP 200, and both temporary route names returned HTTP 404. The visible tab remains
  on the in-memory N. Sanity scene. No game bytes entered Git, browser persistence or the repository
  working tree; keyboard, physical gamepad, fullscreen and touch presentation were not repeated in
  this pass.

  The source-ordered title/controller/LEA/island-map artifact identified by SHA-256
  `ed8d36dd0229ed44312980dea7f418495a6eaa3af5f05c6c992f0b213e48b2f4` was loaded in a fresh visible
  in-app-browser tab. The supplied 632,083,536-byte raw BIN again
  resolved 88 streams, all 44 pairs and 219 MiB of selected extents. Title `0x19` rendered its
  authored tent/island scene at 30.00 Hz with synthesized audio; the source runtime subsequently
  entered Intro while idling. A fresh direct `0x09` mount rendered N. Sanity Beach's world, Crash
  and crate geometry. UI mute/unmute and native pause/resume changed telemetry and were restored to
  `SYNTH ACTIVE` and `RUNNING`. The warning/error console log was empty. The loopback-only test
  bridge was stopped and deleted, the normal no-store server was restored, root returned HTTP 200,
  and both temporary routes returned HTTP 404. The visible tab retains the in-memory local `File`
  and running N. Sanity scene. Keyboard, physical gamepad, fullscreen, and touch presentation were
  not repeated in this pass; no game bytes entered Git, browser persistence, or the repository.

## 2026-07-15 runtime/card/camera/font checkpoint verification

The release gate completed against the current source without copying legally local game data into
the repository:

- the locked default workspace suite passed 785 tests with zero failures and 63 opt-in tests
  ignored; the separate full legally local run executed all 63 ignored tests with zero failures;
- workspace Clippy passed with warnings denied across all native targets, and `crust-web` Clippy
  passed with warnings denied for `wasm32-unknown-unknown`;
- locked optimized native and `wasm32-unknown-unknown` builds passed, as did `npm run build`;
- the generated Wasm is 1,263,308 bytes with SHA-256
  `3f333d8f0d311abde88cf42a7958b38ee1a4311099115f27d3b73556e8ebc357`; the generated loader is
  46,236 bytes.

The focused checks below were also completed:

- Fixed-point SPU ADSR unit goldens passed for attack/decay target transitions, sustain direction,
  linear and exponential modes, slowdown strictly above the `0x6000` exponential-attack threshold,
  the minimum nonzero high-rate counter increment, all-one frozen rates, key-off/release, and Q15
  bounds. The sampled-voice integration test proves ADSR gain is applied
  before each mix sample and that note-off enters the hardware release phase. The legally local
  VAB/SEP bank and raw-disc audio checks also passed. Gaussian interpolation goldens cover all 256
  phase coefficient sums, ignored low counter bits, signed extreme inputs, zero-filled key-on
  history, retained loop-edge history, one-shot completion, repeat reset, the `0x4000` pitch cap,
  and positive/negative final mixer samples. An arbitrary PCM/cursor property test keeps malformed
  states deterministic and bounded. Filtered-ADPCM goldens additionally prove that a repeating
  block is re-decoded from the preceding loop-end predictor pair and that re-keying clears that
  pair; a one-shot End+Mute stops after its final block without an ADSR-release tail. The legally
  local 44-pair census found 194 unique ADIO payloads: 14 repeat, including 13 nonzero-filter loops
  whose second pass differs from decode-once PCM. It found 296 unique VAB waves: four repeating
  waves, all four nonzero-filter and second-pass divergent. One referenced wave (present in two
  banks) has no end flag and was verified to reach an end flag in the following contiguous wave.
  Sony NRPN region-loop tests cover finite total-pass/delay semantics, indefinite value 127,
  retained live voices/controllers, rewind, and an empty-region no-spin boundary. The sequence
  census pins 42 MIDI entries, 64 sequences, 98,067 events and 778 tones; conversion produces all
  six loop starts and four loop ends. It also confirms zero nonzero vibrato/portamento fields and
  zero pressure events. No corpus bytes enter the repository. This is not proof of reverb,
  unobserved modulation, SPU-RAM IRQ/manual-repeat-register timing, or shared 24-voice SFX/music
  arbitration.
- Process-local animation tests passed for complete bounded type-one through type-five payloads in
  same-object internal and register storage, plus type-zero/unknown native no-draw
  behavior. Truncation cases and a 256-case arbitrary-word property test produced checked results
  without panics. The legally local corpus preserved one descriptor of each defined type through a
  register alias. Its direct-LEA census found 31 authored writes to `anim_seq`: 18 static type
  `0x73`, 12 static type `0xef`, and Toxic Waste's one dynamic type-zero `BaraC` source. Operand
  classification pins those as 30 internal and one frame-relative source, with zero external,
  immediate, linked-register, object-register, stack, or null sources. It found no naturally
  selected process-local type-one-through-five route, so their browser render wiring is compile-
  and model-covered rather than claimed as an observed playthrough. Focused lifetime tests prove an
  immediate alias observes later writes to its exact shared rotating-constant slot and a linked
  alias preserves its descriptor while the physical pool slot is free before following same-slot
  reuse. External-state and unbound logical foreign-object aliases still fail without mutation;
  never-used indeterminate free-slot cells and truncated known-type constant payloads fail at the
  same checked boundary.
- The ripple-state test matched an iterative source model for all 16 signed cells over 2,048
  advances at each of the three speed/period combinations and proved no-advance calls preserve the
  exact state. The legally local Upstream regression confirmed its initial world carries graphics
  flag `0x100`; with the camera held constant, one unpaused submission moved visible effect-marked
  WGEO geometry while ordinary visible geometry stayed fixed, repeated paused submissions stayed
  fixed, and a hidden-world gap consumed no wave advance. No fresh fog or lightning parity claim
  follows from this ripple check alone; the separate fog and dynamic-world evidence is recorded
  below.
- The parsed-retail unit fixture executes raw RETURN word `0x82894000` through its initial frame and
  reports `InvalidInitialReturn`; the synthetic `VmObject::new` compatibility case still reports
  `Halted`. The legally local Ending regression then ran the browser-ordered spawn/camera/GOOL loop
  through the complete authored credits request. It anchored `WinGC` executable 61/subtype 3,
  state 1 at external PC 53, processed exactly 113 credits-child spawns, reclaimed returned
  children, reused arena slots through generation three, and requested Title `0x19` on frame 3,396
  with zero faulted objects. Live population peaked at 82, below the regression bound of 89;
  before the fix, returned children remained at PC 54 and filled all 97 slots at frame 1,437. This
  verifies no-TERM reclamation, a clean `LEVEL_END`, and a session carry imported into a fresh
  Title runtime without losing draw phase; it does not exercise the subsequent browser graph
  remount in the same run.
- The legally local N. Sanity → Level Complete → Title vertical-flow golden now asserts the native
  process-lifetime `draw_count` at both exported carries, both imported runtimes, and Title's first
  display frame. The observed sequence retained 1,900 into Level Complete, 2,413 into Title, and
  advanced to 2,414 on Title's first display frame. Unit coverage separately proves a stream
  remount retains a nonzero counter through
  the two-frame loading skip (including `u32` wrap) while `LevelRestart` continues to reset it.
- Seven source-golden `CamDeath` tests cover PC sqrt/atan quantization, signed pitch selection,
  signed negative zoom deltas, nine-frame alignment, cooperative-tick rotation, spin acceleration
  and checked overflow; three
  resolver tests cover live/stale tagged objects, animation/frame/vertex validation and exact
  world-space focus. A renderer golden consumes an explicit non-path Y/X/Z camera pose. Against the
  legally local Cortex Power pair, the active-input 1,300-frame survey observed 117 consecutive
  spin-death frames, 116 pose changes and a maximum count of nine, with exact first and last poses;
  the 1,800-frame Papu Papu survey exercised six ordinary authored restarts and correctly observed
  no spin-death frames.
- Browser-card persistence merge tests prove that writing one physical slot refreshes only that
  slot's `updatedAt`, identical-byte writes still refresh the selected slot, passive snapshots retain
  unchanged envelope/slot timestamps, changed passive snapshots update only changed slots, and
  format clears all slots while refreshing the envelope.
- A legally local authored-title golden reaches Main Menu ready at frame 10, routes Cross to Map,
  one Down pulse plus Cross to Load (mounted at frame 22), two Down pulses through the shared `0e`
  Password controller, and three Down pulses to Options. The empty-card Load route now proves real
  CardC observes `CHECKING`, issues operation-two `ClearFlag6`, and leaves a published zero-part,
  zero-flag card instead of deadlocking at `PENDING | FLAG_6`; 26 focused card tests cover the exact
  acknowledgement and following-update publication sequence.
- Header-length-bounded type-three font tests now cover retail's variable record count instead of
  assuming the C declaration's first 63 slots. The legally local corpus paired 62 text/font
  resources, projected all 1,257 terms into 4,372 quads, and decoded all 32 CardC controller-icon
  terms; malformed and unserialized glyph references remain checked failures.
- The Options first-frame regression executes the exact null-linked-register instruction observed
  in all four legal OptionsC roots and preserves strict failures for neighboring missing-link loads
  and stores. The legally local Main Menu → Options flow mounted state six and then ran all roots
  for 128 additional browser-ordered frames without a VM warning.

The browser verification build was loaded in a fresh visible in-app-browser tab. An ephemeral
same-origin, loopback-only route wrapped the supplied 632,083,536-byte raw BIN in a browser `File`
and dispatched the ordinary local-input event because the browser harness cannot automate a native
file picker.
The importer recognized 88 streams, all 44 pairs, and 219 MiB of selected extents. Title `0x19`
ran its authored publisher and Main Menu sequence at 30.00 Hz with synthesized audio. At a narrow
responsive viewport, held touch-pad taps opened the authored Options screen; it remained
`RUNNING` with no VM warning or browser-console error. A fresh desktop direct `0x09` mount rendered
N. Sanity Beach's world, Crash, and crate geometry. Native pause/resume and mute/unmute changed and
restored live telemetry to `RUNNING` and `SYNTH ACTIVE`. After the tab retained its in-memory local
`File`, the bridge was stopped and deleted and the ordinary no-store server was restored. Root
returns HTTP 200, both temporary routes return HTTP 404, and the served Wasm hash matches the
release hash above. The visible tab remains on live N. Sanity gameplay. Keyboard, physical gamepad,
and fullscreen were not repeated in this pass; no game bytes entered Git, browser persistence, or
the repository working tree. A final independent audit then found that the generic seek helper had
normalized a negative authored death-camera zoom delta. The signed source-compatible correction and
its regression passed the complete native, legally local, Clippy and release gates above; the final
Wasm was regenerated and is what the normal server serves. Because the local-only bridge was already
deleted, the visible in-memory tab was not remounted after that narrow correction; its observed
positive-speed N. Sanity route is unaffected by the signed edge case.

Additional checks for the current source-ordered paging/display change set were completed against
legally local data:

- GOOL `0x8b` open/close/probe now crosses a typed synchronous host boundary. Focused tests cover
  unavailable-open rollback, resident eviction reconciliation, invalid eviction/page responses,
  case-five live-resolution reads, externally seeded page state, native-idempotent close behavior,
  and two EIDs sharing one page. Pager tests cover all eight usable slots (native physical slots
  8–15), source-order free/stale/unprotected replacement, destination protection, and atomic
  exhaustion. Initial mount, hard restart, and normal transition mirror every actual Pager delta
  back into the VM.
- The legally local 360-frame N. Sanity idle golden recorded exactly 24 paging requests from object
  six: 12 opens and 12 closes, no probes, at frames 2, 3, 30, 46, 83, 120, 145, and 194. Every EID
  finished with a zero open/close delta and the run remained clean. Its asset-only `NsfProgramHost`
  acknowledgement does not substitute for an end-to-end browser Pager test.
- Runtime traversal publishes owned display records immediately after each object update and before
  its children. Focused tests verify that later teardown or reparenting cannot retract the record
  and that a failed frame does not replace the last successful publication. Renderer tests cover
  frame-start world/filter membership plus per-object live Pager replay, including same-slot
  `A → B → A`: cached A regions survive, uncached regions follow the current mapping, and returning
  A reuses its frozen generation.
- Normal and hard-restart transitions were audited for old-protection TERM handling, idempotent
  closes, VM/Pager reference reconciliation, and no-core title/special-stream seeding. Normal
  transitions install destination protection before close/open work; hard restart retains old
  protection through RESPAWN, TERM, and closes, then switches before its first restored open.
  Candidate state is preflighted before publication; this is not a whole-transition rollback
  because authored TERM mutations are intentionally irreversible.
- The opt-in fog-start sweep remains green for the two retail Fog starts and 544 projected WGEO
  vertices whose output RGB differs from authored source color. The later dynamic-world closeout
  below covers Lightning, combined Dark and Dark2 separately.

### Dynamic-world and shared-RNG closeout (2026-07-15)

- A read-only source audit fixed the exact pre-camera order, mode priority, all 84 fixed-sequence
  words, six lightning patterns, random reductions, ruins/boss cases, two-stamp `6145` thunder
  cooldown, Dark2 doctor/Crash selection, ambient/distance ramps, and renderer `far_color1` BSS
  writes. Bandicoot remained unmodified.
- The legally local 44-pair reachable-zone census found 362 Lightning, 115 combined Dark and 80
  Dark2 zones. All 557 are reachable from their validated retail graph roots; the aggregate FNV-1a
  fingerprint is `0xa771e6a007ead119` (mode fingerprints `0x08e7ab506b45d34d`,
  `0x79a4940a24031991`, and `0xb3d6ec0ec99bd149`).
- The local spawn-scene integration exercised 9 Lightning, 3 combined-Dark and 2 Dark2 starts. It
  projected 15,168 / 4,218 / 3,951 WGEO vertices and observed 5,208 / 1,447 / 423 colors differ
  from the authored unshaded values. Fixed-point evaluator tests separately cover channel
  selection, fog order/cutoffs and Dark2 target/illumination math.
- Focused simulation tests cover the full fixed table and wrap, seed-zero random sequences, Brio and
  Storm pattern/cooldown/sentinel behavior, doctor-over-Crash illumination, Dark2 ramps, partial
  reinitialization and cross-mount BSS/RNG retention. A read-only lifecycle audit established that
  native keeps the doctor's static-pool pointer and initialized transform after kill. Regression
  coverage now writes the doctor global, frees multiple objects, and reuses the lowest VM handle in
  a different physical slot before the shader's first read; the write-time pool-slot capture still
  preserves the doctor's value. Reuse of the doctor's LIFO pool slot retargets it, and a later write
  of the same tagged word binds the new object. Renderer
  tests cover empty/world-hidden SLST scratch writes and Dark2 retention. Audio tests cover
  ownerless delayed-key thunder creation and template reset.
- The strict direction/button survey ran every one of the 43 selectable pairs for 360 browser-
  ordered simulation frames with clean-runtime enforcement. Lights Out separately retained its
  non-null executable-29 doctor global across the authored same-level restart. The runtime-created
  `DoctC` remains live with native null-zone ownership instead of entering neighbor-zone teardown,
  and completed 360 active frames with no checked issue. The five direct bonus boots (`0x24`,
  `0x25`, `0x26`, `0x33`, `0x34`) each
  captured a same-level restart snapshot; the three routes that died within the window restarted
  cleanly. Parent-carried bonus return remains separately protected, while synthetic direct-boot
  completion is documented as an unresolved host-policy gap.
- The N. Sanity browser-order render contract resolved `WillC → WiI1V → WillG`, retained 381 model
  vertices and 732 authored TGEO triangles, submitted 322 Crash triangles on boot frame two, and
  matched command fingerprint `0xc19351b9ca5b0c36`. This proves authored command construction and
  material mapping, not pixel equivalence or a human-controlled browser playthrough.
- Native RNG-B is now one source-ordered stream across shader updates, thunder/GOOL voice
  allocation, `LEVEL_END`, PBAK choice and destination import. The legally local PBAK sweep counts
  type-19 entries from NSD metadata, covers all nine recordings and 10,966 frame boundaries, and
  ends its nine one-entry choices at seed `0xaf5aad71`. Synthetic tests cover count-one's first two
  seeds (`0x00003039`, `0xd3dc167e`), a count-nine choice, source EID construction and a checked
  out-of-alphabet level instead of native out-of-bounds behavior.
- Recorded PBAK absolute time now follows the newly current frame after Crash's pad boundary;
  initial, completion and physical-interruption frames retain their source state gates. The
  pause-adjusted shader clock includes asynchronous mount validation time and wraps as a 32-bit
  word. Disposable mount previews no longer advance ripple or renderer scratch, while actual
  hidden draw-skip frames transform the real camera/visibility state before presentation.

The supplied 632,083,536-byte raw BIN was mounted in a fresh visible in-app-browser tab through an
ephemeral loopback-only helper that dispatched the ordinary local-input event. The importer
reported 88 streams, all 44 level pairs, and 219 MiB of selected stream extents. Title `0x19`
started at its authored Universal Interactive Studios publisher screen and remained `RUNNING` at
30.00 Hz with `SYNTH ACTIVE` and card `0/15`; the clean tab recorded no console entry. The one-shot
helper closed immediately after its read, and rebuilding the distribution removed its bootstrap
and CSP changes while the retained tab kept its local in-memory `File` and continued running. The
served repository contains no helper route or game byte. A live mid-frame eviction/transition,
keyboard, physical gamepad, touch, fullscreen, or persistence reload was not exercised in this
pass. No game data entered Git, browser persistence, or the repository tree.

The legally local N. Sanity interaction regression additionally follows the retail-authored first
Box7, CrabC and Box12 contacts, then records nine ordinary box-count transitions at frames 207,
334, 512, 644, 651, 683, 685, 762, and 787 before the checkpoint at frame 861.
Its fresh VM checkpoint global begins at native `LevelResetGlobals(1)` sentinel `-1`, rather than
relying on a host-side context to mask a zero-initialized process word.
Checkpoint entity 19 saves `0x900` synchronously before its handler's later live increment to
`0xa00`, with checkpoint ID `0x1300` and translation
`[1_945_600, 4_135_168, 24_165_632]`. Suppressing the route controller's jump in `a8_9Z` permits
the authored TurtC collision: the death camera begins at frame 1,035, changes pose on 116 of its 117
frames with native maximum count nine, and restart completes at frame 1,150. The frame-1,151 trace
sample observes `LevelInitMisc(0)`'s reset to zero; checkpoint respawn recounts to `0x100` at frame
1,152. The run emits one save and one load handshake, retains the checkpoint identity, and reports
no VM error, fault,
terminal fall, or level transition.

A paired reference-C oracle with an exact cooperative 34-tick clock confirms the early sequence.
Native C FE203/204/205 corresponds to Rust survey frames 205/206/207: `PlotObjWalls` links Crash and
Box7, Crash sends `0x400`, and Box7 publishes its counted spawn state. C FE308–310 corresponds to
Rust frames 310–312: `PlotObjWalls` links CrabC while it is still in state one, Crash sends `0x400`,
and CrabC enters its defeat state before the next traversal. The direct-send tail is exactly
`(311, 0x400), (312, 0x1000), (312, 0x400)` and contains no `0x300`. C then matches the CrabC
`0x1d00` send to Box12 and Box12's frame-334 count. Earlier rAF-driven C runs shifted
`frames_elapsed` relative to the camera and were not deterministic goldens. No event suppression or
entity special case was added.

The rebuilt browser artifact was then mounted from the user's exact 632,083,536-byte raw BIN through
a localhost-only one-shot response. The browser recognized 88 streams and all 44 pairs, displayed
the retail Naughty Dog title composition, and separately direct-booted N. Sanity Beach. The retained
tab reported `RUNNING`, 30.00 Hz, `SYNTH ACTIVE`, level `0x09`, 88 files, 44/44 pairs, and no captured
console error while rendering the retail world, Crash, and crates. Both one-shot responses closed
after their single read; a final distribution rebuild removed the temporary bootstrap/CSP changes,
and no helper listener or proprietary byte remained in the repository or served artifact. Keyboard,
physical gamepad, touch, fullscreen, persistence reload, and the newly characterized checkpoint
death route were not repeated manually in this browser pass.

For this change set, 837 locked default tests and the complete 71-test legally local opt-in sweep
passed with zero failures. The separate clean-policy active-input survey covered all 43 selectable
pairs for 360 frames. Formatting, warnings-denied native and Wasm Clippy, the optimized native
workspace build, optimized Wasm build, and web distribution build also passed. The generated Wasm
is 1,322,866 bytes with SHA-256
`68314577df1e2afdf6bdb0c30b3db529b8fd2ced0abfd71583289e783c08c9e8`; the generated loader is
46,236 bytes with SHA-256
`a26ab5a1a47530af0d7ca7326da07e3dcdfb30c8dcd9dafd56523d93813f5abe`.

### Live-collider, raw-hotspot, and pool-pointer closeout (2026-07-15)

- A read-only source audit established that the current collider is a live link, not an index into
  the bounded frame-candidate array. Native solid phases continue reading its translation,
  status-B, state flags, object type, and hotspot size after a candidate rejection, and synchronous
  handlers may replace that link before later phases. Focused Rust tests cover a current collider
  omitted from the candidate snapshot, its retained `BOX_OBJECT` floor offset and priority
  metadata, a handler replacement, and the source-priority self-alias case.
- Native hotspot math forms inset `p1`/`p2` endpoints and compares their faces directly; it does not
  require or normalize `p1 <= p2`. The exact Rolling Stones candidate that exposed the difference
  has hotspot `0x5000` and produces reversed Z endpoints `30,361,440` and `30,361,184`. Its focused
  active-input run completed 1,800 browser-ordered simulation frames with zero VM error, faulted
  object, or checked runtime issue.
- The Jaws of Darkness failure was traced to `FruiC` state 12: global six's `fruit_hud` object
  pointer is copied into the creator link, then opcode word `0x11d08e17` reads that creator's
  `translation.x`. Native static-pool storage retains the reclaimed process word
  `0xffff3800` (`-51,200`). The checked representation now carries physical pool provenance through
  globals, pre-existing process links, registers/stack, and internal/external MOV storage. Focused
  tests exercise live and free-slot register reads/writes, nested pointer provenance, physical
  pointer equality, compact-handle reuse in another slot, and retargeting when the original slot is
  reused. Raw internal/external replacement clears stale provenance, and object-valued audio and
  miscellaneous opcode consumers follow captured physical slots across compact-handle reuse. The
  focused Jaws active-input run then completed 1,800 clean frames.
- Allocator tests cover the ascending 96-slot native free chain, arbitrary-slot unlink, LIFO
  reclaim, ordinary freed-object parent/sibling/child mutations, dedicated-main exclusion, and
  transactional binding failures. Reuse tests seed the retained process array before selective
  initialization and prove raw `sp`, `pc`, `fp`, `tp`, and `ep` are reset while fields native does
  not initialize retain their slot values.
- The ignored legal regression
  `brio_boxsc_creator_link_survives_brioc_pool_reclaim` runs Dr. N. Brio through the same bounded
  browser-order input path. At frame 405 it observes eight live `BoxsC` objects linked to `BriOC`;
  frame 406 reclaims that creator while all eight boxes remain live. The test verifies every link
  four process word retains the original non-null pointer and reports no faulted object. Its focused
  invocation passed one test with zero failures.
- At this checkpoint, address-taking through a free-slot link and event/spawn sidecars were not yet
  covered; both were closed by later checkpoints. The current model still does not retain an
  ordinary replacement's prior local-bound bytes or expose the dedicated main allocation's extra
  0x100-byte stack tail. It deliberately rejects writes to an ordinary free slot's allocator-owned
  parent/sibling/children words instead of attempting to reproduce native malformed-list behavior.
- `cargo test -p crust-sim --lib --locked` passed all 537 simulation-library tests. The strict
  active-input survey ran all 43 bootable pairs for 1,800 frames each (77,400 frames total) with
  clean-policy enforcement and no checked runtime issue. This expands the earlier 360-frame survey;
  it remains a bounded direct-boot simulation sweep rather than full-route or browser-playthrough
  evidence.
- All 861 locked default workspace tests and the complete 72-test legally local opt-in sweep passed
  with zero failures. Locked native workspace tests, warnings-denied native Clippy, the optimized
  native workspace release, warnings-denied `wasm32-unknown-unknown` `crust-web` Clippy, and the
  optimized Wasm build all passed. The final distribution artifact is a 1,337,759-byte Wasm module
  with SHA-256
  `b078aca54e9484b161e2e7ce1c9b0e2d89e7a5c8e8bfcd312382b9ed81a734c0`; its 46,236-byte generated
  loader has SHA-256 `986d18af7d9e4108a2f28fced2206df17020ebde743171a1d3636afb61943c79`.
- The rebuilt distribution was served from `127.0.0.1:4174` and exercised in a fresh foreground
  Google Chrome pass. The native macOS picker selected the user's legally owned raw NTSC-U BIN in
  place. The browser recognized 88 streams, 44/44 pairs, and 219 MiB of extracted in-tab data, then
  reached the retail publisher screen, main menu, and N. Sanity Beach island-map node through the
  title target. A separate direct `0x09` boot visibly rendered N. Sanity Beach with Crash and crates;
  telemetry reported `RUNNING`, 30.00 Hz, `SYNTH ACTIVE`, and level `0x09`.
- Six `Up` taps followed by `Z` visibly changed the final-build gameplay frame. Chrome DevTools
  reported zero console messages after local import, direct boot, audio start, and keyboard input.
  In the earlier browser pass, eight `Up` taps followed by `Z` captured Crash in a jump.
  The page controls changed simulation telemetry to `PAUSED` and back to `RUNNING`, audio telemetry
  to `MUTED` and back to `SYNTH ACTIVE`, and successfully presented the live canvas fullscreen.
  No proprietary byte was written to the repository; retail-data reads stayed in the local browser
  tab or legally local test paths.
- These browser passes did not perform a complete level route or manually repeat gamepad, touch,
  virtual-card persistence, later transitions, bosses, bonuses, or ending flows. It is browser
  evidence for import, title presentation, direct gameplay, keyboard input,
  audio state, pause, and fullscreen—not a full-playthrough or retail-parity claim.

### Event-argv, effect-transaction, and bonus-context checkpoint (2026-07-15)

- Physical retail-pool provenance now accompanies event argv through direct and broadcast sends,
  EARG scopes, mapped-state changes, and child-spawn creation. Validation is transactional: invalid
  sidecars, unknown objects, state mismatches, and conflicting paging metadata leave the complete
  machine unchanged. Focused tests also cover reused physical storage being seeded before child
  argv is applied and public-event inference from a live pool-backed pointer.
- A retail frame drains ordered VM effects after each visited object, and every synchronous event
  recipient starts a fresh bounded effect transaction. Deep 96-object preorder and 95-recipient
  broadcast tests retain exact effect chronology beyond the standalone 256-effect transaction
  bound. This removes the false whole-frame `EffectQueueFull` reached by the authored Dr. N. Brio
  input survey without weakening the local transaction limit.
- Level state publication now writes the current zone graphics flags to GOOL global 30 before the
  next spawn/update pass. All five legally local bonus spawn zones publish `0x2002`, and the exact
  Tawna-bonus WillC WARP program tests bit `0x2000` before its LoadState branch. The test establishes
  the legal corpus and authored branch layout; it does not claim a complete third-token, portal,
  `-2`, and protected parent-remount playthrough.
- All 545 simulation-library tests passed. The locked workspace gate passed 869 default tests with
  zero failures, and the complete legally local opt-in suite passed all 73 tests against the user's
  raw NTSC-U BIN and extracted streams. The final strict active-input survey ran all 43 bootable
  pairs for 5,400 frames each—232,200 pair-frames—with `faulted=0`, `errors=0`, and no checked
  runtime issue. This remains bounded direction-and-button direct-boot evidence, not route
  completion evidence.
- Rustfmt, warnings-denied native workspace Clippy, warnings-denied Wasm Clippy, the optimized
  native workspace build, and the optimized `wasm32-unknown-unknown` build all passed. The rebuilt
  distribution contains a 1,343,521-byte Wasm module with SHA-256
  `335875ede6f7651ca59b48ced29899a56d93c75ed65da5eef0658dbaa6f47563`; its 46,236-byte generated
  loader has SHA-256 `0bd8e59cb89ffe118cff88dc13889ebdae612941808c693a782a6475983770cc`.
  The no-store server returned byte-identical hashes for both files.
- The exact rebuilt distribution was exercised in foreground Google Chrome with the user's raw BIN
  selected through the native macOS picker. It recognized 88 streams, all 44 pairs, 43 playable
  targets, and 219 MiB retained only in the tab; publisher presentation and the N. Sanity Beach
  island node rendered from the title target. A fresh direct `0x09` mount visibly rendered Crash,
  crates, and the level scene at 30.00 Hz with `SYNTH ACTIVE`. Six Up taps followed by Z captured
  Crash airborne. Application pause/resume and mute/unmute were repeated successfully, and the
  runtime engineering log showed the local mount, image decode, scene, camera, paging, and GOOL
  initialization sequence without a reported runtime fault. The rebuilt in-app landing page also
  reported no warning or error console entries.
- No proprietary byte was copied into the repository or generated distribution. This pass did not
  manually repeat physical gamepad, touch/mobile, fullscreen, virtual-card persistence reload,
  later transitions, bosses, a complete bonus return, or ending flows, and it is not a full retail
  parity claim.

### Tawna bonus return checkpoint (2026-07-15)

- Source and legal-corpus tracing corrected the authentic parent route to Jungle Rollers (`0x0c`)
  → Tawna Bonus 1 (`0x24`). WarpC's authored `0x1600` event carries one literal-zero argument.
  WillC sets global one to nine and waits at the CardC confirmation prompt; a tapped Cross after
  CardC's readiness gate selects state 63, clears the global, and releases the WARP completion.
- The legally local vertical regression now mounts the parent and bonus streams with real parsed
  GOOL/ZDAT data, retains the complete parent snapshot through the save-restricted `0x2002` bonus
  zone, taps Cross on deterministic frame 300, and observes WillC's `LoadState` on frame 301. It
  resolves sentinel `-2` to `0x0c`, imports the returned carry into the parent stream, positions the
  camera at the saved path/progress, suppresses the protected initial Crash auto-save, and performs
  the same-level restart. Assertions cover Crash translation/rotation/scale, camera location, box
  count, and all 304 saved spawn words after native active-bit clearing.
- Different-level misc `12/1` now matches native control flow: it retains the ordered `LoadState`
  effect for the browser but continues the current GOOL interpreter, later preorder objects, and
  display latch before the next-frame remount. Bonus global 60 clears synchronously before that
  tail, and the effect carries the saved level captured at its own boundary. Default regressions
  cover both an ordinary frame and a LEVEL_END handler issuing a later same-level SaveState: the
  earlier load remains different-level while the eventual `-2` target follows the newer snapshot.
  Same-level loads still stop at the checked boundary because that deferred transaction
  structurally replaces the live object forest.
- The stable checkpoint passed Rustfmt, 870 locked default tests with zero failures and 74 ignored,
  all 74 legally local opt-in tests with zero failures, warnings-denied native and Wasm Clippy, and
  optimized native and `wasm32-unknown-unknown` builds. `npm run build` regenerated the served
  distribution. Its 1,345,221-byte Wasm module has SHA-256
  `74d69d81473ab74a6e87956304124185cb0dcf047fe74b2990f6244942f2c3bd`; the 46,236-byte generated
  loader has SHA-256 `5d16cd2e5b74c8178a95006de3063a3ea3ce0aa339d6697006afcf4c3ca87ffc`.
- The rebuilt no-store server returned HTTP 200 on `127.0.0.1:4174`, and a fresh visible in-app
  browser load reached the local-media workspace with the Wasm boot controls, 30 Hz standby
  telemetry, 15-slot card status, and no console entries. This checkpoint-specific browser smoke
  did not remount the BIN because the in-app browser does not expose automated file upload; the
  earlier foreground Chrome raw-BIN/direct-gameplay pass above remains the current browser import
  evidence.
- This is control-path and protected-remount evidence, not a claim that automated input collected
  the three Tawna tokens or physically traversed the bonus portal. Those uninterrupted gameplay
  steps remain open.

### Tawna token-entry and portal-gate checkpoint (2026-07-15)

- The legal Jungle Rollers stream contains the three characterized Tawna token crates at
  `0h_cZ:22`, `0w_cZ:59`, and `0G_cZ:79`. Each descriptor is group 3, executable `0x22`, subtype
  10, with initializer `0x69`. Starting at the authored player `HIT 0x0300` boundary enters
  `BoxsC` state 24, spawns subtype-13 `FruiC`, sends `[0x6900, pid]` to the live `DispC` pickup HUD,
  and sends token kind `0x6900` to Crash. The observed counter sequence is
  `0 → 0x100 → 0x200 → 0x300`.
- Only the third token makes the HUD emit `SaveState`, on local frame 4 and before Crash increments
  the counter. After that increment, `DispC` sends completion `0x2700 [0]` on frame 1, resets the
  master-fade step on frame 38, sends status `0x0f00 [0x500]` and emits `Transition(0x24)` on frame
  53, writes checkpoint `79 << 8`, and clears the token counter. Completing `LEVEL_END` resolves
  Tawna Bonus `0x24` and carries the saved Jungle Rollers `0x0c` snapshot into its fresh runtime.
- The legal Tawna Bonus portal descriptor is `1__AZ` entity 15, group 3, executable `0x20`, subtype
  1, spawn flags `0x8`, zero initializer, at point `(1479, 310, 160)`. Its parsed WarpC transition
  requires portal bit `0x20` clear, signed quantized X/Z Euclidean distance `< 0x28000`, Y delta in
  `[-0x20800, 0)`, grounded Crash, and no atop-object bit `0x200000`. Boundary cases include the
  positive signed-shift edge at `0x27f00`/`0x27f01` and the negative edge at `-0x27fff`. An accepted
  gate sends direct event `0x1600 [0]` and selects WillC state 32; every rejected case completes
  without sending an event.
- The downstream legal cross-stream test now also asserts that exact portal entity before covering
  CardC, frame-301 `LoadState`, protected `-2` return, and the complete parent remount. These tests
  deliberately join at controlled program boundaries: they do not steer Crash to the crates or
  portal through collision broadphase, and they are not one uninterrupted browser playthrough.
- The stable checkpoint passed Rustfmt, 870 locked default tests with zero failures and 76 ignored,
  all 76 legally local opt-in tests with zero failures, warnings-denied native and Wasm Clippy, and
  optimized native and `wasm32-unknown-unknown` builds. `npm run build` regenerated the served
  distribution. Its 1,345,221-byte Wasm module has SHA-256
  `74d69d81473ab74a6e87956304124185cb0dcf047fe74b2990f6244942f2c3bd`; the 46,236-byte generated
  loader has SHA-256 `5d16cd2e5b74c8178a95006de3063a3ea3ce0aa339d6697006afcf4c3ca87ffc`.
- The no-store server returned HTTP 200 and byte-identical Wasm/loader hashes. A visible in-app
  browser reload reached the local-media workspace with the picker, 30 Hz standby telemetry,
  15-slot card status, touch controls, and no console entries. This smoke did not remount the raw
  BIN because the browser automation surface cannot populate the native file picker; the earlier
  foreground Chrome raw-BIN/direct-gameplay pass remains the current browser import evidence.

## 2026-07-15 retail browser and stale-build verification

- A legally local NTSC-U raw BIN was supplied to a fresh Wasm page through an ephemeral
  same-origin test helper outside the repository. The helper constructed the same browser `File`
  and dispatched the same production file-input event as the native picker; it differed only by
  reading the whole local BIN into browser memory first. From `import_files` onward the ordinary
  Blob/disc discovery, pair validation, runtime, renderer, audio and input path ran unchanged.
- The page recognized 88 S0–S3 streams and all 44 pairs, selected and rendered the image-backed
  retail title/menu, then directly mounted N. Sanity Beach (`0x09`). The visible gameplay frame
  contained Crash, the authored beach/world geometry and crates. Runtime telemetry remained at
  30.00 Hz with synthesized audio active.
- The N. Sanity engineering log reported 22 reachable zones, 77 retail entity descriptors,
  679/679 first-presented polygons, 49 decoded textures, 80 pages, 231 entries, seven successful
  initial entity bindings with zero unexpected failures, and frame-zero execution of 23 objects
  with zero GOOL errors. Recent browser diagnostics were empty after the final reload.
- The browser exposed the same content-derived build ID in the DOM and build manifest. Generated
  JavaScript and Wasm were requested with that ID. Eight dependency-free Node tests covered valid
  manifests, changed source, tampered generated JavaScript, content-stable fingerprints, a missing
  manifest, generated-JavaScript identity changes, forged identities and Git-state drift. The
  server also served the verified manifest and Wasm with `no-store` and the correct Wasm MIME type.
  A live source-drift probe changed `web/bootstrap.js`, received the intended `503` instead of the
  stale artifact, restored the byte-identical source and immediately received `200`; the temporary
  probe was not retained. A held build lock also rejected a concurrent publisher before it touched
  the valid distribution. Two consecutive successful `npm run build` invocations then published
  and verified the same content-derived identity without leaving the lock behind.
- Formatting, Clippy with warnings denied, the complete non-ignored workspace test suite, native
  release, the locked release Wasm build and `npm run build` passed. This pass verifies a genuine
  retail title/direct-level browser boot and eliminates stale diagnostic bundles; it is not a new
  claim of a complete browser playthrough or full retail parity.

### CoreObjects, island-route, audio-init, and quad-order checkpoint (2026-07-15)

- Initial and destination mounts now execute the source `CoreObjectsCreate` pad-history boundary.
  Three ordinary-input tests cover a new mount press, a button held across the mount, and the
  browser's between-frame latch. A fourth covers `PbakChoose` state three: physical/pending input is
  suppressed to zero while prior held/tapped history still shifts.
- Title island camera modes seven/eight now receive live globals 66/64. Mode seven writes global 66
  before `LevelUpdate`; mode eight writes it after the synchronous `LevelUpdate`/TERM boundary.
  Three native tests exhaust every `u16` mode, signed state passthrough/non-island rejection, and
  the state observed by a synchronous TERM callback. A legally local normal-route regression
  observed mode-seven state `-1 → 1` before its frame-23 cross-zone update, reached Main Menu ready
  at frame 10, loaded Map at frame 20, reached Map ready at frame 30, emitted authored N. Sanity
  destination `0x09` on Cross at frame 31, completed `LEVEL_END` with no handler failure, and
  imported the session carry at draw count 31.
- `MidiInit` voice partitioning is table-tested for every one of the 44 known pairs, both volume
  override branches, mute precedence, and the default branch. Browser initial boot uses
  resume-restored options; destination mounts use carried GOOL globals. Each pair remount also
  resets the all-bus master fade to `0x3fff`, step zero, and unity WebAudio gain.
- Colored and textured quad conversion now matches the C backend's `[0,1,3]`, `[3,2,0]` order.
  Legally local sprite/fragment, text/font, pause-panel, Great Hall dynamic-font and all-pair
  non-vertex scene regressions passed. The N. Sanity authored Crash command fingerprint remained
  `0xc19351b9ca5b0c36`.
- Focused verification passed 69 web-library tests (53 active, 16 local-data ignored), all 71 audio
  tests, all 87 renderer-library tests, warnings-denied Wasm Clippy, all 881 tests in the locked
  non-ignored workspace gate, and all 77 legally local ignored tests against the user's BIN/stream
  directory. The complete workspace inventory is 958 tests.

### Final browser candidate (2026-07-15)

- A fresh in-app-browser load mounted all 88 legally local S0–S3 files through the real file-input
  change handler, recognized 44/44 pairs and retained 219 MiB only in the tab. Because the in-app
  browser automation API cannot operate a native file chooser, this pass used a temporary
  loopback-only test bridge outside the repository to populate that input; the bridge was bound to
  `127.0.0.1`, copied no assets into `crust`, and is not part of the application or distribution.
  Raw-BIN discovery/extraction was exercised separately by the complete legally local suite.
- Boot target `0x19` rendered the authored main menu. Touch Cross entered the island map and showed
  the N. Sanity Beach node; a second Cross mounted `0x09` and visibly rendered the authored opening
  gameplay scene. Telemetry remained at 30.00 Hz with synthesized audio active. Pause/resume and
  mute/unmute toggled to their expected states, keyboard direction events were dispatched, and the
  browser diagnostic log remained empty.
- This was a bounded route/control smoke test, not a full playthrough. Gamepad, fullscreen, virtual
  card persistence, later-level transitions, bosses, bonuses, and ending progression were not
  repeated manually in this pass. The final post-commit distribution is rebuilt with
  `npm run build`, checked by the stale-bundle guard, and served from the ordinary repository
  server; the delivery summary reports its exact clean build identity.

### Physical storage, one-point path, and first-completion route checkpoint (2026-07-15)

- Linked GOOL address-taking now uses a disjoint aligned `0xa100_0000` storage tag carrying the
  physical arena slot and complete 508-word register index. Decode rejects nonzero low bits,
  reserved payload bits, slot 97 or greater, and register 508 or greater. References retain their
  storage through reclamation, retarget only when the same physical slot is reused, and do not
  follow unrelated compact-handle reuse. Scalar writes preserve the allocator-link rejection
  error; three-word translation writes preflight the complete span and cannot partially mutate
  retained state.
- The separately allocated player/main object has retained backing at physical slot 96
  from machine construction. Every live object's link five remains non-null through a missing-main
  interval, main teardown, and later main reuse. Its first pre-main translation write initializes
  both the retained register words and the cached translation view. Focused tests cover live,
  reclaimed and same-slot-reused LEA sources and destinations; malformed tags; protected allocator
  links; cross-decoder tag rejection; exact vector transactionality; and a logical handle 96 bound
  to an unrelated ordinary slot.
- Transform selector `0x85/0` returns point zero for a declared one-point path before performing any
  index arithmetic, including for fractional and extreme signed progress. The legally local Title
  pair contains exactly one matching `IsldC` entity: zone `1a_pZ`, entity zero, id one, executable
  59, item byte range `2889040..2889068`, and point `[99, 200, 200]`. At progress `0x110`, the C
  ordering indexes six bytes beginning two bytes before the following item and reaches its relocated
  parent pointer, so the original result is address-dependent undefined behavior rather than a
  stable value to reproduce. The Rust stationary result is the explicit safe contract.
- The legally local continuous route now starts from a fresh authored Map initialized through the
  card-payload restore path instead of seeded scalar globals. Map requests N. Sanity Beach on frame
  11; N. Sanity requests Level Complete on frame 1,900 at draw count 1,911; Level Complete requests
  Title on frame 513 at draw count 2,424; and the post-completion Map settles, takes Up then Cross,
  and requests Jungle Rollers on frame 253 at draw count 2,677. Those frames and draw counts are
  explicit regression assertions. The final camera is `1b_pZ` path zero at progress `0x0b00`; map
  level two, level count one, two unlocked levels, and island state one survive the session carries.
  Every checked execution and `LEVEL_END` handler succeeds, with zero faulted objects.
- The authentic Jungle carry initially exposed a stale fresh-controller assumption: holding its old
  route entered gameplay-death state 23 at frame 532 and restarted at frame 648. Replacing either
  RNG-A or draw count independently avoided that collision, while replacing globals, spawn tags,
  saved state, RNG-B, respawn/death counters, or `first_spawn` did not. Both values are intentionally
  process-lifetime in the source, so no incompatible mount reset was added. The committed
  phase-robust route instead uses authored attacks at the live hazard phases and reaches checkpoint
  entity 46 at frame 1,117. Its exact-carry golden retains RNG-A/draw continuity, saves the `0x400`
  pre-increment box count, advances the live count to `0x500`, and continues through the remaining
  main-path zones. It reaches `0O_cZ` path zero/progress 17,836, enters the end `WarpC`, and emits
  `Transition(0x2d)` at frame 2,546 with live count `0x1000`, RNG-A `0x742c4322`, and draw count
  5,223. There is no restart, below-zero or terminal fall, VM error, faulted object, or checked issue.
  Jungle `LEVEL_END` resolves Level Complete with current map level two, level count one, and three
  unlocked levels. The second Level Complete screen emits Title on frame 306 at RNG `0xa442cb3a`
  and draw count 5,529. After Title remount, the same 120-idle/Up/120-idle/Cross schedule selects The
  Great Gate `0x12` on Map frame 253 at `1c_pZ` path zero/progress `0x0200`. Its checked Map
  `LEVEL_END` carry has current map level three, level count one, three unlocked levels, island state
  one, RNG `0x4a04f4bf`, and draw count 5,782. The Great Gate imports it and executes an exact carried
  retail-pad main route with 111 successful spawns, 47,371 clean executions, 38 lifecycle zone
  transitions, 14 counted boxes (`0xe00`), and no restart, death camera, terminal fall, VM error,
  faulted object, or checked issue. It traverses `a1_iZ`-`a9_iZ`, crosses the wide pit, observes
  `WalOC` state two, waits through the later rising-log phase, and chains the first three arrow-crate
  bounces. Checkpoint crate 76 emits one `SaveState` at frame 1,152 with pre-increment box count
  `0x900` and checkpoint translation `[20991488, -8397312, 127744]`; the live count advances to
  `0xa00`. It then traverses `b3_iZ`-`c7_iZ`, clears the snake and later hazards, and enters the
  normal end `WarpC`. That warp emits `Transition(0x2d)` at frame 2,471 with Crash at
  `[3593984, -4780682, 83712]`, four unlocked levels, RNG `0x6a219f2c`, and draw count 8,396. Its
  Level Complete screen emits Title at frame 225 with RNG `0x2875d290` and draw count 8,621. The next
  Map takes the same 120-idle/Up/120-idle/Cross schedule to Boulders `0x0e` at frame 253 on `1c_pZ`
  path zero/progress `0x0f00`, with current map four, four unlocked levels, RNG `0x419695fd`, and draw
  count 8,874.

  Boulders imports that exact carry and consumes all 990 34-tick frames directly from the user's
  legally local `pb0eB` PBAK. The test never installs its restart snapshot and never writes PBAK
  bytes or a derived pad trace. The independently live prefix moves from `0Q_eZ:0@0` to
  `0I_eZ:1@3840`, visiting 16 distinct camera paths through 21 path changes and 10 lifecycle zone
  transitions. It performs 37 successful spawns and 20,692 clean executions; the exact box timeline
  is frames 71/173/174/197/232/633/636/695, ending at `0x800`. The final Crash translation is
  `[2377472, 7550502, -12157440]`, RNG is `0xb4e70e26`, and draw count is 9,864. There is no restart,
  save handshake, death camera, below-zero or terminal fall, transition request, VM fault, execution
  error or checked issue.

  A separate deterministic completion route starts from the same carry, replays the legally local
  PBAK opening through frame 895, and then continues with path/state-relative input. Checkpoint ID
  `0x3b00` emits `SaveState` at frame 1,277 with translation `[2303232, 6860544, -5172480]` and saved
  pre-increment box count `0xc00`; the live count reaches `0xf00` (15 boxes). The normal end `WarpC`
  emits `Transition(0x2d)` at frame 2,210. The run records 97 successful spawns, 28,426 spawn
  attempts with 28,329 source-expected rejections, 53,886 clean executions, 26 lifecycle zone
  transitions, 48 observed camera paths and 53 path changes. It ends on `0s_eZ` path one/progress
  12,799 with RNG `0x5def7434` and draw count 11,084, without a restart, death camera, below-zero or
  terminal fall, VM error, faulted object, execution error, or checked issue. The yellow-gem
  alternate branch and box-complete gem evaluation remain outside this golden.

  Boulders' checked `LEVEL_END` exports a Level Complete carry with RNG `0x5def7434`, draw 11,084,
  and globals `game=0x500, title=15, saved-title=15, map=4, count=1, unlocked=5, island=0`. The Level
  Complete runtime imports that exact carry and requests Title `0x19` at frame 105. It performs two
  successful spawns from 210 attempts with 208 source-expected rejections and 435 clean executions,
  with no restart, death camera, below-zero or terminal fall, VM fault, execution error, or checked
  issue. Its terminal runtime globals are
  `game=0x300, title=15, saved-title=15, map=4, count=1, unlocked=5, island=0`, with RNG
  `0x031aa015` and draw 11,189. The checked Level Complete `LEVEL_END` retains those values into
  Title. The Map becomes ready at frame 10, executes 120 idle frames, taps Up, executes another 120
  idle frames, and presses Cross. It requests Upstream `0x0f` at frame 253 on `1c_pZ` path
  one/progress 2,304. Its exported carry has
  `game=0, title=15, saved-title=15, map=5, count=1, unlocked=5, island=1`, RNG `0xae2dd893`, and draw
  11,442.

  Upstream imports that carried normal-spawn session and first feeds it every one of the 934
  34-tick pad frames in the user's legally local `pb0fB`. The test does not install the recording's
  mid-level snapshot and commits neither recording bytes nor a derived pad trace. This prefix
  characterizes a phase-mismatched carried session rather than claiming authentic demo playback;
  the separate browser PBAK fixture installs the snapshot. It produces three deterministic
  `LoadState` restarts at frames 104, 231, and 816. A state-driven continuation releases Cross
  between every action, boards the live entity-23 orbital leaf, crosses platform entities 47, 46,
  and 54, and uses fresh Square taps every 18 frames to suppress repeated lethal contact from
  entity 55. BoxsC subtype-four entity 57 activates on frame 1,935. Its synchronous SaveState
  captures checkpoint `0x3900`, translation `[2252800, 2350080, 15564288]`, and pre-increment box
  count zero; the live count then becomes `0x100` and spawn flags become nine. The same controller
  crosses the live RivOC chains through `0q`, `0x`, `0z`, and `0A`, including entities
  76/77/82/36/35/34, 96/108/109, and 113/112. It breaks two more counted boxes and requests the
  authored normal-end `Transition(0x2d)` on frame 3,791. The complete carried leg performs 152
  successful spawns, 52,371 attempts, 52,219 source-expected rejections, and 146,470 clean
  executions. It observes 24 lifecycle zone transitions, 35 camera ranges, and 40 path changes.
  The final camera is `0A_fZ` path one/progress 8,352; Crash is
  `[2228500, 6590796, -472100]`, live box count is `0x400`, RNG is `0xa7ef4deb`, and draw count is
  2,975. There is no post-prefix restart, death camera, below-zero or terminal fall, VM fault,
  execution error, unexpected spawn error, or checked issue.

  Upstream's checked `LEVEL_END` exports globals
  `game=0x500, title=15, saved-title=15, map=5, count=1, unlocked=6, island=0`. Its Level Complete
  runtime requests Title `0x19` on frame 225 after two successful spawns, 450 attempts, 448
  source-expected rejections, and 1,212 clean executions. Its terminal globals are
  `game=0x300, title=15, saved-title=15, map=5, count=1, unlocked=6, island=0`, with RNG
  `0xbe5213fd` and draw 3,200. The Map becomes ready at frame 10, runs 120 idle frames, taps Up,
  runs another 120 idle frames, and presses Cross. It requests Papu Papu `0x0a` at frame 253 on
  `1d_pZ` path zero/progress 1,024. Its exported carry has
  `game=0, title=15, saved-title=15, map=6, count=1, unlocked=6, island=1`, RNG `0xa984c5b5`, and
  draw 3,453.

  A state-gated ordinary-pad controller completes that carried Papu Papu fight. It approaches the
  arena center with bounded jump windows and retreats outward while ChefC is hurt; it does not
  inject a VM event, object state, or transition. Same-frame ChefC-contact/Crash-event-zero damage
  pairs occur on frames 302/484/666. Entity eight enters ChefC state two on frames 303/485/667,
  recovers to state one on 382/564, and enters win state three on 668. The authored runtime requests
  Title `0x19` on frame 812. It records 6 successful spawns, 5,684 attempts, 5,678 source-expected
  rejections, 16,377 clean executions, three camera ranges, two path changes, no restart or death
  camera, and no terminal fall (the only below-zero sample is an observed eight-unit grounded
  rounding). It has no VM fault, execution error, unexpected spawn error, or checked issue. The
  checked boss carry has
  `game=0x300, title=15, saved-title=15, map=6, count=1, unlocked=7, island=0`, RNG `0xf3ab9165`,
  and draw 4,265.

  The post-boss Map becomes ready on frame 10 at `1e_pZ` path zero/progress `0x1500`. It waits for
  the frame-52 current-node gate at `1d_pZ:0@0x0400`, taps Up on frame 53 and releases on 54, waits
  for `1d_pZ:1@0x0300` on frame 65, and presses Cross on 66. The authored transition requests
  Rolling Stones `0x15`; the clean `LEVEL_END` carry has map/unlocked seven, island one, and draw
  4,331. Rolling Stones imports that exact carry and executes an ordinary-pad normal route through
  `0M_lZ -> 0O_lZ`, bypassing alternate `0N_lZ`. It enters end `WarpC` and requests Level Complete
  `0x2d` on frame 2,450 with no restart, state-31 squash, death camera, terminal fall, VM fault,
  execution error, or LoadState. It activates checkpoint `0x0800` on frame 1,160, retains saved
  boxes `0x0a00`, advances live boxes to `0x0c00`, and records 117 successful spawns, 29,236
  attempts, 29,119 source-expected rejections, and 55,106 clean executions across 32 lifecycle zone
  transitions, 45 camera ranges, and 46 path changes. The final camera is `0O_lZ:0@12199`; Crash is
  in warp state 32 at `[2101120, 9256238, -1866496]`. RNG is `0x96bb47ac` and draw is 6,781. This
  is deterministic native integration over user-supplied local data, not a browser playthrough or
  full-game parity claim. The complete carried chain has not been exercised in a browser.
- The legally local Rolling Stones (`0x15`) direct-boot route uses only ordinary 30 Hz pad words and
  requests Level Complete `0x2d` on frame 2,448 with no restart, state-31 squash, death camera,
  terminal fall, VM fault, execution error, or LoadState. It performs 117 successful spawns from
  29,223 attempts with 29,106 source-expected rejections and 55,226 clean executions across 32
  lifecycle zone transitions, 45 camera ranges, and 46 path changes. The route breaks its
  authored opening wall, defeats PlanC entities 18/49/57 and turtle entities 15/72, times jumps over
  JunOC entity 69, and avoids JunOC entities 75/77/52's `0x1900` squash paths using ordinary
  neutral/run/jump windows. BoxsC subtype-four entity eight activates on frame 1,160. The
  synchronous SaveState records checkpoint `0x0800`, player `[2815232, 2979072, 17458688]`, and
  pre-increment box count `0x0a00`; the live count then becomes `0x0b00` and spawn flags become
  nine. BoxsC entity 92 breaks on frame 1,860 and advances the live count to `0x0c00`. The route
  retains `0I -> 0J -> 0K`, avoids category-`0x300` entity 103 with an ordinary right/left lane
  change, and crosses the successive `0K` pads. Three terrain jumps carry Crash from physical `0M`
  into normal-route `0O` without entering alternate `0N`; a short right-jump enters end `WarpC`.
  WarpC executes states zero through four before the transition. The final camera is
  `0O_lZ:0@12199`, Crash is `[2101152, 9256238, -1866496]` in warp state 32, and RNG is
  `0x9e602d68` at draw 2,448. The post-Papu chain independently reaches the same authored end two
  frames later.
- Hog Wild (`0x11`) now has a complete direct-boot route using ordinary 30 Hz pad words. It
  traverses 67 camera paths/66 changes and 57 lifecycle transitions, activates checkpoints 13 and
  30, advances live boxes to `0x700`, observes WarpC states zero through four, and requests Level
  Complete `0x2d` on frame 1,950. It records 39 successful spawns, 5,857 attempts, 5,818
  source-expected rejections, and 24,311 clean executions. The final camera is
  `1M_hZ:0@10239`, Crash is `[5395712, 13171420, -31800992]`, and RNG is `0xc3448148` after
  1,950 draws. It has no restart, LoadState, fatal-surface state 39, death camera, below-zero or
  terminal fall, VM fault, execution error, or checked issue. A separate 360-frame idle route pins
  its authored restart frames at 178 and 355.
- A separate card-to-map regression restores the retail 128-byte payload at level count eight,
  mounts Hog Wild's `1e_pZ` node, and boots `0x11` through the authored Cross transition. It then
  carries the complete Hog Wild route into Level Complete, returns to Title/Map after 273 frames,
  and uses an ordinary Up/Cross selection to boot newly unlocked Native Fortress (`0x1a`) on map
  frame 253. The checked carry retains exact title/map/unlock globals, RNG and draw count, with no
  restart, VM fault, or execution error. This proves the post-Hog unlock handoff, not completion of
  Native Fortress. Separate fresh-boot goldens characterize both the first greasy-platform
  boundary and the ordinary-pad crossing into `a7_qZ` below.
- Native Fortress (`0x1a`) retains its bounded 550-frame ordinary-pad direct route through the
  opening to the first subtype-two greasy `WalOC`. The final camera is `a6_qZ` path one/progress
  5,548; Crash is alive, grounded in state one, and positioned at
  `[6522624, -11086492, 118784]`. A separate extended profile hops right to the preceding shelf,
  reverses left for four grounded samples, and carries the running jump across that first grease
  segment. At frame 650 its camera is `a7_qZ` path zero/progress 6,240 and Crash is alive, grounded
  in state one, and positioned at `[5885696, -10880527, 118784]`; the next subtype-two `WalOC`
  entity 33 remains live in state 11. The extension records 19 successful spawns from 4,109
  attempts with 4,090 source-expected rejections, 10,880 clean executions, six lifecycle
  transitions, nine camera ranges and eight path changes. It has no restart, death camera,
  terminal fall, level transition, VM fault, execution error, or checked issue. This establishes
  the authored handoff into `a7_qZ`, not a level-completion or browser-playthrough claim.
- Up the Creek (`0x18`) has an exact ordinary-pad opening route beyond its first two authored
  moving logs. It clears the opening boxes and four small-platform jumps, brakes onto executable-28
  entity 30, waits for the log's forward arc, builds speed while supported, and transfers to entity
  31. At frame 370 Crash remains alive and supported at `[2047648, 1166978, 27486512]`; the camera
  is `0d_oZ` path one/progress 7,046. The run performs 26 successful spawns and 12,348 clean
  executions across three lifecycle transitions, seven camera ranges and eight path changes, with
  no restart, death/fall, VM fault, execution error, or LoadState. Local collision characterization
  also established that entity 54 sends `HIT` and is a hazard, not the intended handoff surface.
  The continued route reaches the first static `0f_oZ` island at the 500-frame boundary. Crash is
  `[2075548, 1414590, 26064412]` over raw ZDAT cell `0x0003`, whose top is `Y=1414592`; the player
  has floor-impact stamp 499, impact velocity `-2049723`, entity reference zero, and node
  `0xffff`. Those values distinguish static floor contact from the preceding entity-59 carried
  bounce. The 500-frame run records 42 successful spawns and 16,673 clean executions with no
  restart, LoadState, fall, VM fault, or execution error.
  The same controller crosses the raised stepping stone and reaches `0g_oZ`, where contact advances
  platform entity 44 from state 11 to 12. It then crosses the `0g`/`0h` orbiters, the `0i` sinking
  platform handoff and two counted crates, defeats hazard entity 72, crosses entity 71, and
  activates checkpoint entity 76 on frame 1,057. `SaveState` retains translation
  `[2048000, 1738240, 19455744]`, camera progress 249 and the source-ordered pre-increment box count
  `0x200`; the live count becomes `0x300`.
  The continued route catches and rides entity 75 into `0l_oZ`. At frame 1,200 Crash is grounded
  on that entity at `[1924752, 1779842, 18337744]`, with its tagged collider and floor-impact
  fields resolving exactly. After one complete platform cycle, a four-sample Up+Right run-up and
  Cross transfer lands on authored entity 80. At frame 1,450 Crash is grounded in `0m_oZ` at
  `[2372776, 1719394, 17903224]`; entity 80 is at `[2252032, 1661440, 17816832]`, the collider tag
  resolves to 80, and the camera is `0l_oZ` path one/progress 6,912. The controller then waits 39
  neutral samples and uses four Up samples plus Up/Cross to land on vertical platform entity 88.
  At frame 1,520 Crash is grounded at `[2372776, 1720258, 17415800]`; entity 88 is in state 13 at
  `[2252032, 1683968, 17305600]`, the collider tag resolves to 88, and the camera is `0m_oZ` path
  zero/progress 4,288. The complete bounded run records 94 successful spawns from 22,566 attempts,
  22,472 source-expected rejections, 48,147 clean executions, 16 lifecycle transitions, 21 camera
  ranges and 26 path changes. It has no restart, LoadState, fall, VM fault, execution error, or
  checked issue. This is not an Up the Creek completion or browser-playthrough claim.
- Ripper Roo's legally local 300-frame idle characterization matches the source project's current
  allocator behavior. RooOC requests one ordinary executable-39/subtype-one child on every enabled
  draw; non-reclaiming children fill the 96-slot pool on frame 80, RRooC releases and immediately
  reuses one slot on frame 152, and subsequent saturated requests correctly return null. RRooC
  still enters state one and traverses its deterministic pad loop without a fault. This narrows an
  inherited source boundary; it is not evidence for a Big TNT completion path. A separate
  10,000-frame ordinary-pad sweep produced 123 fall restarts but no transition. A targeted route
  reached Ripper's pad and delivered repeated `WillC` to `RRooC` spin-hit event `0x400`; Ripper
  correctly stayed in state one, then returned event `0x300` to damage Crash. No executable-34
  `BoxsC` spawn request appeared in a 1,000-frame trace, while persistent `RooOC` children filled
  the ordinary pool. The direct-boot graph therefore has no observed controller-only TNT/win path;
  adding one would require speculative object or VM mutation.
- The full browser-scene PBAK test separately selected all nine recordings (`0x0a`, `0x0c`, `0x0e`,
  `0x0f`, `0x12`, `0x1c`, `0x1d`, `0x20`, and `0x29`) and passed every complete recording: 10,966
  recorded pad boundaries in aggregate. Each legal direct-mount fixture completed its final input
  handshake without a caption-handler fault. The `pb0cB` run included its authored same-level
  restart, built each non-restart scene, and retained exact transient `FruiC` incarnation checks.
- The locked workspace currently enumerates 976 tests: 888 default-active and 88 ignored
  legally-local tests. The complete 885-test default gate and the complete then-current 80-test
  owned-data sweep passed before the two Hog Wild tests were added; both new Hog tests, the direct
  Rolling Stones completion test, the Ripper Roo pool characterization, the Native Fortress first-
  grease and `a7_qZ` tests, the Up the Creek log-transfer/static-island/`0g` tests, and the full
  carried completion regression pass independently on the current tree. The three alias-lifetime tests
  and the complete current 553-test simulation library plus default simulation integration suite also
  pass on this tree.
  Rustfmt and warnings-denied simulation Clippy also pass. The last complete browser/release gate
  remains the earlier documented Rustfmt, native/Wasm Clippy, Node, native release, Wasm release,
  `npm run build`, and distribution-verifier run; those gates must be repeated before delivery.
- The checkpoint-specific in-app-browser pass loaded the release picker at 1,280 pixels wide,
  exposed the matching build identity in the DOM, reported 43 playable pairs, 30 Hz standby and all
  15 virtual-card slots, showed no horizontal overflow, and produced no warning/error diagnostics.
  It did not inject local game bytes into this exact artifact: the browser automation API cannot
  operate the native file chooser, so the authored completion route above is native legal-data
  evidence rather than a new browser gameplay claim. The user can select the local BIN or S0-S3
  streams directly in the visible page; those bytes remain local to the tab.

## Reproducible commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy -p crust-web --target wasm32-unknown-unknown --locked -- -D warnings
cargo test --workspace --all-targets --locked
C1_DISC_IMAGE=/path/to/disc.bin C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-formats --test local_disc -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-renderer --test local_loading_images -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin C1_STREAM_DIR=/path/to/streams \
  cargo test --workspace --all-targets --locked -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-formats --test local_pbak --locked -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  prepares_every_legally_local_recording_without_copying_game_data -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --test local_title_mdat_runtime --locked \
  authored_main_menu_map_to_n_sanity_handoff_preserves_session_carry \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --test local_title_mdat_runtime --locked \
  map_island_one_point_trailing_path_alias_is_characterized \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams C1_SURVEY_REQUIRE_CLEAN=1 C1_PROGRESSION_FRAMES=2100 \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  n_sanity_goal_directed_input_characterizes_progression -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  n_sanity_checkpoint_survives_an_authored_death_restart -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  n_sanity_idle_paging_matches_the_legal_360_frame_trace -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  authored_first_five_levels_and_papu_reach_rolling_stones_with_session_carry \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  rolling_stones_direct_boot_reaches_zero_m_boundary_pad -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  hog_wild_direct_boot_reaches_level_complete -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  hog_wild_idle_restarts_on_the_authored_surface_cadence \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  hog_wild_completion_unlocks_native_fortress_through_authored_map \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  native_fortress_ordinary_pad_route_reaches_first_greasy_platform_boundary \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  native_fortress_ordinary_pad_route_crosses_first_greasy_platform_into_a7 \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  up_the_creek_direct_route_reaches_second_moving_log \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  up_the_creek_direct_route_reaches_static_zero_f_island \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  up_the_creek_direct_route_activates_zero_g_platform \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  ripper_roo_idle_matches_source_hop_loop_and_pool_boundary \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  jungle_rollers_tawna_bonus_warp_loads_the_carried_parent_snapshot \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  jungle_rollers_three_tawna_crates_enter_the_authored_bonus \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_gool --locked \
  tawna_bonus_warpc_uses_the_exact_authored_player_proximity_gate \
  -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  n_sanity_a3_authored_crate_pair_has_native_bidirectional_links -- --ignored --exact
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  brio_boxsc_creator_link_survives_brioc_pool_reclaim -- --ignored --exact --nocapture
C1_STREAM_DIR=/path/to/streams C1_SURVEY_LEVEL=11 C1_SURVEY_FRAMES=360 \
  C1_SURVEY_REQUIRE_CLEAN=1 cargo test -p crust-sim --test local_retail_idle_survey --locked \
  every_bootable_pair_runs_a_browser_ordered_idle_window -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams C1_SURVEY_ACTIVE_INPUT=1 C1_SURVEY_FRAMES=1800 \
  C1_SURVEY_REQUIRE_CLEAN=1 cargo test -p crust-sim --test local_retail_idle_survey --locked \
  every_bootable_pair_runs_a_browser_ordered_idle_window -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams C1_SURVEY_LEVEL=0x15 C1_SURVEY_ACTIVE_INPUT=1 \
  C1_SURVEY_FRAMES=1800 C1_SURVEY_REQUIRE_CLEAN=1 \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  every_bootable_pair_runs_a_browser_ordered_idle_window -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams C1_SURVEY_LEVEL=0x1d C1_SURVEY_ACTIVE_INPUT=1 \
  C1_SURVEY_FRAMES=1800 C1_SURVEY_REQUIRE_CLEAN=1 \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  every_bootable_pair_runs_a_browser_ordered_idle_window -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_intro_terminal_start --locked \
  -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-sim --test local_retail_runtime --locked -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_camera --locked -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  every_non_title_camera_drives_300_pair_scoped_scene_builds -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  n_sanity_gool_objects_project_through_the_pair_scoped_scene -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  n_sanity_authored_pause_panel_blinks_five_willt_fragment_quads -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  upstream_ripple_moves_visible_effect_vertices_from_the_retail_wgeo -- --ignored --nocapture
cargo test -p crust-web --lib --locked \
  rejected_render_object_snapshot_is_not_replaced_by_an_empty_scene
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  every_local_fog_start_shades_projected_world_colors -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-formats --test local_scene_formats --locked \
  dynamic_world_shader_zones_match_the_reachable_retail_corpus -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --lib --locked \
  every_local_dynamic_shader_start_reaches_projected_world_colors -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-web --test local_great_hall_dynamic_font --locked -- \
  --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_gool --locked \
  retail_payloads_of_all_five_kinds_survive_a_process_storage_alias -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_ending_return_reclaim --locked \
  -- --ignored --nocapture
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-web --lib --locked \
  builds_every_fractional_spawn_snapshot_directly_from_raw_disc -- --ignored --nocapture
cargo build --workspace --release --locked
cargo build --release --locked --target wasm32-unknown-unknown -p crust-web
npm run build
```

The delivery summary identifies the exact published commit.
