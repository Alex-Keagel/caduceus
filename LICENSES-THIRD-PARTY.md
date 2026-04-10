# Third-Party Licenses

This file lists the licenses of all third-party dependencies used by Caduceus.
Generated via `cargo license` from the workspace crates.

## License Summary

| License | Count | Notable Packages |
|---------|-------|-----------------|
| Apache-2.0 OR MIT | 328 | serde, tokio, reqwest, syn, anyhow, thiserror, chrono, futures, regex, uuid, and most ecosystem crates |
| MIT | 149 | tokio-util, hyper, rusqlite, tracing, bytes, darling, schemars, plist, and GTK/WebKit bindings |
| Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT | 15 | rustix, linux-raw-sys, wasm-encoder, wasmparser, wit-bindgen |
| Unicode-3.0 | 18 | icu_collections, icu_locale_core, icu_normalizer, icu_properties, zerovec |
| MIT OR Unlicense | 8 | aho-corasick, memchr, walkdir, globset, byteorder |
| MPL-2.0 | 7 | cssparser, selectors, dtoa-short, option-ext |
| Apache-2.0 OR MIT OR Zlib | 12 | bytemuck, raw-window-handle, objc2-*, miniz_oxide |
| Apache-2.0 OR BSD-2-Clause OR MIT | 2 | zerocopy, zerocopy-derive |
| Apache-2.0 OR BSD-3-Clause OR MIT | 2 | num_enum, num_enum_derive |
| (Apache-2.0 OR MIT) AND BSD-3-Clause | 1 | encoding_rs |
| (Apache-2.0 OR MIT) AND Unicode-3.0 | 1 | unicode-ident |
| Apache-2.0 AND ISC | 1 | ring |
| Apache-2.0 AND MIT | 1 | dpi |
| Apache-2.0 OR ISC OR MIT | 2 | rustls, hyper-rustls |
| Apache-2.0 OR LGPL-2.1-or-later OR MIT | 2 | r-efi |
| Apache-2.0 OR BSL-1.0 | 1 | ryu |
| Apache-2.0 OR CC0-1.0 OR MIT-0 | 1 | dunce |
| Apache-2.0 WITH LLVM-exception | 1 | target-lexicon |
| Apache-2.0 | 3 | openssl, sync_wrapper, tao |
| BSD-3-Clause | 3 | alloc-no-stdlib, alloc-stdlib, subtle |
| BSD-3-Clause AND MIT | 1 | brotli |
| BSD-3-Clause OR MIT | 1 | brotli-decompressor |
| ISC | 3 | libloading, rustls-webpki, untrusted |
| 0BSD OR Apache-2.0 OR MIT | 1 | adler2 |
| Zlib | 2 | foldhash |

## Caduceus Workspace Crates (MIT)

The following crates are part of the Caduceus project itself and are licensed under MIT:

- caduceus-core
- caduceus-crdt
- caduceus-git
- caduceus-omniscience
- caduceus-orchestrator
- caduceus-permissions
- caduceus-providers
- caduceus-runtime
- caduceus-scanner
- caduceus-storage
- caduceus-tauri
- caduceus-telemetry
- caduceus-tools

## Notable Third-Party Licenses

### MIT License

The MIT license is a permissive license that allows use, copying, modification, and distribution
with minimal restrictions. Packages under this license include: tokio, hyper, rusqlite, tracing,
bytes, schemars, plist, and many GTK/WebKit/Tauri bindings.

### Apache-2.0 License

The Apache License 2.0 is a permissive license with explicit patent grant. The vast majority of
Rust ecosystem crates (serde, tokio, reqwest, syn, anyhow, thiserror, futures, regex, uuid, etc.)
are dual-licensed Apache-2.0 OR MIT.

### MPL-2.0 License

The Mozilla Public License 2.0 is a weak copyleft license. Modifications to MPL-2.0 files must
be made available under MPL-2.0, but the license is file-scoped (does not affect the rest of the
project). Packages: cssparser, selectors, dtoa-short, option-ext.

### BSD Licenses

Standard permissive licenses compatible with MIT and Apache-2.0 use.
Packages: alloc-no-stdlib, alloc-stdlib, subtle, brotli, brotli-decompressor.

### ISC License

A simplified permissive license equivalent to MIT/BSD-2-Clause.
Packages: libloading, rustls-webpki, untrusted.

### ring (Apache-2.0 AND ISC)

The `ring` crate (cryptographic operations) is licensed under a combination of Apache-2.0 and ISC.

### Unicode License

The `unicode-ident` and ICU4X crates use the Unicode License 3.0 (Unicode-3.0), which permits
use and modification of Unicode data and algorithms.

---

*This file was generated from `cargo license` output on the workspace crates. For the authoritative
license text of each dependency, refer to the respective crate's repository or crates.io page.*
