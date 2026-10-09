//! Windows: embeds the app icon in yt-lite.exe (see resources/windows/yt-lite.rc).

fn main() {
    println!("cargo:rerun-if-changed=resources/windows/yt-lite.rc");
    println!("cargo:rerun-if-changed=resources/windows/yt-lite.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("resources/windows/yt-lite.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compiling resources/windows/yt-lite.rc");
    }
}
