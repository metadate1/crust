# Third-party dependency notices

CRUST does not vendor its Rust dependencies. Cargo fetches the exact versions pinned in
`Cargo.lock` from their publishers. The table below records the license expressions reported by
`cargo metadata --locked` on 2026-09-02; the upstream license files and copyright notices remain
authoritative.

There are no npm runtime or development dependencies in `package.json`.

| Package | Locked version | Declared license expression |
| --- | --- | --- |
| `autocfg` | `1.5.1` | `Apache-2.0 OR MIT` |
| `base64` | `0.22.1` | `MIT OR Apache-2.0` |
| `bit-set` | `0.8.0` | `Apache-2.0 OR MIT` |
| `bit-vec` | `0.8.0` | `Apache-2.0 OR MIT` |
| `bitflags` | `2.13.0` | `MIT OR Apache-2.0` |
| `bumpalo` | `3.20.3` | `MIT OR Apache-2.0` |
| `cfg-if` | `1.0.4` | `MIT OR Apache-2.0` |
| `errno` | `0.3.14` | `MIT OR Apache-2.0` |
| `fastrand` | `2.4.1` | `Apache-2.0 OR MIT` |
| `fnv` | `1.0.7` | `Apache-2.0 / MIT` |
| `futures-core` | `0.3.32` | `MIT OR Apache-2.0` |
| `futures-task` | `0.3.32` | `MIT OR Apache-2.0` |
| `futures-util` | `0.3.32` | `MIT OR Apache-2.0` |
| `getrandom` | `0.3.4` | `MIT OR Apache-2.0` |
| `getrandom` | `0.4.3` | `MIT OR Apache-2.0` |
| `itoa` | `1.0.18` | `MIT OR Apache-2.0` |
| `js-sys` | `0.3.103` | `MIT OR Apache-2.0` |
| `libc` | `0.2.186` | `MIT OR Apache-2.0` |
| `linux-raw-sys` | `0.12.1` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| `memchr` | `2.8.3` | `Unlicense OR MIT` |
| `num-traits` | `0.2.19` | `MIT OR Apache-2.0` |
| `once_cell` | `1.21.4` | `MIT OR Apache-2.0` |
| `pin-project-lite` | `0.2.17` | `Apache-2.0 OR MIT` |
| `ppv-lite86` | `0.2.21` | `MIT OR Apache-2.0` |
| `proc-macro2` | `1.0.106` | `MIT OR Apache-2.0` |
| `proptest` | `1.11.0` | `MIT OR Apache-2.0` |
| `quick-error` | `1.2.3` | `MIT/Apache-2.0` |
| `quote` | `1.0.46` | `MIT OR Apache-2.0` |
| `r-efi` | `5.3.0` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` |
| `r-efi` | `6.0.0` | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` |
| `rand` | `0.9.5` | `MIT OR Apache-2.0` |
| `rand_chacha` | `0.9.0` | `MIT OR Apache-2.0` |
| `rand_core` | `0.9.5` | `MIT OR Apache-2.0` |
| `rand_xorshift` | `0.4.0` | `MIT OR Apache-2.0` |
| `regex-syntax` | `0.8.11` | `MIT OR Apache-2.0` |
| `rustix` | `1.1.4` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| `rustversion` | `1.0.23` | `MIT OR Apache-2.0` |
| `rusty-fork` | `0.3.1` | `MIT/Apache-2.0` |
| `serde` | `1.0.228` | `MIT OR Apache-2.0` |
| `serde_core` | `1.0.228` | `MIT OR Apache-2.0` |
| `serde_derive` | `1.0.228` | `MIT OR Apache-2.0` |
| `serde_json` | `1.0.150` | `MIT OR Apache-2.0` |
| `slab` | `0.4.12` | `MIT` |
| `syn` | `2.0.118` | `MIT OR Apache-2.0` |
| `tempfile` | `3.27.0` | `MIT OR Apache-2.0` |
| `unarray` | `0.1.4` | `MIT OR Apache-2.0` |
| `unicode-ident` | `1.0.24` | `(MIT OR Apache-2.0) AND Unicode-3.0` |
| `wait-timeout` | `0.2.1` | `MIT/Apache-2.0` |
| `wasip2` | `1.0.4+wasi-0.2.12` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| `wasm-bindgen` | `0.2.126` | `MIT OR Apache-2.0` |
| `wasm-bindgen-futures` | `0.4.76` | `MIT OR Apache-2.0` |
| `wasm-bindgen-macro` | `0.2.126` | `MIT OR Apache-2.0` |
| `wasm-bindgen-macro-support` | `0.2.126` | `MIT OR Apache-2.0` |
| `wasm-bindgen-shared` | `0.2.126` | `MIT OR Apache-2.0` |
| `web-sys` | `0.3.103` | `MIT OR Apache-2.0` |
| `windows-link` | `0.2.1` | `MIT OR Apache-2.0` |
| `windows-sys` | `0.61.2` | `MIT OR Apache-2.0` |
| `wit-bindgen` | `0.57.1` | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` |
| `zerocopy` | `0.8.54` | `BSD-2-Clause OR Apache-2.0 OR MIT` |
| `zerocopy-derive` | `0.8.54` | `BSD-2-Clause OR Apache-2.0 OR MIT` |
| `zmij` | `1.0.21` | `MIT` |

This inventory does not relicense any dependency. Before distributing a compiled binary or Wasm
bundle, collect and ship the license texts and notices required by the selected license option for
every linked dependency. The current public-source plan does not include binary releases.

Technical materials consulted during implementation, but not included as package dependencies,
are separately attributed in [NOTICE.md](NOTICE.md).
