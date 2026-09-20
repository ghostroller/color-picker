fn main() {
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/app.rc", embed_resource::NONE)
            .manifest_required()
            .expect("failed to embed the required Windows manifest");
    }
}
