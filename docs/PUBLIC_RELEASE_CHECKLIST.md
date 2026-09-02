# Public source-release checklist

Use this checklist before changing the GitHub repository from private to public.

This checklist covers source publication only. It does not approve an open-source license, hosted
game, compiled release, app package, or commercial product.

## 1. Confirm the publication model

- [ ] Describe CRUST as **source-available research software**, not open source.
- [ ] Keep [LICENSE.md](../LICENSE.md) all rights reserved.
- [ ] Publish source and documentation only.
- [ ] Do not publish `dist/`, `target/`, local game data, screenshots, videos, replays, profiles,
  or save files.
- [ ] Keep the user-supplied-file model: users select their own supported game data locally.
- [ ] Do not provide or link to a game disc image, BIOS, extracted streams, or other game data.

## 2. Review the unresolved rights risk

- [ ] Read [RIGHTS_AND_LICENSES.md](../RIGHTS_AND_LICENSES.md).
- [ ] Confirm that [NOTICE.md](../NOTICE.md) identifies every known source and technical reference.
- [ ] Record who made the decision to publish despite any unresolved upstream or game-rights risk.
- [ ] Get advice from a qualified lawyer if the project owner wants a legal opinion. Repository
  checks cannot provide one.

Do not replace the all-rights-reserved notice with an open-source license unless the project has
authority to license every covered contribution.

## 3. Audit files and Git history

Run the audit against every reachable remote branch and tag:

```bash
git fetch --prune origin
bash scripts/check-public-release.sh --remote origin
```

Then confirm:

- [ ] The audit exits with status zero.
- [ ] The only tracked binary media are the four original CRUST images documented in
  `artwork/PROVENANCE.md`.
- [ ] No reachable commit contains a disc image, ROM, BIOS, executable, extracted stream, save,
  replay, screenshot, recording, archive, secret, or large unexpected blob.
- [ ] `git status --ignored` places all local data and generated output under ignored paths.
- [ ] Every remote branch and tag is intentional. Remove obsolete remote refs before publication
  if they add risk and no public value.

## 4. Run the release checks

```bash
npm run fmt
npm run lint
npm run lint:wasm
npm run lint:wasm:browser-harness
npm test
npm run build:wasm
npm run build:wasm:browser-harness
cargo build --workspace --release --locked
npm run build
npm run verify:dist
npm run build:browser-harness
npm run verify:browser-harness
git diff --check
```

- [ ] Every command exits with status zero.
- [ ] Generated build output remains ignored and uncommitted.
- [ ] Public documentation describes the current result and does not turn an older test into a
  present-tense claim.

## 5. Configure GitHub

- [ ] Keep `main` as the default branch.
- [ ] Require the CI workflow before merge.
- [ ] Enable secret scanning and push protection when available.
- [ ] Enable private vulnerability reporting and link to [SECURITY.md](../SECURITY.md).
- [ ] Keep GitHub Pages and release-package automation disabled until hosted or binary
  distribution is reviewed separately.
- [ ] Use **source-available research code** in the repository description.
- [ ] Do not select an open-source license in GitHub settings.
- [ ] Point contribution requests to [CONTRIBUTING.md](../CONTRIBUTING.md).

## 6. Check the public repository

After visibility changes:

- [ ] Inspect the public file list and downloadable source archive.
- [ ] Clone the public repository into a new directory.
- [ ] Run `bash scripts/check-public-release.sh --remote origin` from that fresh clone.
- [ ] Confirm that GitHub does not label the project MIT, Apache, GPL, or another open-source
  license.
- [ ] Confirm that the README links to the license, rights, notices, privacy, security, and
  contribution documents.

Treat a credible rights complaint or takedown request as a release issue. Preserve the relevant
records and handle it privately; do not debate ownership in a public issue thread.

## Later: changing the distribution model

A public playable site, compiled release, package, or open-source license is a new decision. Before
making that change:

- identify the new files and data flows;
- review upstream, game, dependency, trademark, and privacy obligations;
- update the rights, privacy, security, and contribution documents; and
- repeat the history and release audits.
