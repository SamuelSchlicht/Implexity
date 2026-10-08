<!--
SPDX-License-Identifier: Apache-2.0
METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
Open-access statement and disclaimer: see DISCLAIMER.md.
METAPLEXIS-DISCLAIMER-END
-->


# Third-party dependencies

The Apache-2.0 grant in [LICENSE](LICENSE) covers the first-party code of this
workspace (Metaplexis Ingenieurgesellschaft GmbH). It does not relicense
third-party software. The informational [disclaimer](DISCLAIMER.md) does not
change those terms. Third-party crates are not vendored in this repository;
cargo downloads them from crates.io at the versions pinned in `Cargo.lock`.
An executable built from this workspace links the crates marked *linked*
below, and a binary distribution must carry their licence and notice texts:
[`THIRD_PARTY_LICENSES.txt`](THIRD_PARTY_LICENSES.txt) collects them.

Regenerate both files after every `Cargo.lock` change with
`python3 scripts/third_party_notices.py` (`--check` verifies them).

## Summary

* 436 third-party packages in `Cargo.lock`: 259 linked into the
  distributed executables (`implexity`, `implexity-mcp`, `implexity-workbench`,
  `implexity-egl-worker`) on at least one platform, 75 used only at
  compile time (build scripts, procedural macros), 13 used only by tests,
  benchmarks or the `xtask` maintenance tool, 89 compiled only
  for other platforms (Android, WebAssembly, ... branches of dependencies).
* Platforms evaluated: Linux x86_64 (GNU), Windows x86_64 (MSVC), macOS aarch64.
  A crate listed for one platform only is compiled only there.

## Copyleft check

No package is licensed *only* under GPL, LGPL or AGPL.

Packages that offer a GNU licence as one of several alternatives are used
under a permissive alternative of their SPDX expression:

* `r-efi` 5.3.0: `MIT OR Apache-2.0 OR LGPL-2.1-or-later` (other-platform)
* `r-efi` 6.0.0: `MIT OR Apache-2.0 OR LGPL-2.1-or-later` (other-platform)

MPL-2.0 (file-level copyleft; permitted by `deny.toml`) applies to:

* `cssparser` 0.37.0 (other-platform, -)
* `cssparser-macros` 0.7.1 (other-platform, -)
* `dtoa-short` 0.3.5 (other-platform, -)
* `option-ext` 0.2.0 (linked, linux, macos)
* `selectors` 0.38.0 (other-platform, -)

MPL-2.0 requires that the source of these files remains available
under MPL-2.0 (it is, on crates.io and in their repositories) and that the
licence text accompanies a binary distribution; it places no condition on
the rest of the executable. These crates are not modified here.

## Licences of the linked packages

| Licence expression (SPDX) | Packages |
|---|---:|
| `MIT OR Apache-2.0` | 108 |
| `MIT` | 91 |
| `Unicode-3.0` | 15 |
| `MIT/Apache-2.0` | 9 |
| `Apache-2.0 OR MIT` | 7 |
| `Zlib OR Apache-2.0 OR MIT` | 7 |
| `Unlicense OR MIT` | 4 |
| `Apache-2.0` | 2 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 2 |
| `Apache-2.0/MIT` | 2 |
| `MIT OR Zlib OR Apache-2.0` | 2 |
| `0BSD OR MIT OR Apache-2.0` | 1 |
| `Apache-2.0 AND MIT` | 1 |
| `Apache-2.0 OR BSL-1.0` | 1 |
| `BSD-2-Clause` | 1 |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 1 |
| `CC0-1.0 OR MIT-0 OR Apache-2.0` | 1 |
| `MIT AND BSD-3-Clause` | 1 |
| `MIT OR Apache-2.0 OR Zlib` | 1 |
| `MPL-2.0` | 1 |
| `Zlib` | 1 |

## Packages

`use`: *linked* into a distributed executable, *build* (compile time only).
`platforms`: where that use occurs. Development-only packages follow in the
next section.

`used under`: the alternative this distribution relies on where the package
offers a choice (its text is the one collected in THIRD_PARTY_LICENSES.txt).

