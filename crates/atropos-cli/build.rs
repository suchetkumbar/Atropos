fn main() {
    println!("cargo:rerun-if-changed=atr.rc");
    println!("cargo:rerun-if-changed=../../assets/atropos.manifest");
    println!("cargo:rerun-if-changed=../../assets/atropos.ico");
    // The manifest and icon are Windows resources; other targets have nothing to embed.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    embed_resource::compile("atr.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|e| panic!("embedding resources failed: {e}"));
}
