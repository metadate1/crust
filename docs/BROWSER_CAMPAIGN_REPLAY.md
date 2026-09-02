# Local browser campaign replay discovery and composition

These tools join locally generated replay fragments into the format used by
`scripts/browser-harness-smoke.mjs`. They order, copy, and validate captured input. They do not
play the game, invent input, or change game state:

- discovery orders an unordered directory of opt-in local fragment JSON files by exact state;
- composition reads only the resulting ordered local manifest and its fragment JSON files;
- composition copies fragment input runs without generating, predicting, or changing pad input;
- it inserts only handoff fragments named by the manifest;
- it adds a current/mounted-LID guard to every segment;
- it requires exact deterministic state continuity across every phase; and
- it has no operation for changing GOOL state, forcing a transition, or mounting a chosen
  destination.

No campaign manifest, generated replay, PBAK-derived pad word, game stream, screenshot, or browser
profile belongs in Git. Put them under `target/`, `local-data/`, `artifacts/`, `captures/`, or
`recordings/`, all of which are local artifact boundaries. The CLI refuses a repository-local
output anywhere else and also refuses to overwrite its manifest or a fragment.

## Discover, compose, and run

Export one campaign route into its own ignored/local fragment directory. Validate that every
export in the directory belongs to one unique exact path without writing a manifest:

```bash
npm run discover:browser-campaign-replay -- \
  --fragments target/local-campaign/fragments \
  --check
```

Write the ordered manifest, optionally choosing the captured input profile where browser tracing
should begin:

```bash
npm run discover:browser-campaign-replay -- \
  --fragments target/local-campaign/fragments \
  --trace-input-profile jungle-rollers-completion-route \
  --output target/local-campaign/manifest.json
```

Discovery reads only exporter-named `lid-*-draw-*-*-to-*.json` files. It requires a path beginning
with the browser's zeroed physical pad and matches every adjacent checkpoint, progression
snapshot, and post-mount five-word pad history exactly. It fails if any capture is disconnected or
if equally long exact orderings are ambiguous. Keep alternate branches in separate directories;
the tool will not choose a preferred branch, skip a capture, fill a missing edge, or emit
controller segments. Fragment references in the manifest are relative to the manifest output.

Both the fragment directory and output must be outside the repository or beneath an ignored local
artifact root. Use `--force` only to replace an existing ignored manifest intentionally. The
discovery CLI will not overwrite one of its input fragments.

Validate an ignored manifest without producing another file:

```bash
npm run compose:browser-campaign-replay -- \
  --manifest target/local-campaign/manifest.json \
  --check
```

Compose it and pass the result to the isolated browser harness:

```bash
npm run compose:browser-campaign-replay -- \
  --manifest target/local-campaign/manifest.json \
  --output target/local-campaign/campaign.replay.json

npm run verify:browser-harness:smoke -- \
  --asset /path/to/owned-disc.bin \
  --replay target/local-campaign/campaign.replay.json
```

Use the composer's `--force` only to replace an existing ignored replay intentionally. The output
has `localDiagnosticOnly: true` and `canonicalCampaign: false`; it is local evidence from one
exact data/runtime phase, not a distributable controller oracle.

## Manifest

The manifest is schema 1 and explicitly opts into local diagnostic handling:

```jsonc
{
  "schema": 1,
  "localDiagnosticOnly": true,
  "canonicalCampaign": false,
  "bootLid": 1,
  "unlockAll": false,
  "traceFromPhase": "island-map-a-to-b",
  "settleFrames": 0,
  "phases": [
    {
      "id": "stage-a",
      "fragment": "./fragments/stage-a.json",
      "entry": { "/* exact checkpoint */": "see below" },
      "exit": { "/* exact checkpoint */": "see below" }
    },
    {
      "id": "stage-b",
      "fragment": "./fragments/stage-b.json",
      "entry": { "/* exact checkpoint */": "must equal the preceding handoff exit" },
      "exit": { "/* exact checkpoint */": "see below" }
    }
  ],
  "titleMapHandoffs": [
    {
      "kind": "title-map",
      "after": "stage-a",
      "before": "stage-b",
      "phases": [
        {
          "id": "stage-a-complete",
          "fragment": "./fragments/stage-a-complete.json",
          "entry": { "/* exact checkpoint */": "must equal stage-a.exit" },
          "exit": { "/* exact checkpoint */": "Title / Island Map mounted" }
        },
        {
          "id": "island-map-a-to-b",
          "fragment": "./fragments/island-map-a-to-b.json",
          "entry": { "/* exact checkpoint */": "Title / Island Map mounted" },
          "exit": { "/* exact checkpoint */": "must equal stage-b.entry" }
        }
      ]
    }
  ]
}
```