| Package | Version | Licence (SPDX) | Used under | Use | Platforms |
|---|---|---|---|---|---|
| `adler2` | 2.0.1 | `0BSD OR MIT OR Apache-2.0` | `MIT` | linked | all |
| `aho-corasick` | 1.1.5 | `Unlicense OR MIT` | `MIT` | build | macos |
| `atk` | 0.18.2 | `MIT` |  | linked | linux |
| `atk-sys` | 0.18.2 | `MIT` |  | linked | linux |
| `atomic-wait` | 1.1.0 | `BSD-2-Clause` |  | linked | all |
| `atomic-waker` | 1.1.2 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `autocfg` | 1.5.1 | `Apache-2.0 OR MIT` | `MIT` | build | all |
| `axum` | 0.8.9 | `MIT` |  | linked | all |
| `axum-core` | 0.5.6 | `MIT` |  | linked | all |
| `base64` | 0.22.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `base64` | 0.23.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `bindgen` | 0.72.1 | `BSD-3-Clause` |  | build | macos |
| `bitflags` | 1.3.2 | `MIT/Apache-2.0` | `MIT` | linked | linux, windows |
| `bitflags` | 2.13.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `bitvec` | 1.1.1 | `MIT` |  | linked | all |
| `block-buffer` | 0.10.4 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `block2` | 0.6.2 | `MIT` |  | linked | macos |
| `bytemuck` | 1.25.2 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | all |
| `bytemuck_derive` | 1.12.1 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | build | all |
| `byteorder` | 1.5.0 | `Unlicense OR MIT` | `MIT` | linked | macos |
| `byteorder-lite` | 0.1.0 | `Unlicense OR MIT` | `MIT` | linked | all |
| `bytes` | 1.12.1 | `MIT` |  | linked | all |
| `cairo-rs` | 0.18.5 | `MIT` |  | linked | linux |
| `cairo-sys-rs` | 0.18.2 | `MIT` |  | linked | linux |
| `cc` | 1.5.1 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `cexpr` | 0.6.0 | `Apache-2.0/MIT` | `MIT` | build | macos |
| `cfg-expr` | 0.15.8 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `cfg-if` | 1.0.5 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `cfg_aliases` | 0.2.2 | `MIT` |  | build | macos |
| `clang-sys` | 1.9.1 | `Apache-2.0` |  | build | macos |
| `color_quant` | 1.1.0 | `MIT` |  | linked | all |
| `cookie` | 0.18.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `core-foundation` | 0.10.1 | `MIT OR Apache-2.0` | `MIT` | linked | macos |
| `core-foundation-sys` | 0.8.7 | `MIT OR Apache-2.0` | `MIT` | linked | macos |
| `core-graphics` | 0.25.0 | `MIT OR Apache-2.0` | `MIT` | linked | macos |
| `core-graphics-types` | 0.2.0 | `MIT OR Apache-2.0` | `MIT` | linked | macos |
| `cpufeatures` | 0.2.17 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crc32fast` | 1.5.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam` | 0.8.5 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam-channel` | 0.5.17 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam-deque` | 0.8.8 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam-epoch` | 0.9.21 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam-queue` | 0.3.14 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crossbeam-utils` | 0.8.23 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `crypto-common` | 0.1.7 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `data-encoding` | 2.11.1 | `MIT` |  | linked | all |
| `dbus` | 0.9.12 | `Apache-2.0/MIT` | `MIT` | linked | linux |
| `defer` | 0.2.1 | `MIT/Apache-2.0` | `MIT` | build | linux, windows |
| `deranged` | 0.5.8 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `digest` | 0.10.7 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `dirs` | 7.0.0 | `MIT OR Apache-2.0` | `MIT` | linked | linux, macos |
| `dirs-sys` | 0.5.0 | `MIT OR Apache-2.0` | `MIT` | linked | linux, macos |
| `dispatch2` | 0.3.1 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | macos |
| `displaydoc` | 0.2.7 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `dlopen2` | 0.8.2 | `MIT` |  | linked | linux |
| `dlopen2_derive` | 0.4.3 | `MIT` |  | build | linux |
| `dpi` | 0.1.2 | `Apache-2.0 AND MIT` |  | linked | all |
| `dunce` | 1.0.5 | `CC0-1.0 OR MIT-0 OR Apache-2.0` | `MIT-0` | linked | windows |
| `dyn-stack` | 0.13.2 | `MIT` |  | linked | all |
| `dyn-stack-macros` | 0.1.3 | `MIT` |  | build | all |
| `either` | 1.18.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `enum-as-inner` | 0.6.1 | `MIT/Apache-2.0` | `MIT` | build | macos |
| `equator` | 0.2.2 | `MIT` |  | linked | all |
| `equator` | 0.4.2 | `MIT` |  | linked | all |
| `equator` | 0.6.0 | `MIT` |  | linked | all |
| `equator-macro` | 0.2.1 | `MIT` |  | build | all |
| `equator-macro` | 0.4.2 | `MIT` |  | build | all |
| `equator-macro` | 0.6.0 | `MIT` |  | build | all |
| `equivalent` | 1.0.2 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `errno` | 0.3.14 | `MIT OR Apache-2.0` | `MIT` | linked | linux, macos |
| `faer` | 0.24.4 | `MIT` |  | linked | all |
| `faer-traits` | 0.24.0 | `MIT` |  | linked | all |
| `fdeflate` | 0.3.7 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `field-offset` | 0.3.6 | `MIT OR Apache-2.0` | `MIT` | linked | linux |
| `find-msvc-tools` | 0.1.14 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `flate2` | 1.1.10 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `foreign-types` | 0.5.0 | `MIT/Apache-2.0` | `MIT` | linked | macos |
| `foreign-types-macros` | 0.2.4 | `MIT/Apache-2.0` | `MIT` | build | macos |
| `foreign-types-shared` | 0.3.1 | `MIT/Apache-2.0` | `MIT` | linked | macos |
| `form_urlencoded` | 1.2.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `fs4` | 1.1.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `funty` | 2.0.0 | `MIT` |  | linked | all |
| `futures-channel` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `futures-core` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `futures-executor` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | linux |
| `futures-io` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | linux |
| `futures-macro` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `futures-sink` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `futures-task` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `futures-util` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `gdk` | 0.18.2 | `MIT` |  | linked | linux |
| `gdk-pixbuf` | 0.18.5 | `MIT` |  | linked | linux |
| `gdk-pixbuf-sys` | 0.18.0 | `MIT` |  | linked | linux |
| `gdk-sys` | 0.18.2 | `MIT` |  | linked | linux |
| `gdkwayland-sys` | 0.18.2 | `MIT` |  | linked | linux |
| `gdkx11` | 0.18.2 | `MIT` |  | linked | linux |
| `gdkx11-sys` | 0.18.2 | `MIT` |  | linked | linux |
| `gemm` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-c32` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-c64` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-common` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-f16` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-f32` | 0.19.0 | `MIT` |  | linked | all |
| `gemm-f64` | 0.19.0 | `MIT` |  | linked | all |
| `generativity` | 1.2.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `generic-array` | 0.14.7 | `MIT` |  | linked | all |
| `getrandom` | 0.3.4 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `gif` | 0.14.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `gio` | 0.18.4 | `MIT` |  | linked | linux |
| `gio-sys` | 0.18.1 | `MIT` |  | linked | linux |
| `glib` | 0.18.5 | `MIT` |  | linked | linux |
| `glib-macros` | 0.18.5 | `MIT` |  | build | linux |
| `glib-sys` | 0.18.1 | `MIT` |  | linked | linux |
| `glob` | 0.3.4 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `gobject-sys` | 0.18.0 | `MIT` |  | linked | linux |
| `gtk` | 0.18.2 | `MIT` |  | linked | linux |
| `gtk-sys` | 0.18.2 | `MIT` |  | linked | linux |
| `gtk3-macros` | 0.18.2 | `MIT` |  | build | linux |
| `half` | 2.7.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `hashbrown` | 0.17.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `heck` | 0.4.1 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `heck` | 0.5.0 | `MIT OR Apache-2.0` | `MIT` | build | linux, macos |
| `hex` | 0.4.3 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `http` | 1.5.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `http-body` | 1.1.0 | `MIT` |  | linked | all |
| `http-body-util` | 0.1.5 | `MIT` |  | linked | all |
| `httparse` | 1.10.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `httpdate` | 1.0.3 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `hyper` | 1.11.1 | `MIT` |  | linked | all |
| `hyper-util` | 0.1.21 | `MIT` |  | linked | all |
| `icu_collections` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_locale_core` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_normalizer` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_normalizer_data` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_properties` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_properties_data` | 2.3.0 | `Unicode-3.0` |  | linked | all |
| `icu_provider` | 2.3.1 | `Unicode-3.0` |  | linked | all |
| `idna` | 1.1.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `idna_adapter` | 1.2.2 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `image-webp` | 0.2.4 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `indexmap` | 2.14.2 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `interpol` | 0.2.1 | `MIT` |  | build | linux, windows |
| `itertools` | 0.13.0 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `itoa` | 1.0.18 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `javascriptcore-rs` | 1.1.2 | `MIT` |  | linked | linux |
| `javascriptcore-rs-sys` | 1.1.1 | `MIT` |  | linked | linux |
| `less-avc` | 0.1.5 | `MIT/Apache-2.0` | `MIT` | linked | all |
| `libc` | 0.2.189 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `libdbus-sys` | 0.2.7 | `Apache-2.0/MIT` | `MIT` | linked | linux |
| `libloading` | 0.8.9 | `ISC` |  | build | macos |
| `libm` | 0.2.16 | `MIT` |  | linked | all |
| `libproc` | 0.14.11 | `MIT` |  | linked | macos |
| `linux-raw-sys` | 0.12.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | linked | linux |
| `litemap` | 0.8.3 | `Unicode-3.0` |  | linked | all |
| `lock_api` | 0.4.14 | `MIT OR Apache-2.0` | `MIT` | linked | linux, windows |
| `log` | 0.4.34 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `matchit` | 0.8.4 | `MIT AND BSD-3-Clause` |  | linked | all |
| `matrixmultiply` | 0.3.11 | `MIT/Apache-2.0` | `MIT` | linked | all |
| `memchr` | 2.8.3 | `Unlicense OR MIT` | `MIT` | linked | all |
| `memoffset` | 0.9.1 | `MIT` |  | linked | linux |
| `mime` | 0.3.17 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `minimal-lexical` | 0.2.1 | `MIT/Apache-2.0` | `MIT` | build | macos |
| `miniz_oxide` | 0.8.9 | `MIT OR Zlib OR Apache-2.0` | `MIT` | linked | all |
| `miniz_oxide` | 0.9.1 | `MIT OR Zlib OR Apache-2.0` | `MIT` | linked | all |
| `mio` | 1.2.3 | `MIT` |  | linked | all |
| `nano-gemm` | 0.2.2 | `MIT` |  | linked | all |
| `nano-gemm-c32` | 0.2.1 | `MIT` |  | linked | all |
| `nano-gemm-c64` | 0.2.1 | `MIT` |  | linked | all |
| `nano-gemm-codegen` | 0.2.1 | `MIT` |  | build | all |
| `nano-gemm-core` | 0.2.1 | `MIT` |  | linked | all |
| `nano-gemm-f32` | 0.2.1 | `MIT` |  | linked | all |
| `nano-gemm-f64` | 0.2.1 | `MIT` |  | linked | all |
| `ndarray` | 0.17.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `nix` | 0.31.3 | `MIT` |  | linked | macos |
| `nom` | 7.1.3 | `MIT` |  | build | macos |
| `num-complex` | 0.4.6 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `num-conv` | 0.2.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `num-integer` | 0.1.47 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `num-traits` | 0.2.19 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `num_cpus` | 1.17.0 | `MIT OR Apache-2.0` | `MIT` | linked | linux, windows |
| `objc2` | 0.6.4 | `MIT` |  | linked | macos |
| `objc2-app-kit` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | macos |
| `objc2-core-foundation` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | macos |
| `objc2-encode` | 4.1.0 | `MIT` |  | linked | macos |
| `objc2-exception-helper` | 0.1.1 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | macos |
| `objc2-foundation` | 0.3.2 | `MIT` |  | linked | macos |
| `objc2-web-kit` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | macos |
| `once_cell` | 1.21.4 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `option-ext` | 0.2.0 | `MPL-2.0` |  | linked | linux, macos |
| `pango` | 0.18.3 | `MIT` |  | linked | linux |
| `pango-sys` | 0.18.0 | `MIT` |  | linked | linux |
| `parking_lot` | 0.12.5 | `MIT OR Apache-2.0` | `MIT` | linked | linux, windows |
| `parking_lot_core` | 0.9.12 | `MIT OR Apache-2.0` | `MIT` | linked | linux, windows |
| `paste` | 1.0.15 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `percent-encoding` | 2.3.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `pin-project-lite` | 0.2.17 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `pkg-config` | 0.3.34 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `png` | 0.18.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `potential_utf` | 0.1.6 | `Unicode-3.0` |  | linked | all |
| `powerfmt` | 0.2.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `ppv-lite86` | 0.2.21 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `private-gemm-x86` | 0.1.20 | `MIT` |  | linked | linux, windows |
| `proc-macro-crate` | 1.3.1 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `proc-macro-crate` | 2.0.2 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `proc-macro-error` | 1.0.4 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `proc-macro-error-attr` | 1.0.4 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `proc-macro2` | 1.0.107 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `pulp` | 0.22.3 | `MIT` |  | linked | all |
| `pulp-wasm-simd-flag` | 0.1.1 | `MIT` |  | linked | all |
| `qd` | 0.8.0 | `MIT` |  | linked | all |
| `quick-error` | 2.0.1 | `MIT/Apache-2.0` | `MIT` | linked | all |
| `quote` | 1.0.47 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `radium` | 0.7.0 | `MIT` |  | linked | all |
| `rand` | 0.9.5 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `rand_chacha` | 0.9.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `rand_core` | 0.9.5 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `raw-cpuid` | 11.6.0 | `MIT` |  | linked | all |
| `raw-window-handle` | 0.6.2 | `MIT OR Apache-2.0 OR Zlib` | `MIT` | linked | all |
| `rawpointer` | 0.2.1 | `MIT/Apache-2.0` | `MIT` | linked | all |
| `rayon` | 1.12.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `rayon-core` | 1.13.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `reborrow` | 0.5.5 | `MIT` |  | linked | all |
| `regex` | 1.13.1 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `regex-automata` | 0.4.18 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `regex-syntax` | 0.8.11 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `rustc-hash` | 2.1.3 | `Apache-2.0 OR MIT` | `MIT` | build | macos |
| `rustc_version` | 0.4.1 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `rustix` | 1.1.5 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | `MIT` | linked | linux, macos |
| `ryu` | 1.0.23 | `Apache-2.0 OR BSL-1.0` | `Apache-2.0` | linked | all |
| `scopeguard` | 1.2.0 | `MIT OR Apache-2.0` | `MIT` | linked | linux, windows |
| `semver` | 1.0.28 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `seq-macro` | 0.3.6 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `serde` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `serde_core` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `serde_derive` | 1.0.229 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `serde_json` | 1.0.151 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `serde_path_to_error` | 0.1.20 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `serde_spanned` | 0.6.9 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `serde_urlencoded` | 0.7.1 | `MIT/Apache-2.0` | `MIT` | linked | all |
| `sha1` | 0.10.7 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `sha2` | 0.10.9 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `shlex` | 1.3.0 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `shlex` | 2.0.1 | `MIT OR Apache-2.0` | `MIT` | build | macos |
| `signal-hook-registry` | 1.4.8 | `MIT OR Apache-2.0` | `MIT` | linked | linux, macos |
| `simd-adler32` | 0.3.10 | `MIT` |  | linked | all |
| `slab` | 0.4.12 | `MIT` |  | linked | all |
| `smallvec` | 1.16.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `socket2` | 0.6.5 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `soup3` | 0.5.0 | `MIT` |  | linked | linux |
| `soup3-sys` | 0.5.0 | `MIT` |  | linked | linux |
| `spindle` | 0.2.6 | `MIT` |  | linked | all |
| `stable_deref_trait` | 1.2.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `syn` | 1.0.109 | `MIT OR Apache-2.0` | `MIT` | build | linux, windows |
| `syn` | 2.0.119 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `syn` | 3.0.6 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `sync_wrapper` | 1.0.2 | `Apache-2.0` |  | linked | all |
| `synstructure` | 0.14.0 | `MIT` |  | build | all |
| `sysctl` | 0.6.0 | `MIT` |  | linked | macos |
| `system-deps` | 6.2.2 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `tao` | 0.37.1 | `Apache-2.0` |  | linked | all |
| `tap` | 1.0.1 | `MIT` |  | linked | all |
| `target-lexicon` | 0.12.16 | `Apache-2.0 WITH LLVM-exception` |  | build | linux |
| `thiserror` | 1.0.69 | `MIT OR Apache-2.0` | `MIT` | linked | linux, macos |
| `thiserror` | 2.0.21 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `thiserror-impl` | 1.0.69 | `MIT OR Apache-2.0` | `MIT` | build | linux, macos |
| `thiserror-impl` | 2.0.21 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `time` | 0.3.55 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `time-core` | 0.1.9 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `time-macros` | 0.2.32 | `MIT OR Apache-2.0` | `MIT` | build | all |
| `tinystr` | 0.8.4 | `Unicode-3.0` |  | linked | all |
| `tinyvec` | 1.13.3 | `Zlib OR Apache-2.0 OR MIT` | `MIT` | linked | all |
| `tokio` | 1.53.1 | `MIT` |  | linked | all |
| `tokio-macros` | 2.7.2 | `MIT` |  | build | all |
| `tokio-tungstenite` | 0.29.0 | `MIT` |  | linked | all |
| `toml` | 0.8.2 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `toml_datetime` | 0.6.3 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `toml_edit` | 0.19.15 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `toml_edit` | 0.20.2 | `MIT OR Apache-2.0` | `MIT` | build | linux |
| `tower` | 0.5.3 | `MIT` |  | linked | all |
| `tower-layer` | 0.3.3 | `MIT` |  | linked | all |
| `tower-service` | 0.3.3 | `MIT` |  | linked | all |
| `tungstenite` | 0.29.0 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `typenum` | 1.20.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `unicode-ident` | 1.0.26 | `(MIT OR Apache-2.0) AND Unicode-3.0` |  | build | all |
| `unicode-normalization` | 0.1.25 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `unicode-segmentation` | 1.13.3 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `ureq` | 3.4.2 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `ureq-proto` | 0.6.4 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `url` | 2.5.8 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `utf8-zero` | 0.8.1 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `utf8_iter` | 1.0.4 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `version-compare` | 0.2.1 | `MIT` |  | build | linux |
| `version_check` | 0.9.5 | `MIT/Apache-2.0` | `MIT` | build | all |
| `webkit2gtk` | 2.0.2 | `MIT` |  | linked | linux |
| `webkit2gtk-sys` | 2.0.2 | `MIT` |  | linked | linux |
| `webview2-com` | 0.39.1 | `MIT` |  | linked | windows |
| `webview2-com-macros` | 0.8.1 | `MIT` |  | build | windows |
| `webview2-com-sys` | 0.39.1 | `MIT` |  | linked | windows |
| `weezl` | 0.1.12 | `MIT OR Apache-2.0` | `MIT` | linked | all |
| `winapi` | 0.3.9 | `MIT/Apache-2.0` | `MIT` | linked | windows |
| `winapi-util` | 0.1.11 | `Unlicense OR MIT` | `MIT` | linked | windows |
| `windows` | 0.62.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-collections` | 0.3.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-core` | 0.62.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-future` | 0.3.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-implement` | 0.60.2 | `MIT OR Apache-2.0` | `MIT` | build | windows |
| `windows-interface` | 0.59.3 | `MIT OR Apache-2.0` | `MIT` | build | windows |
| `windows-link` | 0.2.1 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-numerics` | 0.3.1 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-permissions` | 0.2.4 | `MIT` |  | linked | windows |
| `windows-result` | 0.4.1 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-strings` | 0.5.1 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-sys` | 0.42.0 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-sys` | 0.61.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-threading` | 0.2.1 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows-version` | 0.1.7 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `windows_x86_64_msvc` | 0.42.2 | `MIT OR Apache-2.0` | `MIT` | linked | windows |
| `winnow` | 0.5.40 | `MIT` |  | build | linux |
| `writeable` | 0.6.4 | `Unicode-3.0` |  | linked | all |
| `wry` | 0.57.0 | `Apache-2.0 OR MIT` | `MIT` | linked | all |
| `wyz` | 0.5.1 | `MIT` |  | linked | all |
| `x11` | 2.21.0 | `MIT` |  | linked | linux |
| `x11-dl` | 2.21.0 | `MIT` |  | linked | linux |
| `yoke` | 0.8.3 | `Unicode-3.0` |  | linked | all |
| `yoke-derive` | 0.8.3 | `Unicode-3.0` |  | build | all |
| `zerocopy` | 0.8.59 | `BSD-2-Clause OR Apache-2.0 OR MIT` | `MIT` | linked | all |
| `zerocopy-derive` | 0.8.59 | `BSD-2-Clause OR Apache-2.0 OR MIT` | `MIT` | build | all |
| `zerofrom` | 0.1.8 | `Unicode-3.0` |  | linked | all |
| `zerofrom-derive` | 0.1.8 | `Unicode-3.0` |  | build | all |
| `zerotrie` | 0.2.5 | `Unicode-3.0` |  | linked | all |
| `zerovec` | 0.11.8 | `Unicode-3.0` |  | linked | all |
| `zerovec-derive` | 0.11.6 | `Unicode-3.0` |  | build | all |
| `zlib-rs` | 0.6.8 | `Zlib` |  | linked | all |
| `zmij` | 1.0.23 | `MIT` |  | linked | all |

