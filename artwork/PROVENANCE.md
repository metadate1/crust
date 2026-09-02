# Original interface artwork provenance

The interface artwork in this directory and `web/assets/` was generated specifically for
Crust. No Crash Bandicoot game data, extracted texture, screenshot, logo file, character art,
or other proprietary visual input was supplied to the generator. These files do not change the
project's all-rights-reserved, source-available status described in `LICENSE.md` and
`RIGHTS_AND_LICENSES.md`.

## `crust-wordmark`

- Generated: 2026-07-16
- Generator: OpenAI ImageGen through Codex's built-in image generation tool
- Generated source: `artwork/source/crust-wordmark-chroma.png`
- Browser asset: `web/assets/crust-wordmark.png`
- Post-processing: the repository-independent ImageGen chroma-key helper removed the sampled
  green border with a soft matte, thresholds 12/220, and despill. The source remains alongside
  the production asset so the extraction is reproducible.
- Validation: 1921×819 RGBA; alpha bounds `(122, 155, 1805, 679)`; all four corner alpha values
  are zero.

Exact prompt:

```text
Use case: logo-brand
Asset type: original game-runtime wordmark production asset
Primary request: create an original energetic wordmark for the browser game runtime named CRUST, evoking the broad exuberant mood of a colorful mid-1990s jungle platform adventure without copying any existing game logo, character, mask, letterform, trademark, or publisher mark
Style/medium: highly polished hand-painted vector-like game logo, chunky irregular carved-stone letterforms with unique silhouettes, playful uneven baseline, crisp readable typography, production-ready isolated graphic
Text (verbatim): "CRUST"
Spelling invariant: render exactly five letters, C-R-U-S-T, exactly once, with no other words, initials, numbers, or symbols
Composition/framing: single centered horizontal wordmark, front-facing, generous empty padding on all sides, no mockup and no scene
Color palette: warm mango orange and sunlit yellow faces, deep indigo and teal edge accents, subtle dark earthen outline; do not use the chroma-key green anywhere in the wordmark
Scene/backdrop: perfectly flat solid #00ff00 chroma-key background for local removal
Lighting/mood: lively tropical adventure energy, bold and joyful, readable at small browser-header size
Constraints: original design only; no copyrighted characters; no anthropomorphic mascot; no feathers; no tribal mask; no exact resemblance to any existing logo; background must be one uniform #00ff00 color with no shadows, gradients, texture, reflections, floor plane, or lighting variation; crisp silhouette; generous padding; no halos or fringing; no watermark
```

## `crust-game-frame`

- Generated: 2026-07-16
- Generator: OpenAI ImageGen through Codex's built-in image generation tool
- Generated source: `artwork/source/crust-game-frame-chroma.png`
- Browser asset: `web/assets/crust-game-frame.png`
- Post-processing: the same chroma-key helper removed the sampled green border and center with a
  soft matte, thresholds 12/220, and despill.
- Validation: 1672×941 RGBA; alpha bounds `(92, 35, 1571, 909)`; the four corners and exact center
  are fully transparent.

Exact prompt:

```text
Use case: stylized-concept
Asset type: original game UI window-frame overlay
Primary request: create a single production-ready decorative bezel that surrounds a central 4:3 browser game display, with the broad joyful energy of a colorful mid-1990s tropical platform adventure while remaining an entirely original design
Subject: one continuous irregular frame made from chunky sun-warmed carved stone and dark jungle wood, wrapped with broad tropical leaves, curling vines, small purple flowers, and two original abstract guardian-mask carvings integrated into the left and right sides; the guardian carvings should use simple geometric eyes and spiral grooves, have no feathers or character likeness, and not resemble any existing game mask
Style/medium: polished hand-painted game UI asset, playful low-poly 1990s console-CGI surface language, crisp silhouette, bold readable forms, rich material texture without photorealism
Composition/framing: landscape 16:9 asset canvas; centered rectangular frame; a completely unobstructed 4:3 rectangular opening in the middle occupying about 72 percent of the asset width and 76 percent of its height; no leaves, vines, shadows, protrusions, or decoration may cross into the central opening; generous clear space outside the frame; front-facing with no perspective tilt
Color palette: deep forest and teal leaves, warm orange sandstone, dark cocoa wood, indigo and violet accents, small golden highlights; never use chroma-key green within the frame artwork
Scene/backdrop: the entire central opening and all exterior space around the frame must be the same perfectly flat solid #00ff00 chroma-key color for local transparency removal
Lighting/mood: warm tropical daylight on the frame itself, inviting and adventurous, lively but not visually cluttered
Constraints: original design only; no text; no logo; no trademark; no copyrighted character; no mascot; no fruit icons; no crates; no feathers; no skulls; no exact resemblance to any existing mask or game UI; chroma-key areas must be uniform #00ff00 with no gradient, texture, reflections, floor plane, lighting variation, or cast shadow; crisp isolated silhouette; no halo or fringing; no watermark
```
