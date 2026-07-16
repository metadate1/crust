# crust

`crust` is a browser-only Rust/Wasm interoperability rewrite of the C1 game-engine lineage. It
loads data from a legally owned Crash Bandicoot NTSC-U disc locally, recognizes the S0–S3 stream
set, and runs without uploading or bundling game data.

> **Private repository.** No public distribution license is granted. C1's upstream licensing is
> unresolved and the original game rights require separate review. Keep source and builds private;
> see [NOTICE.md](NOTICE.md).

## Implementation status

This repository is a working, tested Rust/Wasm compatibility foundation, but it is **not yet a
retail-equivalent game runtime**. Disc extraction, all 44 stream pairs, checked NSD/NSF parsing,
the cooperative 30 Hz scheduler, mounted retail title/menu/options/password/load/map object flows,
direct boot, local persistence, input, WebGL2, WebAudio, and substantial native engine subsystems
are implemented. The browser does not advance synthetic title or gameplay flow when authored
retail objects are unavailable. The live browser host retains and remounts each validated
destination stream pair, decodes retail LDAT loading
images, composes image-backed retail title states from MDAT/IPAL/IMAG entries, and drives the
renderer command backend. Title presentation preserves the source type-zero MDAT category mask,
latches display/animate through the source `GOOL → TitleUpdate → TitleLoadState → GLUpdate`
transaction and draws the native 16-level nonlinear black overlay. `RetailRuntime` owns that
transition; `RetailFlowMirror` is a passive screen mirror and cannot advance a second fade clock. The 4:3
output is authored-scene only: until mounted retail objects produce a scene, it
shows a loading/error diagnostic with no synthetic menu, password/options UI, or gameplay geometry.
For the 40 world-bearing playable starts, gameplay presents bounds-checked
ZDAT/SLST/WGEO path snapshots with decoded TPAG textures and retail camera/depth math. The
loading-image path follows the observed two-tick presentation gate and uses the first presented
path point and texture-animation count; N. Sanity Beach resolves to 679 visible polygons at that
boundary. After that gate, `RetailCameraRuntime` owns the exact zone, path and signed-8.8 progress
used to rebuild SLST visibility, camera projection and animated texture selection. Source-derived
automatic modes 0/1/3, tapped transition skipping and path/zone crossings are live. Modes 5/6 feed
the hosted main object's typed transform, camera zoom, held pad and frame stamp into the checked
`CamFollow` projection/neighbor/smoothing path whenever that object is available. Its gem-path
neighbor gate reads the live authored `gem_stamp` global and retains the source
`frames_elapsed - gem_stamp <= 15` window.
On the island-map title state, WGEO item three is parsed with its unusual serialized
`len + type-as-record-zero` layout. The renderer carries the active path group across worlds and
applies globals 73/75 as non-mutating per-frame animation-mask overrides, matching
`GfxAnimMapPaths` without writing into user-supplied stream bytes. The last effective masks persist
through the map's fade-out, matching the native resident-WGEO write lifetime. Camera modes
seven/eight consume the authored island globals with their distinct source ordering: mode seven
publishes its next state before `LevelUpdate`, while mode eight publishes after the synchronous
`LevelUpdate`/TERM boundary. Both complete before the following GOOL traversal, so the normal Main
Menu → Map → N. Sanity Cross route emits and mounts retail level `0x09` rather than stopping at a
host boundary.

