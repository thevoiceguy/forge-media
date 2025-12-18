fn main() {
    // Use pkg-config to find bcg729
    if cfg!(feature = "system") {
        match pkg_config::Config::new()
            .atleast_version("1.0.0")
            .probe("libbcg729")
        {
            Ok(library) => {
                // pkg-config succeeded
                for include_path in &library.include_paths {
                    println!("cargo:include={}", include_path.display());
                }
                println!("cargo:rerun-if-changed=build.rs");
            }
            Err(e) => {
                // Fallback: try manual linking
                eprintln!("pkg-config failed to find libbcg729: {}", e);
                eprintln!("Attempting manual linking...");

                // Common installation paths
                let possible_paths = vec!["/usr/lib", "/usr/local/lib", "/opt/homebrew/lib"];

                for path in &possible_paths {
                    println!("cargo:rustc-link-search=native={}", path);
                }

                println!("cargo:rustc-link-lib=bcg729");

                eprintln!("\nNote: If build fails, install bcg729:");
                eprintln!("  Ubuntu/Debian: sudo apt-get install libbcg729-dev");
                eprintln!("  Fedora/RHEL:   sudo dnf install bcg729-devel");
                eprintln!("  macOS:         brew install bcg729");
                eprintln!("  From source:   https://github.com/BelledonneCommunications/bcg729");
            }
        }
    } else {
        println!("cargo:rustc-link-lib=bcg729");
    }

    println!("cargo:rerun-if-changed=build.rs");
}
