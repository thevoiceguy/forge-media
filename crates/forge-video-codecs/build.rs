//! With the `vpx` or `svt-av1` feature: bind the system libvpx or SVT-AV1
//! encoder headers at build time. Nothing to do otherwise.
//!
//! libvpx is bound here rather than through `env-libvpx-sys`, whose bindgen
//! (0.68) reads libclang 22's AST wrongly: every struct comes out empty.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "vpx")]
    {
        let lib = pkg_config::probe_library("vpx").expect("libvpx-dev (pkg-config vpx)");
        // The same selection env-libvpx-sys made, so `vpx.rs` sees the
        // same items: everything `v`/`V`, enums as Rust enums.
        let mut builder = bindgen::Builder::default()
            .header_contents(
                "wrapper.h",
                "#include <vpx/vp8cx.h>\n\
                 #include <vpx/vp8dx.h>\n\
                 #include <vpx/vpx_decoder.h>\n\
                 #include <vpx/vpx_encoder.h>\n",
            )
            .allowlist_type("^[vV].*")
            .allowlist_var("^[vV].*")
            .allowlist_function("^[vV].*")
            .rustified_enum("^v.*")
            .layout_tests(false)
            .generate_comments(false);
        for inc in &lib.include_paths {
            builder = builder.clang_arg(format!("-I{}", inc.display()));
        }
        let bindings = builder.generate().expect("bindgen vpx");
        let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        bindings.write_to_file(out.join("vpx.rs")).unwrap();
    }
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