The browser now owns a checked retail object runtime for title, gameplay, bonus, boss, level-
complete, intro and ending states.
At the cooperative 30 Hz boundary it scans displayed current-zone neighbors, spawns group-three
ZDAT entities into the bounded arena, binds their GOOL programs from the mounted NSF, applies hosted
child-spawn effects, and preserves typed arena/VM links. Entity objects now receive their
zone-relative path position, rotation/mode flags, scale, process defaults, player/object color
matrix and typed parent/player links; runtime children inherit their parent's transform.
Every gameplay, boss, bonus, and map mount also creates the native executable-four life, fruit,
and pickup HUD roots before the first zone scan and publishes their checked tagged references to
the exact GOOL globals used by pickups, saves, and bonus routing. Title, level-complete, intro, and
ending mounts retain the source's no-HUD branch. Before those roots run, initial and destination
mounts perform the source `CoreObjectsCreate` pad-history shift; an armed attract recording forces
the new held/tapped words to zero while retaining prior history. The mount then runs the
object-creating half of
`LevelInitMisc(1)`: its six applicable levels receive the native root-four controller, including
Ripper Roo's executable-39/subtype-four controller and checked `ambiance_obj` global-eight link.
State changes rebind at the synchronous host boundary: a captured once block runs before the state
stamp, then the target transition block runs after it, including nested calls and hosted spawns;
normal updates continue into newly bound state code in that same native update. Initial/call frames
share the bounded
process word array at `init_sp`, state links apply target-state guards, and checked failures
quarantine only the affected object. GOOL `0x8b` open cases one/six, close case two, and probe case
three cross a typed synchronous pager boundary; cases four/five remain VM-local. An unavailable
open rolls back its optimistic VM reference, a resident replacement re-arms the displaced page,
and mismatched EID/page acknowledgements are rejected. Checked aligned code/storage/entry tags,
five-word pad history, camera-relative movement, gravity, rotation, every source selector in the
`0x85` transform-vector and `0x8e` solid/color families, and `SZON`'s reverse current-header
neighbor search are implemented without native pointers or C undefined behavior. Misc 12/7 also
performs the distinct forward current-header neighbor TERM sweep through the typed object host.
GOOL `0x14` (LEA) preserves input-before-output address translation and represents process-local
animations as checked same-object handles. Descriptors in internal or register storage are
revalidated from the live aliased words: type one supplies its model to both bounds and vertex
rendering, types two/four/five use the sprite, text, and fragment paths, and type three remains a
resource-only no-draw selection. Type zero and unknown type bytes follow the native switch default
with no draw and the standard non-vertex collision bound, including Toxic Waste's observed `BaraC`
case. Foreign-object, external-state-table, and rotating-constant-buffer aliases are rejected
because their backing lifetimes cannot yet be represented without silently retargeting a native
pointer. Opcode `0x81` retains the native interpreter's intentional one-cycle no-op behavior.
`GoolObjectColors` now delivers Crash's category-`0x300` invincibility-hit event `0x0a00`
synchronously before the same object's physics, so authored enemy handlers can change that frame's
motion or state. The source's `argc=1`/null-argument quirk is represented by one checked zero word
instead of dereferencing null; sender/recipient generations are validated, the hosted path emits no
duplicate queued event, and ignored native handler failures remain available as typed diagnostics.
The WebGL stage transactionally replaces the camera/path scene while reusing shared
immutable texture allocations. Parsed item-five animation descriptors now resolve pair-scoped
TGEO plus 3D SVTX/CVTX frames, type-two sprites, type-five fragments and type-four text through its
header-length-bounded type-three font resources, including the extended controller-icon records
that retail pointer-indexes beyond its 63-slot C declaration. Each object's post-update,
pre-child display boundary appends an owned ordered record, so later teardown or reparenting cannot
retract already displayed state. A frame-start pager snapshot supplies world/filter membership,
while each record replays its live `(EID, generation, page)` texture map before fixed-point
projection, lighting/color modulation, and ordering through the shared resident TPAG cache;
the status-B 2D CVTX path uses the shared retail sprite matrix. Sprite and fragment half-size math
uses the MIPS variable-shift low five bits and explicit signed 32-bit wrapping before the checked
GTE validity gate; focused arithmetic goldens cover raw counts beyond 31 without turning a
saturating/cullable sprite into a runtime failure. The legal `pb0cB` trace separately pins two
successive `FruiC` physical generations that reuse one compact VM slot, their exact authored scale
sequence, and reclamation after the second child. Eligible
animation bounds follow the native Crash-stamp schedule. Same-stamp objects register their
transformed frame bound before GOOL and physics and execute the same-stamp Crash
collision-link/hotspot tail; objects visited before Crash register after physics when they remain
inside the exact `±0x7d000/±0xaf000/±0x7d000` box. Opcodes `0x83` and `0x84` synchronously refresh
only the persistent local bound at their source call site.
The mover's current collider is retained as a validated snapshot of the live link-six object's
translation, status, state flags, object type, and hotspot size, independent of whether that object
is present in the current bounded frame-candidate slice. Synchronous collision handlers can replace
the link, after which the remaining native solid phases re-resolve the new live object. Hotspot
insets also preserve their raw source endpoint order: a large inset may invert an axis, and the
following direct face comparisons do not normalize it. This exact case previously stopped Rolling
Stones; its legally local active-input regression now completes 1,800 clean simulation frames.
WGEO zones with graphics flag `0x100` now apply the source ripple transform to effect-marked world
vertices before the ordinary world matrix. The independent 16-cell signed wave uses the native
seed, advance, wrap, absolute-value conversion, and level-specific speed/period table. Pair-scoped
wave state advances only for an unpaused submission containing visible ripple-world polygons.
Pause or a world-hidden/empty submission freezes it independently of texture animation; a later
draw-skip presentation gate still performs the source transform and advances the wave.
World graphics now also execute the complete source-priority `Dark2 > Dark > Fog > Ripple >
Lightning > Plain` dispatch. The process-lifetime `ShaderParamsUpdate` state covers every fixed,
random, ruins, boss and thunder sequence; combined Dark applies lightning before its non-backdrop-
exempt fog pass, and Dark2 follows the live doctor/Crash illumination point plus torch distance and
ambient ramps. Native `far_color1` scratch persists across stream mounts, including Dark2's
intentional reuse, and hidden draw-skip frames still transform worlds before presentation. The
doctor global and pointer-shaped process words retain physical pool-slot identity beside their raw
32-bit tags. Before a retail object is reclaimed, the VM captures provenance for existing process
links, registers, stack words, and internal/external MOV storage instead of nulling inbound links.
Linked register reads and writes address the live occupant when the slot is allocated and its
retained process storage while the slot is free; reusing that same physical slot retargets the old
pointer, while compact-handle reuse elsewhere does not. The 96 ordinary slots follow the native
free-list parent/sibling mutations and LIFO reuse. The dedicated main slot stays outside that list,
and pool binding is preflighted before it is committed so a failed association cannot leave a
partially installed object.