The comments above make the shape readable but are not literal JSON. Each checkpoint must contain
all of these exact integer fields:

```json
{
  "currentLid": 25,
  "mountedLid": 25,
  "retailDrawCount": 100,
  "retailProcessDrawCount": 100,
  "retailRandomSeed": 305419896,
  "retailRandomSeedB": 2271560481,
  "retailHardRestarts": 0,
  "retailLoadStates": 0,
  "retailDeathCameraFrames": 0,
  "titleState": 15
}
```

`currentLid` and `mountedLid` must be equal because a phase boundary describes a completed mount,
not an asynchronous request in flight. Every field of one phase's `exit` must exactly equal the
following phase's `entry`; missing fields are rejected rather than treated as wildcards. `bootLid`
must equal the first entry LID.

The recovery counters describe source calls, not only successful same-stream commits.
`retailLoadStates` counts GOOL `LoadState` effects. `retailHardRestarts` counts every resulting
`LevelRestart` call, including the different-level call that selects a saved parent and the
protected same-level call performed after that parent is mounted. Consequently a bonus return adds
one LoadState and two hard-restart calls; any earlier death/checkpoint recovery remains in both
cumulative session totals.

Base phases remain in the manifest's listed order. A `title-map` handoff names two adjacent base
phases and supplies one or more ordinary authored fragments to run between them. At least one of
those phases must enter the retail Title / Island Map stream (`0x19`). A normal level can therefore
provide its Level Complete fragment followed by its Island Map selection fragment, while a boss
that transitions directly to the map needs only the latter. The composer inserts those phases; it
does not synthesize their inputs or skip either mount.

`traceFromPhase` is optional and becomes the browser harness's 1-based `traceFromSegment`.
`settleFrames` is the final replay settle budget. A phase may specify its own `settleFrames` to
override the fragment's root settle budget at that boundary.

Standalone exported fragments require at least one destination GOOL execution after a cross-LID
transition. The composer removes that generic smoke-test requirement at campaign handoffs so the
next phase owns destination frame 1. The manifest's exact exit checkpoint remains authoritative:
if its draw/RNG/counter fields describe a later destination frame, settlement advances to that
frame; if they describe the completed mount at frame zero, no following-phase input is consumed.
This prevents a map handoff from shifting a frame-exact boss route while retaining fail-closed
continuity checks.

Settlement is a bounded number of ordinary cooperative destination frames, not wall-clock waiting
and not an engine timing correction. It is therefore allowed to advance the newly mounted stream.
For example, at the earlier captured-browser checkpoint, the publisher-first Jungle Rollers
completion reached its exact level-end request on route frame 3,387, then needed 119 zero-input
Level Complete frames before the stable destination checkpoint. Those 119 frames belonged to that
harness settlement budget; the current native controller reaches its level-end request on frame
3,384. A following phase's
`while` guard prevents stale source-phase input from leaking after an already-advanced destination
transitions. The runner reports per-trace `settleFramesUsed`, total `segmentSettleFramesUsed`, and
`skippedReplayFrames`, so an overlapped local fragment is visible and can be regenerated or
trimmed at the exact post-settlement checkpoint instead of being misreported as browser/native
simulation lag.

## Fragment contract

Each referenced document must be a schema-1 run-length replay emitted to a local directory by the
opt-in survey export path. In addition to ordinary `bootLid`, `segments`, `expect`, and
`settleFrames`, the composer checks the export metadata:

