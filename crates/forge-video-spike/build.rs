//! Bind the system SVT-AV1 encoder (libsvtav1enc-dev) at build time.
fn main() {
    let lib = pkg_config::Config::new()
        .probe("SvtAv1Enc")
        .expect("libsvtav1enc-dev (pkg-config SvtAv1Enc)");
    let mut builder = bindgen::Builder::default()
        .header_contents("wrapper.h", "#include <EbSvtAv1Enc.h>\n")
        .allowlist_function("svt_av1_.*")
        .allowlist_type("Eb.*")
        .allowlist_var("EB_.*")
        .prepend_enum_name(false)
        .derive_default(true);
    for inc in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", inc.display()));
    }
    let bindings = builder.generate().expect("bindgen SvtAv1Enc");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings.write_to_file(out.join("svt.rs")).unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