Reallocation seeds the replacement from the slot's retained process words, then applies native's
selective in-place initialization. Raw `sp`, `pc`, `fp`, `tp`, and `ep` words are reset along with
the other source-initialized fields; untouched process words keep their previous values. This
covers Jaws of Darkness's reclaimed `fruit_hud` copy/read and the legal Dr. N. Brio boundary where
eight live `BoxsC` children retain creator link four after `BriOC` is reclaimed. Event argv,
mapped-state rebinds, and child-spawn creation arguments carry the same physical-slot provenance;
copying a pointer through EARG or an owned host request therefore cannot retarget it through compact
handle reuse. Linked address-taking now uses a separate validated 32-bit physical-pool storage tag:
it remains readable and writable while a slot is free and retargets only when that exact slot is
reused. The separately allocated player/main address is non-null from machine initialization,
persists through Title teardown, and remains outside the ordinary free list. Exact retention of an
ordinary replacement's preexisting local-bound bytes and the dedicated allocation's extra 0x100
stack-tail bytes remain open; legal-corpus audits have not yet observed either boundary in use.
Writes that would corrupt the three allocator-owned link words of an ordinary free slot are
deliberately rejected instead of reproducing the native C allocator's unsafe malformed-list state.
The separate zero-initialized RNG-B word is shared in source order by lighting, PBAK choice and
audio voice stealing. Accepted lightning cues decode one of the local `lt1rA`–`lt3rA` ADIO entries
and create an ownerless delayed-key voice without copying sample bytes into the repository.
Static solid geometry follows native `cur_zone` as the camera crosses zones instead of remaining
bound to Crash's spawn zone; a detached object zone remains typed and supplies only its source
rectangle/graphics/water fallback, never extra geometry candidates. A previously recorded strict
18,000-frame state-aware input trace carried the camera and Crash from `e0_9Z` through the complete
`a0_9Z`–`b7_9Z` authored chain and requested Level Complete `0x2d` at frame 1,995 with no VM error,
faulted object, death restart, below-zero position, or terminal fall. The current native-schedule
characterization also completes: a legally local 2,100-frame invocation follows
`b5_9Z:p4 → b5_9Z:p1 → b6_9Z:p0`, reaches `b7_9Z`'s `WarpC`, and emits
`Transition(0x2d)` at frame 1,900. It records 18 zone transitions, 42 observed paths, 65 successful
spawns and 40,881 GOOL executions with zero restarts, falls, VM errors or faulted objects. The
former b5/b6 stop came from missing route actions in the test controller at authored static cells;
the later b7 stop came from steering `LEFT` around the live portal lane. Correcting those route
actions required no camera or collision runtime change. The current six-frame change instead comes
from restoring `PlotObjWalls(flag=1)`'s ordered `GoolCollide` calls for overlapping frame bounds.
`docs/VERIFICATION.md` records the exact invocation and boundaries.
An opt-in legally local vertical-flow test now keeps the native process session intact across five
normal-level completions and the next Map choice into Papu Papu. A fresh authored Map initialized
through the card-payload restore path emits N. Sanity Beach `0x09` on frame 11. N. Sanity emits Level Complete
`0x2d` at frame 1,900, its checked `LEVEL_END` phase exports `RetailSessionCarry`; Level Complete
imports that carry and emits Title `0x19` at
frame 513, and Title imports the second carry into its parsed graph, ZDAT entities, lifecycle, and
map camera schedule. The post-completion Map unlocks level two; after the authored Up/Cross route
it reaches `1b_pZ` path zero at progress `0x0b00` and emits Jungle Rollers `0x0c` on frame 253. The
same uninterrupted test imports that carry into Jungle Rollers, flings both early PlanC hazards,
breaks four counted boxes, and reaches checkpoint entity 46 at frame 1,117 with the exact saved
checkpoint translation/count. It then continues through the remaining main-path route and enters
the end `WarpC`. The warp emits
`Transition(0x2d)` at frame 2,546 with a live counted box total of `0x1000` and no restart, death
camera, below-zero or terminal fall, VM error, faulted object, or checked issue. Jungle raises the
unlocked count to three before its checked `LEVEL_END`; its Level Complete screen emits Title on
frame 306, and the remounted Map takes Up/Cross to select The Great Gate `0x12` on frame 253 at
`1c_pZ` path zero
and progress `0x0200`. Current map level three, level count one, three unlocked levels, RNG and draw
state survive the final checked Map handoff. All seven outgoing `LEVEL_END` broadcasts in this chain
complete without a checked handler failure. The Great Gate then runs an exact carried retail-pad
route to completion: it clears the horizontal `a1_iZ`-through-`a9_iZ` opening, crosses the wide pit,
cycles the `WalOC` logs through their safe phases, and chains the first three arrow-crate bounces.
Checkpoint crate 76 emits its exact `SaveState` at frame 1,152 with pre-increment box count `0x900`,
checkpoint translation `[20991488, -8397312, 127744]`, and live count `0xa00`. The route continues
through `b3_iZ`-`c7_iZ`, clears the snake and later hazards, enters the normal end `WarpC`, and emits
`Transition(0x2d)` at frame 2,471 with 14 counted boxes (`0xe00`). The terminal boundary retains RNG
`0x6a219f2c` and draw count 8,396 after 111 successful spawns, 47,371 clean executions, and 38
lifecycle zone transitions, with no restart, death camera, terminal fall, VM error, faulted object,
or checked issue. The yellow-gem alternate branch, box-complete gem evaluation, and browser
playthrough remain outside this native integration claim. The ordinary completion carry continues
through Level Complete to Title at frame 225 (RNG `0x2875d290`, draw 8,621), then takes the same Map
Up/Cross schedule to Boulders `0x0e` at frame 253 on `1c_pZ` path zero/progress `0x0f00` (RNG
`0x419695fd`, draw 8,874).

