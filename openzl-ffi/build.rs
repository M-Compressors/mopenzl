use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Every `.c` file below `directory`, sorted so the archive is reproducible.
fn c_sources(directory: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries = fs::read_dir(&next)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", next.display()));
        for entry in entries {
            let path = entry.expect("failed to read vendored source entry").path();
            if path.is_dir() {
                if recursive {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|extension| extension == "c") {
                sources.push(path);
            }
        }
    }
    sources.sort();
    sources
}

/// The C optimization level: `-O2` for tests, or `OPENZL_FFI_OPT_LEVEL`
/// (`3` matches OpenZL's CMake release build) for benchmarks.
fn opt_level() -> u32 {
    println!("cargo:rerun-if-env-changed=OPENZL_FFI_OPT_LEVEL");
    env::var("OPENZL_FFI_OPT_LEVEL")
        .ok()
        .and_then(|level| level.parse().ok())
        .unwrap_or(2)
}

/// The define OSS-Fuzz and ClusterFuzzLite put in `CFLAGS`.
const FUZZING_MODE: &str = "FUZZING_BUILD_MODE_UNSAFE_FOR_PRODUCTION";

/// Refuses to build the reference in fuzzing mode. With [`FUZZING_MODE`]
/// defined, the vendored OpenZL skips frame checksums, accepts trailing bytes
/// and caps LZ match lengths at 1024, and zstd and lz4 change too: the
/// library is no longer the reference this crate is an oracle for, and every
/// differential test against it reports false findings. `cc` appends the
/// environment's flags after its own, so a `-U` here could not undo it.
fn reject_fuzzing_mode() {
    let target = env::var("TARGET").unwrap_or_default();
    let names = [
        format!("CFLAGS_{target}"),
        format!("CFLAGS_{}", target.replace('-', "_")),
        "TARGET_CFLAGS".to_string(),
        "HOST_CFLAGS".to_string(),
        "CFLAGS".to_string(),
    ];
    for name in names {
        println!("cargo:rerun-if-env-changed={name}");
        if env::var(&name).is_ok_and(|flags| flags.contains(FUZZING_MODE)) {
            panic!(
                "{name} defines {FUZZING_MODE}, which turns the reference into a \
                 non-conforming fuzzing build; remove it from {name} (see \
                 .clusterfuzzlite/build.sh)"
            );
        }
    }
}

fn base_build() -> cc::Build {
    let mut build = cc::Build::new();
    build
        .include("vendor/openzl/include")
        .include("vendor/openzl/src")
        .include("vendor/openzl")
        .include("vendor/zstd/lib")
        .include("vendor/lz4/lib")
        .define("NDEBUG", None)
        // The assembly Huffman decoders are optional fast paths; the C
        // fallbacks decode the same bytes and need no assembler.
        .define("ZSTD_DISABLE_ASM", None)
        .define("HUF_DISABLE_ASM", None)
        .flag_if_supported("-std=c11")
        // Pin floating point: fused multiply-adds (clang's aarch64 default)
        // would change the numeric selectors' scores, hence the bytes.
        .flag_if_supported("-ffp-contract=off")
        .opt_level(opt_level())
        .cargo_metadata(false)
        .warnings(false);
    build
}

fn compile(name: &str, sources: &[PathBuf]) {
    base_build().files(sources).compile(name);
    for source in sources {
        println!("cargo:rerun-if-changed={}", source.display());
    }
}

fn main() {
    reject_fuzzing_mode();
    let output_directory = env::var("OUT_DIR").expect("OUT_DIR is not set");

    let zstd: Vec<PathBuf> = ["common", "compress", "decompress", "dictBuilder"]
        .iter()
        .flat_map(|part| c_sources(&Path::new("vendor/zstd/lib").join(part), false))
        .collect();
    let lz4: Vec<PathBuf> = ["lz4.c", "lz4hc.c"]
        .iter()
        .map(|file| Path::new("vendor/lz4/lib").join(file))
        .collect();
    let openzl = c_sources(Path::new("vendor/openzl/src/openzl"), true);
    assert!(
        !openzl.is_empty(),
        "vendor/openzl is empty: run `git submodule update --init`"
    );

    // Per-family test helpers live next to `shim.c`; a new file reruns this.
    println!("cargo:rerun-if-changed=shim");
    compile("mopenzl_shim", &c_sources(Path::new("shim"), false));
    compile("openzl", &openzl);
    compile("openzl_zstd", &zstd);
    compile("openzl_lz4", &lz4);

    // Dependents precede their dependencies for single-pass static linkers.
    println!("cargo:rustc-link-search=native={output_directory}");
    println!("cargo:rustc-link-lib=static=mopenzl_shim");
    println!("cargo:rustc-link-lib=static=openzl");
    println!("cargo:rustc-link-lib=static=openzl_zstd");
    println!("cargo:rustc-link-lib=static=openzl_lz4");

    let target_family = env::var("CARGO_CFG_TARGET_FAMILY").unwrap_or_default();
    if target_family == "unix"
        && !env::var("CARGO_CFG_TARGET_VENDOR").is_ok_and(|vendor| vendor == "apple")
    {
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=pthread");
    }
    println!(
        "cargo:include={}",
        absolute("vendor/openzl/include").display()
    );
    println!("cargo:rerun-if-changed=UPSTREAM_VERSION");
}

fn absolute(path: impl AsRef<Path>) -> PathBuf {
    PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"))
        .join(path)
}