## Development-only and other-platform packages

Not part of any distributed executable for the evaluated platforms: *dev*
(tests, benchmarks, examples, `xtask`) or *other-platform* (locked for a
platform branch of a dependency that these platforms never compile).

| Package | Version | Licence (SPDX) | Use |
|---|---|---|---|
| `bit-set` | 0.8.0 | `Apache-2.0 OR MIT` | dev |
| `bit-vec` | 0.8.0 | `Apache-2.0 OR MIT` | dev |
| `cesu8` | 1.1.0 | `Apache-2.0/MIT` | other-platform |
| `combine` | 4.6.8 | `MIT` | other-platform |
| `crunchy` | 0.2.4 | `MIT` | other-platform |
| `cssparser` | 0.37.0 | `MPL-2.0` | other-platform |
| `cssparser-macros` | 0.7.1 | `MPL-2.0` | other-platform |
| `derive_more` | 2.1.1 | `MIT` | other-platform |
| `derive_more-impl` | 2.1.1 | `MIT` | other-platform |
| `dom_query` | 0.28.0 | `MIT` | other-platform |
| `dtoa` | 1.0.11 | `MIT OR Apache-2.0` | other-platform |
| `dtoa-short` | 0.3.5 | `MPL-2.0` | other-platform |
| `fastrand` | 2.5.0 | `Apache-2.0 OR MIT` | dev |
| `fnv` | 1.0.7 | `Apache-2.0 / MIT` | dev |
| `foldhash` | 0.2.0 | `Zlib` | other-platform |
| `generator` | 0.8.10 | `MIT/Apache-2.0` | other-platform |
| `getrandom` | 0.4.3 | `MIT OR Apache-2.0` | dev |
| `hermit-abi` | 0.5.3 | `MIT OR Apache-2.0` | other-platform |
| `html5ever` | 0.39.0 | `MIT OR Apache-2.0` | other-platform |
| `jni` | 0.21.1 | `MIT/Apache-2.0` | other-platform |
| `jni-sys` | 0.3.1 | `MIT OR Apache-2.0` | other-platform |
| `jni-sys` | 0.4.1 | `MIT OR Apache-2.0` | other-platform |
| `jni-sys-macros` | 0.4.1 | `MIT OR Apache-2.0` | other-platform |
| `lazy_static` | 1.5.0 | `MIT OR Apache-2.0` | other-platform |
| `libredox` | 0.1.25 | `MIT` | other-platform |
| `loom` | 0.7.2 | `MIT` | other-platform |
| `markup5ever` | 0.39.0 | `MIT OR Apache-2.0` | other-platform |
| `matchers` | 0.2.0 | `MIT` | other-platform |
| `ndk` | 0.9.0 | `MIT OR Apache-2.0` | other-platform |
| `ndk-context` | 0.1.1 | `MIT OR Apache-2.0` | other-platform |
| `ndk-sys` | 0.6.0+11769913 | `MIT OR Apache-2.0` | other-platform |
| `new_debug_unreachable` | 1.0.6 | `MIT` | other-platform |
| `nu-ansi-term` | 0.50.3 | `MIT` | other-platform |
| `num-dual` | 0.15.0 | `MIT OR Apache-2.0` | dev |
| `num_enum` | 0.7.6 | `BSD-3-Clause OR MIT OR Apache-2.0` | other-platform |
| `num_enum_derive` | 0.7.6 | `BSD-3-Clause OR MIT OR Apache-2.0` | other-platform |
| `objc2-cloud-kit` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-core-data` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-core-graphics` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-core-image` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-core-location` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-core-text` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-io-surface` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-quartz-core` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-ui-kit` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `objc2-user-notifications` | 0.3.2 | `Zlib OR Apache-2.0 OR MIT` | other-platform |
| `phf` | 0.13.1 | `MIT` | other-platform |
| `phf_codegen` | 0.13.1 | `MIT` | other-platform |
| `phf_generator` | 0.13.1 | `MIT` | other-platform |
| `phf_macros` | 0.13.1 | `MIT` | other-platform |
| `phf_shared` | 0.13.1 | `MIT` | other-platform |
| `portable-atomic` | 1.15.0 | `Apache-2.0 OR MIT` | other-platform |
| `portable-atomic-util` | 0.2.8 | `Apache-2.0 OR MIT` | other-platform |
| `precomputed-hash` | 0.1.1 | `MIT` | other-platform |
| `proc-macro-crate` | 3.5.0 | `MIT OR Apache-2.0` | other-platform |
| `proptest` | 1.11.0 | `MIT OR Apache-2.0` | dev |
| `quick-error` | 1.2.3 | `MIT/Apache-2.0` | dev |
| `r-efi` | 5.3.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | other-platform |
| `r-efi` | 6.0.0 | `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | other-platform |
| `rand_xorshift` | 0.4.0 | `MIT OR Apache-2.0` | dev |
| `redox_syscall` | 0.5.18 | `MIT` | other-platform |
| `redox_users` | 0.5.3 | `MIT` | other-platform |
| `rustversion` | 1.0.23 | `MIT OR Apache-2.0` | other-platform |
| `rusty-fork` | 0.3.1 | `MIT/Apache-2.0` | dev |
| `same-file` | 1.0.6 | `Unlicense/MIT` | other-platform |
| `scoped-tls` | 1.0.1 | `MIT/Apache-2.0` | other-platform |
| `selectors` | 0.38.0 | `MPL-2.0` | other-platform |
| `servo_arc` | 0.4.3 | `MIT OR Apache-2.0` | other-platform |
| `sharded-slab` | 0.1.7 | `MIT` | other-platform |
| `siphasher` | 1.0.4 | `MIT OR Apache-2.0` | other-platform |
| `string_cache` | 0.9.0 | `MIT OR Apache-2.0` | other-platform |
| `string_cache_codegen` | 0.6.1 | `MIT OR Apache-2.0` | other-platform |
| `tao-macros` | 0.1.4 | `MIT OR Apache-2.0` | other-platform |
| `tempfile` | 3.27.0 | `MIT OR Apache-2.0` | dev |
| `tendril` | 0.5.1 | `MIT OR Apache-2.0` | other-platform |
| `thread_local` | 1.1.10 | `MIT OR Apache-2.0` | other-platform |
| `toml_datetime` | 1.1.1+spec-1.1.0 | `MIT OR Apache-2.0` | other-platform |
| `toml_edit` | 0.25.15+spec-1.1.0 | `MIT OR Apache-2.0` | other-platform |
| `toml_parser` | 1.1.3+spec-1.1.0 | `MIT OR Apache-2.0` | other-platform |
| `tracing` | 0.1.44 | `MIT` | other-platform |
| `tracing-core` | 0.1.36 | `MIT` | other-platform |
| `tracing-log` | 0.2.0 | `MIT` | other-platform |
| `tracing-subscriber` | 0.3.23 | `MIT` | other-platform |
| `unarray` | 0.1.4 | `MIT OR Apache-2.0` | dev |
| `valuable` | 0.1.1 | `MIT` | other-platform |
| `wait-timeout` | 0.2.1 | `MIT/Apache-2.0` | dev |
| `walkdir` | 2.5.0 | `Unlicense/MIT` | other-platform |
| `wasi` | 0.11.1+wasi-snapshot-preview1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | other-platform |
| `wasip2` | 1.0.4+wasi-0.2.12 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | other-platform |
| `web_atoms` | 0.2.6 | `MIT OR Apache-2.0` | other-platform |
| `winapi-i686-pc-windows-gnu` | 0.4.0 | `MIT/Apache-2.0` | other-platform |
| `winapi-x86_64-pc-windows-gnu` | 0.4.0 | `MIT/Apache-2.0` | other-platform |
| `windows-sys` | 0.45.0 | `MIT OR Apache-2.0` | other-platform |
| `windows-targets` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_aarch64_gnullvm` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_aarch64_msvc` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_i686_gnu` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_i686_msvc` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_x86_64_gnu` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `windows_x86_64_gnullvm` | 0.42.2 | `MIT OR Apache-2.0` | other-platform |
| `winnow` | 1.0.4 | `MIT` | other-platform |
| `wit-bindgen` | 0.57.1 | `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | other-platform |

## Components outside Cargo

* **Browser workbench** (`viewer/`): first-party JavaScript/HTML/CSS, vendored
  byte-identical from the Python reference and embedded in `implexity`. No font
  binaries are distributed; the viewer uses the operating system's sans-serif
  fonts and fetches no external font service. `viewer/fonts/OFL.txt` (the
  historical IBM Plex OFL notice) is retained for provenance only.
* **Colour-map tables** (`crates/implexity-render/src/dynamic/colormap_data.rs`):
  viridis, magma, inferno and plasma by Nathaniel J. Smith, Stefan van der Walt and
  (viridis) Eric Firing, dedicated to the public domain under CC0 1.0; rounded to
  8-bit sRGB from the Matplotlib source by `fixtures/generator/gen_dynamic_colormaps.py`.
  The diverging (Moreland 2009) and cyclic maps are computed, not copied.
* **System libraries loaded at run time, not distributed**: the WebView of the
  desktop window (Microsoft Edge WebView2 Runtime on Windows, WKWebView on macOS,
  WebKitGTK 4.1 on Linux), `libEGL`/GLES drivers for `implexity-egl-worker`, and
  an optional locally installed Chromium/Chrome driven over the DevTools protocol
  for viewer capture. Their own licences apply to those installations. On Linux
  the desktop window links dynamically against the system GTK 3 and WebKitGTK
  libraries (LGPL-2.1-or-later / LGPL-2.0-or-later, installed by the operating
  system, never bundled or modified here); the `gtk`/`webkit2gtk` crates listed
  above are MIT-licensed bindings to them.
* **Windows installation bundle**: `windows/build_windows_installation_bundle.ps1`
  may add Microsoft's WebView2 bootstrapper (`MicrosoftEdgeWebView2Setup.exe`,
  redistributed under Microsoft's WebView2 distribution terms) and is compiled
  with Inno Setup (a build tool; its licence governs the tool, not the payload).
* **Python reference**: fixture generators under `fixtures/generator/` run
  against the separately installed Python implementation (NumPy, SciPy, JAX); no
  Python package is distributed with the Rust executables.

Preserve these notices when redistributing. First-party file headers do not
override third-party notices.