Boulders imports that exact carry and reads all 990 34-tick pad frames from the user's legally local
`pb0eB` PBAK without installing its recording snapshot or committing recording bytes or a derived
pad trace. The exact prefix moves from `0Q_eZ:0@0` to `0I_eZ:1@3840` across 16 camera paths, 21 path
changes and 10 lifecycle zone transitions, breaks eight counted boxes, performs 37 successful
spawns and 20,692 clean executions, and ends at Crash translation
`[2377472, 7550502, -12157440]`, RNG `0xb4e70e26`, and draw count 9,864. A separate deterministic
completion route from the same carry uses that local PBAK opening before continuing under
path/state-relative input. Checkpoint ID `0x3b00` emits `SaveState` at frame 1,277 with translation
`[2303232, 6860544, -5172480]` and saved pre-increment box count `0xc00`; the live route reaches 15
counted boxes (`0xf00`) and the normal end `WarpC`, which emits `Transition(0x2d)` at frame 2,210.
That completion golden records 97 successful spawns, 53,886 clean executions, 26 lifecycle zone
transitions, 48 observed camera paths and 53 path changes, ending at RNG `0x5def7434` and draw count
11,084 with no restart, death camera, terminal fall, VM error, faulted object, or checked issue.
Boulders' checked `LEVEL_END` exports that RNG/draw phase and globals
`game=0x500, title=15, saved-title=15, map=4, count=1, unlocked=5, island=0` to Level Complete. Its
screen requests Title `0x19` at frame 105 after two successful spawns, 210 attempts, 208
source-expected rejections, and 435 clean executions, with no restart, VM fault, or execution error.
The post-screen runtime has `game=0x300` with the other six globals unchanged, RNG `0x031aa015`, and
draw count 11,189. After the checked Title handoff, the Map waits 10 frames, follows
120-idle/Up/120-idle/Cross, and requests Upstream `0x0f` at frame 253 on `1c_pZ` path one/progress
2,304. Its carry has `game=0, title=15, saved-title=15, map=5, count=1, unlocked=5, island=1`, RNG
`0xae2dd893`, and draw count 11,442.

Upstream first feeds all 934 34-tick pad frames from the user's legally local `pb0fB` into the exact
post-Boulders normal-spawn session, without installing the recording's mid-level snapshot or
committing recording bytes or a derived pad trace. This prefix is deliberately a carried-session
stress run, not authentic demo playback; separate browser PBAK coverage installs the recording
snapshot. Its phase-mismatched input produces deterministic same-level `LoadState` restarts at
frames 104, 231, and 816. A state-driven controller then releases every Cross interval, boards the
live entity-23 orbital leaf, crosses the entity-47/46/54 platform chain, and uses fresh Square edges
every 18 frames to suppress the lethal entity-55 fish contact. It activates authentic BoxsC
subtype-four entity 57 on frame 1,935: checkpoint `0x3900`, saved pre-increment box count zero,
saved player translation `[2252800, 2350080, 15564288]`, then live box count `0x100` and native
spawn flags nine. The controller then crosses the live RivOC leaf/platform sequence through `0q`
to `0A`, including entities 76/77/82/36/35/34, 96/108/109, and the final 113/112 pair. It breaks
two more counted boxes and reaches the authored normal-end `Transition(0x2d)` on frame 3,791.
That complete Upstream leg performs 152 successful spawns from 52,371 attempts with 52,219
source-expected rejections, 146,470 clean executions, 24 lifecycle zone transitions, 35 camera
ranges and 40 path changes. It ends on `0A_fZ` path one/progress 8,352 with Crash at
`[2228500, 6590796, -472100]`, box count `0x400`, RNG `0xa7ef4deb`, and draw count 2,975, with no
post-prefix restart, death camera, terminal fall, VM fault, execution error, or checked issue.

