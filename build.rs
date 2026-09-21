fn main() {
    println!("cargo:rerun-if-changed=resources/app.rc");
    println!("cargo:rerun-if-changed=resources/app.manifest");
    println!("cargo:rerun-if-changed=resources/app.ico");
    println!("cargo:rerun-if-changed=Cargo.toml");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo package version");
        let components: Vec<u16> = ["MAJOR", "MINOR", "PATCH"]
            .map(|part| {
                std::env::var(format!("CARGO_PKG_VERSION_{part}"))
                    .expect("Cargo version component")
                    .parse()
                    .expect("Windows version components must fit in 16 bits")
            })
            .into();
        let definitions = [
            format!(
                "APP_VERSION={},{},{},0",
                components[0], components[1], components[2]
            ),
            format!("APP_VERSION_STRING=\"{version}\""),
        ];
        embed_resource::compile("resources/app.rc", &definitions)
            .manifest_required()
            .expect("failed to embed the required Windows manifest");
        embed_resource::compile_for_examples("resources/app.rc", &definitions)
            .manifest_required()
            .expect("failed to embed the pixel fixture's required Windows manifest");
    }
}
