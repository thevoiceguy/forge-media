//! With the `svt-av1` feature: bind the system SVT-AV1 encoder header at
//! build time. Nothing to do otherwise.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "svt-av1")]
    {
        let lib = pkg_config::Config::new()
            .atleast_version("2.0")
            .probe("SvtAv1Enc")
            .expect("libsvtav1enc-dev (pkg-config SvtAv1Enc >= 2.0)");
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
    }
}
