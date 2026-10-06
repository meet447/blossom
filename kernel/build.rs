fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed=linker.ld");
    let packed = std::path::Path::new(&dir).join("../target/initramfs.bin");
    println!("cargo:rerun-if-changed={}", packed.display());
    let destination = std::path::Path::new(&out).join("initramfs.bin");
    if packed.exists() {
        std::fs::copy(&packed, &destination).unwrap();
    } else {
        // Magic MXFS, zero files. `make verify` replaces this before the kernel build.
        let empty = [0x4D, 0x58, 0x46, 0x53, 0, 0, 0, 0];
        std::fs::write(&destination, empty).unwrap();
    }
}
