fn main() {
    // Tell cargo to rerun if the eBPF source changes
    println!("cargo:rerun-if-changed=../forge-kernel-ebpf/src/main.rs");
    println!("cargo:rerun-if-changed=../forge-kernel-ebpf/Cargo.toml");
    
    // For now, we'll skip building the eBPF program during regular builds
    // The eBPF program needs special tooling (bpf-linker) which may not be installed
    // In Phase 2, we'll add proper eBPF building infrastructure
    
    eprintln!("Note: eBPF program defined at crates/forge-kernel-ebpf/src/main.rs");
    eprintln!("      eBPF building will be configured in later phases");
}
