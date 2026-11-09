# Contributing to mopenzl

Thank you for helping. Bug reports, differential test cases, fuzz findings,
documentation fixes, and code are all welcome. By participating you agree to
follow the [Code of Conduct](CODE_OF_CONDUCT.md). Security issues are reported
privately; see [SECURITY.md](SECURITY.md).

## Before you start

- For anything beyond a small fix, open an issue first.
- Read [`architecture/README.md`](architecture/README.md); it describes the
  code as it is and is updated with every change.
- Encoding is **byte-identical to OpenZL v0.3.0** (the pinned
  `openzl-ffi/vendor/openzl` revision). A change that writes different bytes
  will not be merged unless the reference writes them too.
- Do not add a dependency on an existing OpenZL implementation. Zstandard
  comes from `mzstd`; new OpenZL codecs are ported into this crate.
- Do not edit `openzl-ffi/vendor/` unless the change is an upstream update.

## Setting up

```sh
git clone --recurse-submodules https://github.com/M-Compressors/mopenzl.git
```

or `git submodule update --init` in an existing clone. A C compiler is
required for `openzl-ffi`. The minimum supported Rust version is 1.89.

## Code guidelines

The full rules are in [`AGENTS.md`](AGENTS.md): public modules own the API and
private `core` modules the implementation; no `core`, SIMD or `mzstd` types in
public signatures; `thiserror` errors without panics on recoverable failures;
100% function coverage; SIMD kernels with a scalar baseline, run under Miri
and AddressSanitizer; Criterion evidence for performance claims.

## Before opening a pull request

Run the checks in [docs/development.md](docs/development.md), update
`architecture/` and `CHANGELOG.md`, and describe how you verified the change.
