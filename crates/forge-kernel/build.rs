use std::env;
use std::path::PathBuf;

fn main() {
    // Tell cargo to rerun if the pre-compiled eBPF object changes
    println!("cargo:rerun-if-changed=ebpf/rtp_forward.o");

    // Only try to embed eBPF on Linux
    if cfg!(not(target_os = "linux")) {
        eprintln!("Skipping eBPF: not on Linux");
        return;
    }

    // Check if the pre-compiled eBPF object exists
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let ebpf_path = PathBuf::from(&manifest_dir).join("ebpf/rtp_forward.o");

    if ebpf_path.exists() {
        eprintln!("✓ Found pre-compiled eBPF program: ebpf/rtp_forward.o");
        eprintln!(
            "  Size: {} bytes",
            std::fs::metadata(&ebpf_path).unwrap().len()
        );

        // Tell the Rust compiler where to find it for include_bytes! (use absolute path)
        println!("cargo:rustc-env=EBPF_OBJECT_PATH={}", ebpf_path.display());
    } else {
        eprintln!("Warning: Pre-compiled eBPF not found at ebpf/rtp_forward.o");
        eprintln!("         XDP will run in stub mode");
        eprintln!("         To compile eBPF, run: cd crates/forge-kernel-ebpf && cargo +nightly build --lib --release --target=bpfel-unknown-none");
    }
}
