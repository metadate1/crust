# Verification record

This file records observed checks for the initial private rewrite delivery on 2026-07-12, the
stream, title, GOOL, entity, SLST, camera, cached-scene and hosted-runtime slices on 2026-07-13,
and the title-overlay, PBAK, object-shader and current-zone collision slices on 2026-07-14. It does
not turn subsystem tests into a claim of retail gameplay parity.

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

The 2026-07-13 and 2026-07-14 opt-in tests used the same legally owned raw BIN in place and did not
copy any disc or stream bytes into the repository:

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
- The fixed-layout type-three font sweep found 19 validated text/font pairs containing 1,214 terms.
  All terms passed bounded four-argument formatting; 1,182 projected safely into 4,265 glyph or
  backdrop quads and 64 representative glyph textures decoded. The remaining 32 are unused
  lowercase sentinel terms that Rust rejects instead of indexing beyond retail's 63-glyph table.
  Across 42 non-title idle boots, 531 live type-four text frames emitted 3,894 textured quads; the
  same trace also exercised 50,714 sprite and 177 fragment frames. No dynamic-font override became
  live in that idle window, so default-vs-override selection remains covered by a focused unit test.
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
  continuing to its final input handshake. These traces honor display-mask/spin-death camera
  suppression, apply camera-emitted zone TERM/lifecycle transitions and save handshakes, refresh
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
- The exact Jungle Rollers `pb0cB` integration trace builds every scene through frame 231 and
  checks every contained object execution. It covers `FruiC` raw sprite shifts 24, 26, 28, 31, 34,
  246, 271 and 297 with their low-five-bit effective values, and verifies the caption's
  executable-four/subtype-nine child keeps a null lifecycle zone while using the current ZDAT for
  environment/colors.

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
the latest claimed end-to-end local-disc browser exercise.

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

## Current change-set verification

The final checks below were run against this change set on 2026-07-14:

- `cargo fmt --all -- --check` passed.
- locked workspace Clippy across all native targets and an explicit `wasm32-unknown-unknown`
  `crust-web` Clippy pass both completed with warnings denied.
- the locked asset-free workspace suite passed all 728 default tests across 33 targets. Another 52
  legally local tests remain ignored by default, for 780 listed tests across all targets.
- the complete legally local ignored sweep used the supplied raw BIN and read-only extracted
  streams in place. All 52 selected tests passed with zero failures, including every
  raw-disc/catalog, all-pair parser, camera, title, audio, PBAK, renderer and runtime golden.
- executable-`0x22` crate coverage now checks the native strict adjacency boundary, checked
  bidirectional misc-A links, skipped-lower-crate Y compaction, activation/restart reset, stagger
  calculation and stale-reference cleanup before VM-handle reuse. The opt-in local golden confirms
  that authored N. Sanity `a3_9Z` entities 23 and 24 are linked in both directions.
- the corrected legally local 2,100-frame N. Sanity invocation passes. Its controller follows
  `b5_9Z:p4 → b5_9Z:p1 → b6_9Z:p0`, reaches `b7_9Z`'s `WarpC`, and emits the authored
  `Transition(0x2d)` at frame 1,906. It records 18 zone transitions, 42 observed paths, 65
  successful spawns and 32,808 GOOL executions with zero restarts, below-zero or terminal falls,
  VM errors or faulted objects. The former b5/b6 stop was caused by missing test-controller route
  actions at authored static cells; a later b7 stop came from steering `LEFT` around the live portal
  lane. Correcting those inputs required no camera or collision runtime change. This deterministic
  local test is not a browser playthrough or a claim of full retail parity.
- the legal Jungle Rollers PBAK scene test passed after pinning the first source-correct `FruiC`
  incarnation: synchronous `0x83`/`0x84` local-bound refresh moves its first shrink-four frame to
  wall frame 190/pad boundary 191, followed by the exact raw/effective shift checkpoints through
  frame 217.
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
  the live VM.
- the object-only graphics-flag `0x1000` camera has a cross-crate fixed-point golden covering its
  direct pitch matrix, fixed/bobbing translation and camera-space point. A separate clock test
  proves GOOL `frames_elapsed` advances while texture `draw_count` is frozen; scene locations carry
  both values so hidden/loading frames cannot desynchronize shading from geometry.
- locked optimized native and `wasm32-unknown-unknown` workspace builds passed, as did the generated
  web release. The Wasm payload is 1,230,231 bytes with SHA-256
  `6acdcd1002041099e82ac03d2a6319988049bf9abf6c4b38dd62e77c5ba9ca8c`.
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

## Reproducible commands

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
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
C1_STREAM_DIR=/path/to/streams C1_SURVEY_REQUIRE_CLEAN=1 C1_PROGRESSION_FRAMES=2100 \
  cargo test -p crust-sim --test local_retail_idle_survey --locked \
  n_sanity_goal_directed_input_characterizes_progression -- --ignored --nocapture
C1_STREAM_DIR=/path/to/streams \
  cargo test -p crust-sim --test local_retail_runtime --locked \
  n_sanity_a3_authored_crate_pair_has_native_bidirectional_links -- --ignored --exact
C1_STREAM_DIR=/path/to/streams C1_SURVEY_LEVEL=11 C1_SURVEY_FRAMES=360 \
  C1_SURVEY_REQUIRE_CLEAN=1 cargo test -p crust-sim --test local_retail_idle_survey --locked \
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
C1_DISC_IMAGE=/path/to/disc.bin \
  cargo test -p crust-web --lib --locked \
  builds_every_fractional_spawn_snapshot_directly_from_raw_disc -- --ignored --nocapture
cargo build --workspace --release --locked
cargo build --release --locked --target wasm32-unknown-unknown -p crust-web
npm run build
```

The delivery summary identifies the exact published commit.
