fn main() {
    println!("cargo:rerun-if-changed=directcut.rc");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("directcut.rc", embed_resource::NONE)
            .manifest_required()
            .expect("Compile DirectCut RS Windows resources");
    }
}
