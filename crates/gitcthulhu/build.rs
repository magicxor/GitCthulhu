//! Windows: embed `assets/app-icon/<channel>/icon.ico` as the executable icon.
//!
//! The channel is the `GITCTHULHU_CHANNEL` environment variable at build time (`release` by default).

fn main() {
    println!("cargo:rerun-if-env-changed=GITCTHULHU_CHANNEL");
    println!("cargo:rerun-if-changed=assets/app-icon");
    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    let channel = std::env::var("GITCTHULHU_CHANNEL").unwrap_or_else(|_| "release".into());
    let icon = format!("assets/app-icon/{channel}/icon.ico");
    if !std::path::Path::new(&icon).exists() {
        println!("cargo:warning=app icon {icon} not found; building without an icon");
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon);
    if let Err(err) = res.compile() {
        println!("cargo:warning=failed to embed the app icon: {err}");
    }
}
