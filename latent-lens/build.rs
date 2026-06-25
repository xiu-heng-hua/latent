//! Link against the system lensfun (dynamic linking) and generate Rust FFI
//! bindings from its public header with bindgen. Mirrors the LibRaw boundary in
//! `latent-raw`.
//!
//! Note the licensing split: the lensfun *library* is LGPL (linked here), but the
//! lens-profile *database* is CC-BY-SA and ships separately (`liblensfun-data`),
//! read at runtime — it is never vendored into this repository.

use std::env;
use std::path::PathBuf;

fn main() {
    let lib =
        pkg_config::probe_library("lensfun").expect("lensfun not found (install liblensfun-dev)");

    let bindings = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <lensfun/lensfun.h>")
        .clang_args(
            lib.include_paths
                .iter()
                .map(|p| format!("-I{}", p.display())),
        )
        // Only lensfun's own surface — keep the generated file small and skip the
        // transitively-included glib headers.
        .allowlist_function("lf_.*")
        .allowlist_type("lf.*")
        .allowlist_var("LF_.*")
        .generate()
        .expect("failed to generate lensfun bindings");

    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("bindings.rs"))
        .expect("failed to write bindings.rs");
}
