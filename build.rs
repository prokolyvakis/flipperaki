// Rust bindings are generated from the single shared schema at build time;
// the C bindings in protocol/generated/ come from the same file via nanopb.
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=protocol/buddy.proto");
    prost_build::compile_protos(&["protocol/buddy.proto"], &["protocol"])
}