- `localDiagnosticOnly` is exactly `true`;
- `canonicalCampaign` is exactly `false`;
- `level` and `bootLid` equal the phase entry LID;
- `initialDrawCount` equals the phase's exact entry draw count;
- `frames` equals the sum of segment frame counts;
- `transition.frame`, when present, equals `frames`;
- `transition.lid` and the fragment's root LID expectations agree with the phase exit; and
- `unlockAll` agrees with the manifest.

Every publisher/title, authored island-map, ordinary gameplay, and Level Complete survey fragment
emits exact observer metadata:

- `entryCheckpoint` and `exitCheckpoint` contain the same complete checkpoint shape used by the
  manifest, including `titleState`;
- `entryProgression` and `exitProgression` contain `gameState`, `titleState`,
  `savedTitleState`, `currentMapLevel`, `levelCount`, `levelsUnlocked`, and
  `islandCameraState`. Current exports also include the complete inventory group
  `gemCount`, `keyCount`, `itemPool1`, and `itemPool2`. The composer remains compatible with
  older schema-1 captures that omit that entire group; it rejects a partial group, and compares
  or emits browser inventory expectations only where the source capture actually recorded it;
- `initialPad` and `finalPad` contain `tapped`, `held`, `tappedPrevious`, `heldPrevious`, and
  `heldPrevious2`; and
- every emitted segment explicitly has `"inputKind": "physical"` or `"recorded"`.

The checked composer requires all six metadata objects. Checkpoints must equal the manifest
exactly, progression must be continuous between adjacent captured fragments, and replaying every
ordered run from `initialPad` must reproduce `finalPad`. Across a cross-LID phase, the composer
applies the real `CoreObjectsCreate` pad update before comparing the next fragment's initial
history. The first captured phase must begin with the browser's zeroed boot pad. This catches a
missing mount-held sample, a fabricated tap, a dropped gameplay/Level Complete pad history, or a
legacy fragment whose only metadata was its draw count.

Survey export refuses a session-carried phase unless its caller supplies the same
`PersistentPadState` that crossed the preceding mount. This is intentional: silently restarting
the five-word pad history at zero would create a replay that cannot be the browser campaign.

Set `C1_BROWSER_REPLAY_EXPORT` to an ignored local directory while running a legally-local
characterization test to opt in:

```bash
C1_STREAM_DIR=/path/to/local/streams \
C1_BROWSER_REPLAY_EXPORT=target/local-campaign/fragments \
cargo test -p crust-sim --test local_retail_idle_survey \
  exported_publisher_opening_composes_through_jungle_mount \
  -- --ignored --exact
```

That focused legally-local test exports and checks the complete Publisher Title → N. Sanity Beach
→ Level Complete → Title / Island Map → Jungle Rollers mount chain. The Rust exporter refuses a
repository-local destination unless it is under `target/`, `local-data/`, `artifacts/`,
`captures/`, or `recordings/`. It observes harness frames and the authored transition, then builds
an isolated destination session-import snapshot solely for boundary metadata. It does not mutate
the source runtime, request a transition, load destination assets, spawn pair objects, or execute
a destination frame.

The discovery CLI exposes `discoverLongestCampaignManifest` for those local capture documents in
strict mode: all exporter-named files must belong to one unambiguous path. The underlying ordering
still begins with the browser's zeroed physical pad and matches checkpoint, progression, and
post-mount pad history exactly. It never fills a missing edge or generates an input run. Tests
cover unordered input, hybrid PBAK/physical fragments, ambiguous graphs, disconnected captures,
and one-word checkpoint and pad-history mismatches.

The current completed legally local browser campaign proof starts with the real publisher/title
sequence and follows the complete ordinary main-map route through every gameplay, boss,
completion, and map phase, including its Great Gate/Bonus 2 round trip, Dr. Neo Cortex, Ending,
and the authored return to Title. Its 89 fresh
exporter fragments form one unique exact strict-discovery path with no disconnected capture,
ambiguous branch, or synthetic handoff. The composed route contains 19,961 input segments and
146,501 replay frames; Chrome executes two additional declared transition-settle frames and skips
zero replay frames.

