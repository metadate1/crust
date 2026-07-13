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

Those C1 repositories do not provide an express root license. No C source is included or linked
into this runtime. This repository must remain private and must not be redistributed, hosted, or
published as a playable build until contributor permissions and the original-game rights have
been reviewed by qualified counsel. Default copyright remains with the respective authors and
rights holders.

The rewrite's CSS shell and procedural low-poly diagnostic WebGL geometry are original to this
repository and contain no copied game imagery. Retail loading images are decoded transiently from
the user's selected local data and are never committed. Generated `wasm-bindgen` loader code is
build output and is not committed.
