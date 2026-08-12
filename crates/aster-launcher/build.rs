fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("aster-launcher.manifest");
        res.compile()
            .expect("failed to embed Windows manifest into aster-launcher.exe");
    }
}