Upstream's checked `LEVEL_END` exports globals
`game=0x500, title=15, saved-title=15, map=5, count=1, unlocked=6, island=0`. Its Level Complete
screen requests Title `0x19` on frame 225 after two successful spawns, 450 attempts, 448 expected
rejections, and 1,212 clean executions; the resulting RNG is `0xbe5213fd` at draw count 3,200.
After the checked Title handoff, the Map follows 120-idle/Up/120-idle/Cross and selects Papu Papu
`0x0a` on frame 253 at `1d_pZ` path zero/progress 1,024. Its carry has
`game=0, title=15, saved-title=15, map=6, count=1, unlocked=6, island=1`, RNG `0xa984c5b5`, and draw
count 3,453. A state-gated ordinary-pad route completes the carried Papu Papu fight without a
restart or host-injected event. Crash and ChefC exchange the three authored damage collisions on
frames 302, 484, and 666; ChefC enters hurt state two on frames 303, 485, and 667, recovers on
frames 382 and 564, and enters win state three on frame 668. The boss requests Title `0x19` on
frame 812 after 6 successful spawns, 5,684 attempts, 5,678 expected rejections, and 16,377 clean
executions. The carry unlocks level seven with RNG `0xf3ab9165` and draw 4,265.

The post-boss Map becomes ready on frame 10, waits for its authored current-node camera gate,
taps Up on frame 53, waits for the next-node gate, and presses Cross on frame 66 to select Rolling
Stones `0x15`. Its checked carry has `map=7, unlocked=7, island=1` at draw 4,331. A session-gated
ordinary-pad continuation now completes carried Rolling Stones and requests Level Complete `0x2d`
on frame 2,450. It follows the normal `0M_lZ -> 0O_lZ` leg, bypassing alternate `0N_lZ`, enters
the end `WarpC`, and ends at camera `0O_lZ:0@12199` with Crash in warp state 32 at
`[2101120, 9256238, -1866496]`. It activates checkpoint `0x0800` on frame 1,160, retains saved box
count `0x0a00`, and advances the live count to `0x0c00`. The route records 117 successful spawns
from 29,236 attempts with 29,119 source-expected rejections, 55,106 clean executions, 32 lifecycle
zone transitions, 45 camera ranges and 46 path changes. It has no restart, state-31 squash, death
camera, terminal fall, VM fault, execution error, or LoadState; RNG is `0x96bb47ac` at draw 6,781.
These are deterministic native integration goldens over user-supplied local data, not a browser
playthrough or full-game parity claim; a browser exercise of this complete carried chain remains
open.