The current terminal snapshot is current/mounted Title LID `0x19`, draw/process count 134,970,
RNG-A `0xec6e8edc`, and RNG-B `0x369bbacd`. The harness reports three source hard-restart calls and
two LoadState effects: one same-level load/restart from Jungle Rollers' authored terminal fall,
then the different-stream load/restart and protected parent restart from The Great Gate's Tawna-
bonus return. It records zero death-camera frames and no checkpoint, runtime/GOOL/zone/spawn,
console, network, or WebGL failure. A per-source-frame, silent 30 fps H.264 recording and 89-entry
chapter list preserve this exact run under ignored artifacts. The earlier 141,776-frame/96-settle
campaign remains historical comparison evidence for its captured ordinary route. A separate
nine-phase, 9,477-frame exact branch still proves The Great Gate's physical Tawna path, Bonus 2,
its `-2` save-state return, and the remounted Great Gate checkpoint. A separate direct-boot replay
completes Stormy Ascent's 9,334 captured frames and
mounts Level Complete at exact draw/RNG state with no recovery or skipped frame. The natural
Sunset/Whole Hog and Jaws/Fumbling key branches each pass independently from fresh browser storage;
joining both in the same empty-card campaign, alternate completion, and other explicitly documented
edge routes remain separate parity gates.

Every composed gameplay, completion, and island-map segment receives:

```json
"while": { "currentLid": 25, "mountedLid": 25 }
```

using that phase's actual entry LID. If an authored transition occurs before a run ends, the
harness stops at the mount request and skips the remainder after the destination mount instead of
leaking old-level input into the new stream. The final segment of every phase receives the exact
exit expectation and its bounded settle budget. Those boundary expectations include the exact gem
count, key count, and both retail item-pool words in addition to the mount, draw, RNG-A, recovery,
and title-state checkpoint. A transition that is missing, late, points to the wrong LID, loses
inventory, or reaches the right LID with the wrong draw/RNG/counter state therefore fails closed.

`retailRandomSeedB` remains exact native capture metadata: discovery and composition reject a
fragment whose entry or exit RNG-B word breaks the native chain. The composed browser replay lists
that field under `composition.browserObservedOnlyCheckpointFields` and does not compare it at a
browser checkpoint. Native route capture has no audio host, while the browser's `RetailAudioEngine`
legitimately advances the shared RNG-B word when it allocates SFX voices. The harness still reports
the browser word for diagnosis; treating the native no-audio value as a browser gameplay invariant
would turn correct WebAudio activity into a false campaign failure.

Publisher/title captures are the deliberate exception: their segments omit the entry-LID guard
because the authored opening temporarily mounts Intro before returning to Title. Their final exact
checkpoint still requires the expected destination mount, so this does not synthesize or skip a
phase boundary.

Fragments default to 16-bit `"physical"` input. Export profiles containing complete 32-bit words
reconstructed from the user's own PBAK annotate their segments as `"inputKind": "recorded"`; a
local manifest phase may still supply that classification for older unannotated diagnostic input.
The complete five-word pad history supports 32-bit recorded words, while physical segments remain
strictly 16-bit. This opt-in does not make the generated replay distributable; the fragment,
manifest, and output must remain ignored and local.

Physical fragments must also describe input that a live pad can produce. The composer rejects
Up+Down or Left+Right in both `held` and `settleHeld`, because the browser's physical-input path
resolves those impossible pairs before the game sees them. Mark an exact locally reconstructed
PBAK word as `"recorded"` instead; otherwise native characterization and browser playback would
silently execute different pad states.

## What the evidence means

A successful composition proves that the declared fragment metadata forms one exact static chain.
A successful browser-harness run is the stronger evidence: it proves the real Wasm runtime
actually executed every supplied pad segment, acknowledged every authored remount, and reached
every exact checkpoint without runtime, GOOL, WebGL, console, or network failure.

The harness retains cumulative evidence that GOOL executed during the campaign. A replay may end
at a valid asynchronous destination mount whose final snapshot is frame zero and has no per-frame
executions yet; that final snapshot no longer erases execution evidence from the levels that led
to it.

Neither result alone proves an unrepresented campaign branch. Bonus returns, deaths/checkpoint
reloads, secret paths, password/card flows, and alternate endings need their own local phases and
browser executions before they can be claimed.
