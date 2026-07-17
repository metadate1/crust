# Copyright, provenance, and distribution notice

`crust` is an independent Rust interoperability rewrite informed by observable behavior and the
public C1 lineage. It includes no Crash Bandicoot disc sectors, executables, artwork, audio,
textures, level streams, ROM images, or other proprietary game content. Runtime data is supplied
by the user, read locally by the browser, and is never uploaded by this project.

Behavioral and format references were taken from these unlicensed upstream snapshots:

- `wurlyfox/c1` at `256fdcef59f15a190290cc19db3fa9a707843b69`
- `mateusfavarin/c1` (`windows` branch) at `408d6409afadc1202230ac1183d4d7f40292b87c`
- local browser-port reference `c1-browser-runtime` at
  `7f05e5febd63e603f243c089c8b9918211c7b991`

PS1 SPU ADPCM loop/end behavior, Gaussian interpolation, and the fixed 512-word coefficient ROM were
validated against the PSX-SPX hardware notes at
<https://psx-spx.consoledev.net/soundprocessingunitspu/#spu-adpcm-samples> and
<https://psx-spx.consoledev.net/soundprocessingunitspu/#4-point-gaussian-interpolation>. The PSX-SPX
repository itself states that it has no acquired license and is not a clean-room work; this project
therefore makes no clean-room claim for that hardware reference. Continuous predictor history over
repeat-address jumps and immediate End+Mute behavior were independently cross-checked, without
copying implementation code, against DuckStation at
`e39033c4480cfbb9106e32beb844b0649ad9c2db` (`src/core/spu.cpp`) and the MiSTer PSX FPGA core at
`bcaa0bbc5b1b68a8e9c2c376251c7ce63a53c0a2` (`rtl/spu.vhd`). The Gaussian formula and coefficient
sequence were independently cross-checked against `howprice/hopstation` at
`974653fe77e30493e7dc6043cccdbaa69820175c`, `psx/SPU.cpp`, which is distributed under the MIT
License:

> MIT License
>
> Copyright (c) 2026 howprice
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
> associated documentation files (the "Software"), to deal in the Software without restriction,
> including without limitation the rights to use, copy, modify, merge, publish, distribute,
> sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
> furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or
> substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
> NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
> NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
> DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT
> OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

Sony SEQ NRPN loop behavior was validated against the public Net Yaroze User Guide's sound-service
description at <https://www.psxdev.net/downloads/Net%20Yaroze%20Official%20-%20Startup%20Guide.pdf>
and independently cross-checked against `ps2dev/ps2sdk` at
`b1adc2eab736d5717e8c53bd6d4a67cab20fd1d5`, whose libsnd2 implementation is distributed under the
Academic Free License 2.0. No source from either reference is included in this repository.

Those C1 repositories do not provide an express root license. No C source is included or linked
into this runtime. This repository must remain private and must not be redistributed, hosted, or
published as a playable build until contributor permissions and the original-game rights have
been reviewed by qualified counsel. Default copyright remains with the respective authors and
rights holders.

The rewrite's CSS/browser shell is original to this repository and contains no copied game imagery.
The two committed decorative shell images were generated specifically for this private project on
2026-07-16 with OpenAI's image-generation service from text-only prompts: an original
jungle/wood/stone game-window surround (`web/assets/crust-game-frame.png`, SHA-256
`f065de37fe957794b7f477b1e339adeefa5e41851f32dfe39f7971091a594261`) and an original
`CRUST` wordmark (`web/assets/crust-wordmark.png`, SHA-256
`220068c73614f4cc55dba334defa810dbd312e5831d1d8bc5f3f85220d44ce5c`). No retail screenshot,
disc asset, extracted texture, or third-party logo file was supplied as an image reference. These
original shell assets do not change the repository's restricted-distribution status or imply
endorsement by the game's rights holders.

The former procedural diagnostic geometry is no longer part of the runtime. Retail loading/title
images, worlds and textures are decoded transiently from the user's selected local data and are
never committed. Generated `wasm-bindgen` loader code is build output and is not committed.