An independent legally local direct-boot route now completes Rolling Stones using only ordinary
30 Hz pad input.
It breaks the authored opening wall and later crates, defeats PlanC entities 18/49/57 and turtle
entities 15/72, clears JunOC entity 69, and avoids the `0x1900` squash paths from JunOC entities
75/77/52. BoxsC subtype-four entity 8 still activates on frame 1,160: SaveState captures checkpoint
`0x0800`, player `[2815232, 2979072, 17458688]`, and pre-increment box count `0x0a00`; the live
count then becomes `0x0b00` and spawn flags become nine. It also breaks BoxsC entity 92 on frame
1,860, advancing the live count to `0x0c00`. Three ordinary terrain jumps carry Crash from `0M`
into normal-route `0O` without entering alternate `0N`; a short right-jump enters end `WarpC`.
WarpC executes states zero through four and requests Level Complete `0x2d` on frame 2,448. The
route records 117 successful spawns from 29,223 attempts with 29,106 source-expected rejections,
55,226 clean executions, 32 lifecycle zone transitions, 45 camera ranges and 46 path changes. Its
final camera is `0O_lZ:0@12199`, RNG is `0x9e602d68` at draw 2,448, and it has no restart,
state-31 squash, death camera, terminal fall, VM fault, execution error, or LoadState. The carried
route above independently reaches the same authored end two frames later.
The same legal N. Sanity data now characterizes its first authored interaction sequence: the first
CrabC defeat, nine ordinary counted crates, the checkpoint crate, the source-ordered pre-increment
checkpoint snapshot, a TurtC death, the 117-frame death camera, and the same-level checkpoint
restart. The checkpoint snapshot contains `0x900` before the handler's later live increment to
`0xa00`. Restart, including `LevelInitMisc(0)`'s reset, completes at frame 1,150; the next trace
sample observes zero at frame 1,151, and the respawned checkpoint recounts to `0x100` at frame
1,152. A fixed-34-tick reference-C oracle confirms the early Box7, CrabC and Box12 contact order;
the Crab gate does not emit the previously observed premature direct event `0x300`. This is a
focused deterministic route, not broad checkpoint/death certification.
Hog Wild now also has a complete direct-boot ordinary-pad route. It traverses 67 camera paths,
activates checkpoints 13 and 30, advances live boxes to `0x700`, observes WarpC states zero through
four, and requests Level Complete `0x2d` on frame 1,950. The route performs 39 successful spawns
from 5,857 attempts with 5,818 expected rejections and 24,311 clean executions, with no restart,
LoadState, fatal-surface state, death camera, terminal fall, VM fault, execution error, or checked
issue. Its RNG is `0xc3448148` at draw 1,950. A separate strict idle characterization pins the
authored fall/load-state cadence at frames 178 and 355.
Native Fortress now has a bounded direct-boot ordinary-pad route to its first greasy-platform
boundary. At frame 550 the camera is `a6_qZ` path one/progress 5,548, Crash is grounded at
`[6522624, -11086492, 118784]`, and the first subtype-two `WalOC` has executed state 11. The run
performs 17 successful spawns and 8,988 clean executions with no restart, death camera, terminal
fall, VM fault, or execution error. It stops at the last stable contact after a three-frame leftward
hop; crossing that greasy `WalOC` segment into `a7_qZ` remains an active route gap.
Up the Creek now has a bounded direct-boot ordinary-pad route beyond its first two moving logs. At
the 500-frame boundary Crash stands on `0f_oZ`'s static raw `0x0003` cell at
`[2075548, 1414590, 26064412]`; the cell top is `Y=1414592`, the player has no entity reference,
and the recorded floor-impact registers distinguish it from the preceding carried log contact. The
route then crosses the raised stepping stone and reaches `0g_oZ`, where contact advances platform
entity 44 from state 11 to 12. At frame 580 Crash remains alive and supported at
`[2074052, 1647954, 25003356]` after 18,922 clean executions and no restart, LoadState, terminal
fall, VM fault, or execution error. This is opening-route characterization through the first `0g`
platform, not an Up the Creek completion or browser-playthrough claim.
The current native schedule therefore includes seven deterministic normal-level completions, the
complete carried Upstream route, Papu Papu's authored completion, and both direct and carried
Rolling Stones completion; this is not a full retail playthrough or a browser-playthrough claim.
Broader progression, several GOOL host
operations, pixel-level rendering edge cases, later same-level restart cases, and asynchronous
CD/page-residency timing remain incomplete. Source-ordered zone lifetime and synchronous paging,
save/restart, event and audio calls, display-mask latching and local ADIO SFX are now connected.
Zone graphics now select local
retail MIDI/INST data; checked VAB/SEP decoding feeds the Rust software synth with 30-tick zone
fades, the native all-bus master fade and GOOL-controlled alternate tracks. Sampled VAB voices
apply the two retail ADSR register words through an exact fixed-point 44.1 kHz attack, decay,
sustain, and release generator before mixing. Every stream mount also applies retail's level- and
volume-specific MIDI/SFX slot boundary and resets the all-bus fade to full scale. Authored
`next_lid`
writes now run the eight-root postorder `LEVEL_END` phase, carry process-lifetime state into a
fresh destination runtime, and restore bonus returns from the saved zone/path/progress.
Normal bonus entry retains that parent snapshot. Fresh direct bonus boot alone seeds a one-shot
same-level restart snapshot because all five bonus spawn zones are save-restricted; directly booted
bonus completion still lacks a distinct host return destination and is not claimed as a complete
round trip. A legally local controlled regression drives all three authentic Jungle Rollers Tawna
crate descriptors from their authored player `HIT 0x0300` boundary through `BoxsC` → `FruiC` →
`DispC`. Only token three performs `SaveState`, before its counter increment; the HUD then sends
completion `0x2700`, resets the master fade, sends status `0x0f00 [0x500]`, emits destination
`0x24`, and `finish_level_transition` carries the saved `0x0c` snapshot into Tawna Bonus. A
separate parsed WarpC/WillC regression covers the transition's exact proximity/status gate and its
direct `0x1600 [0]` handoff into WillC state 32. The downstream cross-stream characterization runs the
real WarpC/CardC confirmation path, observes `LoadState` on frame 301, resolves `-2` back to Jungle
Rollers, and reproduces the protected parent remount while checking Crash's transform, camera
path/progress, box count, and all 304 spawn words. These deterministic boundaries do not
substitute for one uninterrupted pad-driven route or browser full-playthrough. Every LevelUpdate
also republishes the destination zone's graphics flags in GOOL
global 30 before spawning or execution. This restores the authored `0x2000` bonus WARP branch
instead of falling through to the ordinary Title transition. WebAudio
receives mounted ADIO SFX and retail music synthesis only; the former procedural sine-wave SFX
fallback has been removed. The native 3,592-halfword encountered-object registry is retained
separately from each mount's fresh 304-word
active spawn table. Exact `LevelResetGlobals(1)` and `CardRestorePayload` ordering preserves the
active table and savestate while resetting the documented scalar globals and encounter registry.
Retail object shader modes two and three, including their source depth rejection/ramp behavior,
are live; zone-graphics flag `0x1000` substitutes the fixed Q24.8 bobbing camera and pitch for GOOL
objects only. Simulation and rendering share its exact matrix and carry its `frames_elapsed` clock
separately from texture `draw_count` across hidden frames. Mode four is also live: the simulation
advances the Lights Out/Fumbling
`dark_dist` ramp before camera work, retains its renderer-BSS words across stream remounts, and
captures the checked player reference at each object's source-order display boundary. A checked
pause-object reference is preferred when one exists. For all three modes, the native display gate
runs after that object's update and before child traversal, writes the derived colors into the live
GOOL object, and preserves the same effective colors in its render snapshot. Status-B `0x100000`
then restores the live VM colors from the object/player zone while leaving that snapshot intact;
root objects without an attached zone fall back to the current ZDAT exactly like native.
The gate also honors the main-object, display-mask `0x10000`, status-B `0x400`, near-plane/
`0x40000`, and CVTX-only `0x200` conditions. A legally local all-pair regression exercised 1,800
mode-four vertex displays and 2,880 primitives, including 540 changed color results verified in
both the render snapshots and live VM. START now runs the native gate against the
prior Crash pad snapshot, creates executable-four/subtype-four beneath root seven, publishes the
tagged pause reference, and resumes through event `0xC00` with the saved GOOL clock restored.
Paused frames continue spawn, object traversal, display latching, scene presentation and audio;
ordinary GOOL, camera/shader motion and draw-count advancement remain frozen while the exact
subtype-four/seven menu override executes. The authored pause panel is a type-five `WillT`
fragment animation, not font text: its five pieces render as `PAUSED / PUSH SELECT FOR MAP` and
follow the retail 15-frame visible/15-frame hidden blink cycle. A legally local renderer regression
and an on-cycle WebGL browser capture cover that path.
Bounds-checked type-19 PBAK parsing and browser playback restore the recorded camera/player
snapshot, spawn table, RNG, timing, bounds and full 32-bit pad words. All nine local recordings
(10,966 Crash pad boundaries) pass complete live runtime/render traces, including same-level death
restart handling, display-mask camera suppression, zone TERM/lifecycle commits, and camera save
handshakes. `PbakChoose` counts NSD type-19 names, consumes the shared RNG-B stream even for one
recording, generates the retail `pb0?B` EID, and suppresses drawing while armed; its nine legal
choices end at the characterized seed `0xaf5aad71`. Recorded absolute ticks are published from the
new current frame after Crash's pad boundary, while the start and terminal frames retain their
source wall-clock/state gates. The checked caption controller now survives
the demo restart beneath logical root one; a nonzero island-camera target dispatches its checked
event `0xE00`, while a zero target releases physical input without inventing a title transition.
Playback advances at Crash's actual root-six traversal boundary: root-one caption work runs first,
the completion event and input-lock rebind are synchronous, and Crash plus later roots observe the
new pad/controller state in that same frame. Caption objects retain their intentional null lifecycle
zone; spawned caption children consult the current camera ZDAT only for the environment/colors that
native `GoolObjectCreate` obtains through `cur_zone`.
Parsed retail objects that execute `RETURN` through their initial frame now produce the native
invalid-return lifecycle signal. Preorder traversal reclaims that subtree immediately, without a
TERM event and before display or child traversal, while retaining the source protection for the
dedicated main object outside Title. This fixes the Ending credits-object leak that previously
filled all 97 arena slots at frame 1,437; the legally local 1,800-frame regression now peaks at 82
live objects and proves returned slots are reused without a VM fault. This bounded lifecycle check
does not certify the complete ending flow.
The current strict direction/button survey runs all 43 bootable pairs for 5,400
browser-ordered simulation frames each—232,200 frames total—without a checked runtime issue. Rolling
Stones and Jaws of Darkness also pass focused 1,800-frame reproductions of the two failures above.
The `crust-sim` library has 546 passing tests; the locked workspace inventory has 885 default-active
tests plus 88 ignored-by-default legally local tests. Native
warnings-denied Clippy, optimized native release, warnings-denied Wasm Clippy, and optimized Wasm
build gates pass.
A fresh foreground Chrome pass mounted the user's legally local raw BIN through the native picker,
recognized all 88 streams and 44/44 pairs (219 MiB retained in the tab), and exercised the publisher
screen, main menu, island map, and a direct N. Sanity Beach boot. The level rendered and ran at the
reported 30.00 Hz with synthesized audio active; keyboard movement/jump, pause/resume, mute/unmute,
and fullscreen were visibly exercised. This remains bounded verification, not a full-playthrough
or retail-parity claim; gamepad, touch, card persistence, and later progression were not repeated
manually in that pass.
See [compatibility](docs/COMPATIBILITY.md) for the exact gaps and
[verification](docs/VERIFICATION.md) for checks actually performed.

