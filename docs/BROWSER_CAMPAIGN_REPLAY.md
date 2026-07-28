# Local browser campaign replay composition

The browser campaign composer joins legally local replay fragments into the schema consumed by
`scripts/browser-harness-smoke.mjs`. It is deliberately a composition and validation tool, not a
second game controller:

- it reads only an ordered local manifest and opt-in local fragment JSON files;
- it copies fragment input runs without generating, predicting, or changing pad input;
- it inserts only handoff fragments named by the manifest;
- it adds a current/mounted-LID guard to every segment;
- it requires exact deterministic state continuity across every phase; and
- it has no operation for changing GOOL state, forcing a transition, or mounting a chosen
  destination.

No campaign manifest, generated replay, PBAK-derived pad word, game stream, screenshot, or browser
profile belongs in Git. Put them under `target/`, `local-data/`, `artifacts/`, `captures/`, or
`recordings/`, all of which are local artifact boundaries. The CLI refuses a repository-local
output anywhere else and also refuses to overwrite its manifest or a fragment.

## Compose and run

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

Use `--force` only to replace an existing ignored replay intentionally. The output has
`localDiagnosticOnly: true` and `canonicalCampaign: false`; it is local evidence from one exact
data/runtime phase, not a distributable controller oracle.

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
  "retailDeathCameraFrames": 0
}
```

`titleState` may also be pinned. `currentLid` and `mountedLid` must be equal because a phase
boundary describes a completed mount, not an asynchronous request in flight. Every field of one
phase's `exit` must exactly equal the following phase's `entry`; missing fields are rejected rather
than treated as wildcards. `bootLid` must equal the first entry LID.

Base phases remain in the manifest's listed order. A `title-map` handoff names two adjacent base
phases and supplies one or more ordinary authored fragments to run between them. At least one of
those phases must enter the retail Title / Island Map stream (`0x19`). A normal level can therefore
provide its Level Complete fragment followed by its Island Map selection fragment, while a boss
that transitions directly to the map needs only the latter. The composer inserts those phases; it
does not synthesize their inputs or skip either mount.

`traceFromPhase` is optional and becomes the browser harness's 1-based `traceFromSegment`.
`settleFrames` is the final replay settle budget. A phase may specify its own `settleFrames` to
override the fragment's root settle budget at that boundary.

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

Every composed segment receives:

```json
"while": { "currentLid": 25, "mountedLid": 25 }
```

using that phase's actual entry LID. If an authored transition occurs before a run ends, the
harness stops at the mount request and skips the remainder after the destination mount instead of
leaking old-level input into the new stream. The final segment of every phase receives the exact
exit expectation and its bounded settle budget. A transition that is missing, late, points to the
wrong LID, or reaches the right LID with the wrong draw/RNG/counter state therefore fails closed.

Fragments default to 16-bit `"physical"` input. A local fragment containing complete 32-bit words
reconstructed from the user's own PBAK may explicitly annotate its segments with
`"inputKind": "recorded"`, or its local manifest phase may set `"inputKind": "recorded"` as a
default for unannotated segments. This opt-in does not make the generated replay distributable;
the fragment, manifest, and output must remain ignored and local.

## What the evidence means

A successful composition proves that the declared fragment metadata forms one exact static chain.
A successful browser-harness run is the stronger evidence: it proves the real Wasm runtime
actually executed every supplied pad segment, acknowledged every authored remount, and reached
every exact checkpoint without runtime, GOOL, WebGL, console, or network failure.

Neither result alone proves an unrepresented campaign branch. Bonus returns, deaths/checkpoint
reloads, secret paths, password/card flows, and alternate endings need their own local phases and
browser executions before they can be claimed.
