//! Link against the system LibRaw (dynamic linking — LGPL boundary) and
//! generate Rust FFI bindings from its public header with bindgen.

use std::env;
use std::path::PathBuf;

fn main() {
    let lib = pkg_config::probe_library("libraw").expect("libraw not found (install libraw-dev)");

    let bindings = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <libraw/libraw.h>")
        .clang_args(
            lib.include_paths
                .iter()
                .map(|p| format!("-I{}", p.display())),
        )
        // Only LibRaw's own surface — keep the generated file small.
        .allowlist_function("libraw_.*")
        .allowlist_type("libraw_.*")
        .allowlist_var("LIBRAW_.*")
        .generate()
        .expect("failed to generate LibRaw bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}
