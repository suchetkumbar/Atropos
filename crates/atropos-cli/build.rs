fn main() {
    println!("cargo:rerun-if-changed=atr.rc");
    println!("cargo:rerun-if-changed=../../assets/atropos.manifest");
    println!("cargo:rerun-if-changed=../../assets/atropos.ico");
    embed_resource::compile("atr.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|e| panic!("embedding resources failed: {e}"));
}
