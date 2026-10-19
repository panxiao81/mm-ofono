// ModemManager's enum values come from its own C header (libmm-glib-dev /
// modemmanager-dev) rather than being copied by hand.
fn main() {
    let mm = pkg_config::Config::new()
        .cargo_metadata(false) // header only, nothing to link
        .probe("ModemManager")
        .expect("ModemManager.pc (install modemmanager-dev)");

    let bindings = bindgen::Builder::default()
        .header_contents("mm.h", "#include <ModemManager.h>")
        .clang_args(mm.include_paths.iter().map(|p| format!("-I{}", p.display())))
        .allowlist_type("MM.*")
        // Constants rather than Rust enums: many of these are bit flags.
        .constified_enum_module("MM.*")
        .prepend_enum_name(false)
        .generate()
        .expect("bindgen ModemManager.h");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("mm.rs");
    bindings.write_to_file(out).unwrap();
}
