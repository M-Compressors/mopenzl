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
        .opt_level(2)
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

    compile("mopenzl_shim", &[PathBuf::from("shim/shim.c")]);
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