## Run locally

Requirements are Node.js 20+, Rust 1.97.0 through `rustup`, and the matching `wasm-bindgen` CLI:

```bash
rustup toolchain install 1.97.0 --profile minimal --component clippy,rustfmt \
  --target wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.126 --locked
npm run dev
```

Open <http://127.0.0.1:4174>. Choose either the raw `.bin`/`.iso`, or the `.NSD` and `.NSF`
files from the disc's S0–S3 directories. The browser reads Blob ranges locally. The content
security policy blocks cross-origin connections, and no runtime API uploads asset bytes.
`npm run dev` performs a release Wasm rebuild before serving. `npm run serve` serves an existing
`dist/` only after its source and artifact fingerprints pass; it refuses stale or modified output.
The exact identity is available as `window.__crustBuild` and is used to version both generated
JavaScript and Wasm requests, so an already-cached prototype bundle cannot silently reappear. Web
builds are staged under ignored `target/` storage and replace `dist/` only after compilation,
binding generation, and source-stability checks succeed.

The 44 retail pairs are recognized. Cave (`0x04`) is mounted as a shared index/archive but is not
a boot target; the other 43 pairs are selectable. Partial stream sets containing at least one
complete pair are accepted. Each cross-level transition now validates and mounts its destination
pair on demand; a missing destination pauses the simulation with an actionable error instead of
continuing against stale data. Image-backed title entries are materialized, and retail GOOL
entry/state graphs can be validated and bound natively. Zone entities and their 304-slot spawn
flags are instantiated into a checked 96-object arena and run by the live browser at 30 Hz. This
execution slice supplies the live follow camera and camera-selected WebGL scene and is observable
through the engineering log/debug counters. Its 3D vertex-object slice is now rendered with the
camera-selected world; Crash accepts retail pad input and has a clean characterized route from
`e0_9Z` through `a0_9Z`–`b7_9Z` to the authored Level Complete transition. Save/checkpoint behavior
now includes the focused N. Sanity enemy/crate/checkpoint/death/restart regression described above,
but is not yet broadly playthrough-certified, and a
same-level load nested inside `LEVEL_END` remains a checked resumable-host boundary. A legally
local scan of all 44 retail pairs found zero authored occurrences of that nested case.

