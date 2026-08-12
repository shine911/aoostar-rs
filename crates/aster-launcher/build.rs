fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("aster-launcher.manifest");
        // Embed the tray icon under the resource name "aster-launcher" (not
        // the default numeric id `set_icon` would use) so it matches what
        // `tray.rs`'s `IconSource::Resource("aster-launcher")` looks up at
        // runtime via `LoadImageW` with that exact string as the resource
        // name.
        res.set_icon_with_id("aster-launcher.ico", "aster-launcher");
        res.compile()
            .expect("failed to embed Windows manifest/icon into aster-launcher.exe");
    }
}
