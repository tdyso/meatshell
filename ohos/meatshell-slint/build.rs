//! Compiles the real upstream UI (`ui/app.slint`, same entry as the desktop
//! build) into this crate. The embedded font imports in app.slint
//! (Meatshell Mono + Material Icons) are embedded automatically; runtime
//! fonts (HarmonyOS Sans SC for UI text) are registered in lib.rs.
//!
//! The upstream `ui/` tree is copied to `OUT_DIR` before compiling so the one
//! OHOS workaround below can be applied to a copy; the repository's own
//! `ui/*.slint` files stay untouched.

use std::path::{Path, PathBuf};

/// Upstream sizes the session dialog's scroll area from
/// `body.preferred-height`, which under-reports the laid-out height by ~30 px
/// once the advanced section (port forwarding / login triggers) is expanded:
/// the card is already capped at `parent.height - 40px`, so the last row
/// (Test connection / Cancel / Create) can never be scrolled into view and is
/// clipped by the window's bottom edge. The row is a plain Rectangle with no
/// `clip`, so it is drawn outside the card and then cut by the surface.
///
/// Adding the missing slack to `content-height` only when the body actually
/// overflows the viewport keeps the collapsed dialog pixel-identical.
const SCROLL_FIX_FROM: &str = "content-height: body.preferred-height;";
const SCROLL_FIX_TO: &str =
    "content-height: body.preferred-height + (body.preferred-height + 1px > self.height ? 32px : 0px);";

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to)
        .unwrap_or_else(|e| panic!("create {}: {e}", to.display()));
    for entry in std::fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display())) {
        let entry = entry.expect("read dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target)
                .unwrap_or_else(|e| panic!("copy to {}: {e}", target.display()));
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=../../ui");
    println!("cargo:rerun-if-changed=../../assets");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("ui");
    copy_tree(Path::new("../../ui"), &out_dir);
    // The .slint files reference `@image-url("../assets/...")`, so the sibling
    // asset directory has to keep the same relative position next to the copy.
    copy_tree(
        Path::new("../../assets"),
        &out_dir.join("..").join("assets"),
    );

    let dialog = out_dir.join("session_dialog.slint");
    let source = std::fs::read_to_string(&dialog).expect("read session_dialog.slint");
    // Fail loudly if upstream changes the binding, instead of silently
    // shipping an unpatched dialog.
    assert!(
        source.contains(SCROLL_FIX_FROM),
        "session_dialog.slint no longer contains `{SCROLL_FIX_FROM}`; update the OHOS scroll fix"
    );
    std::fs::write(&dialog, source.replace(SCROLL_FIX_FROM, SCROLL_FIX_TO))
        .expect("write patched session_dialog.slint");

    slint_build::compile_with_config(
        out_dir.join("app.slint").to_str().expect("utf-8 path"),
        slint_build::CompilerConfiguration::new()
            .with_style("fluent".into())
            // Embed @image-url files into the binary; decoding happens at
            // runtime via i-slint-core's image-decoders feature (see Cargo.toml).
            // EmbedForSoftwareRenderer would pre-rasterize glyphs at compile
            // time, which panics on some font outlines (embed_glyphs.rs).
            .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles),
    )
    .expect("Slint build failed");
    // Upstream also bundles `lang/` translations; skipped here for now — the
    // source msgids are already Chinese, which matches the test device. Wire
    // .with_bundled_translations when locale switching is ported (P2-B).
}