## Controls

| PlayStation input | Keyboard | Standard gamepad |
|---|---:|---:|
| D-pad | Arrow keys | D-pad / left stick |
| Cross / jump | `Z` | A / Cross |
| Square / spin | `X` | X / Square |
| Circle | `C` | B / Circle |
| Triangle | `V` | Y / Triangle |
| L1 / R1 | `A` / `S` | LB / RB |
| L2 / R2 | `Q` / `W` | LT / RT |
| L3 / R3 | `K` / `L` | Stick clicks |
| Start / Select | `Enter` / `Space` | Start / Back |

The complete pad is also available through multi-touch controls on coarse-pointer devices.
Fullscreen, pause, and mute are in the stage toolbar.

## Development

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
cargo build --release --target wasm32-unknown-unknown -p crust-web
```

`npm run build` additionally generates the unavoidable JavaScript/Wasm loader into ignored
`dist/pkg/`. All authored engine, parsing, simulation, rendering, audio, input, and persistence
logic is Rust. Static HTML/CSS and the small Wasm bootstrap are the only hand-authored web glue.

## Workspace

- `crust-formats` — endian-explicit ISO9660, raw-sector, NSD/NSF, page, entry, EID, GOOL program and
  animation descriptors, PBAK recordings, TGEO/SVTX/CVTX object models, ZDAT entity/path, stateful
  SLST visibility, scene metadata and tagged-reference validation.
- `crust-sim` — deterministic 30 Hz clock/presentation contract, checked GOOL program
  binding/word machine, hosted retail entity runtime with state rebinding, bounded object arena,
  source-ordered movement/solid physics, level/title flow, collision, camera, paging, demos, and
  exact level-global reset, encountered-object registry and retail card payload/state handshakes.
- `crust-renderer` — PSX texture/TPAG/UV decoding and cache, world and object fixed-point
  projection/lighting/culling, safe GOOL sprite/fragment/text layout and projection, ordering,
  zone shader modes, object-only fixed-camera substitution, clipping, blend passes, title
  composition and WebGL2-ready commands.
- `crust-audio` — SPU ADPCM, retail 24-voice SFX control/cache/mixer, sequence events, exact
  fixed-point sampled-voice ADSR, and a 44.1 kHz software synth.
- `crust-platform` — keyboard/gamepad/touch mapping and versioned browser persistence envelopes.
- `crust-web` — Blob-backed local imports, WebGL2/WebAudio presentation, browser storage and the
  cooperative application loop.

Every crate forbids unsafe Rust. On-disk 32-bit offsets and tags remain typed values or validated
handles; they are never reinterpreted as host pointers. See [Architecture](docs/ARCHITECTURE.md),
[migration evidence](docs/MIGRATION.md), and [compatibility](docs/COMPATIBILITY.md).

## Data and legal boundary

The repository intentionally contains no game asset, disc sector, executable, art, audio, texture,
or stream. `*.bin`, `*.iso`, `*.nsd`, `*.nsf`, local data directories, build output, fuzz corpora,
storage exports, caches, and captures are ignored. The two supported persistent records contain
only the retail 128-byte progression/options payload:

- `c1.virtual-memory-card.v1` — 15 slots containing checksummed retail-format payloads.
- `c1.browser-resume.v1` — one checksummed automatic resume record.

Selected game files are not persisted and must be selected again after reload.
