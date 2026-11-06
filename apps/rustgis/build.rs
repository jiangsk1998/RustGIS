//! On Windows, embeds the app icon in `rustgis.exe`, so Explorer, shortcuts and pinned taskbar
//! entries show the logo. The icon is built by the same code as the window icon
//! (`crates/render/src/logo.rs`), from the logo image it embeds.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../crates/render/src/logo.rs");
    println!("cargo:rerun-if-changed=../../crates/render/assets/huagis-logo.png");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows::embed_icon()?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(dead_code)]
#[path = "../../crates/render/src/logo.rs"]
mod logo;

#[cfg(windows)]
mod windows {
    use std::path::PathBuf;

    /// Writes an `.ico` of the logo (PNG entries, 16 to 256 px) and a resource script that
    /// references it, then compiles and links the script.
    pub fn embed_icon() -> Result<(), Box<dyn std::error::Error>> {
        let out = PathBuf::from(std::env::var("OUT_DIR")?);
        let ico = out.join("rustgis.ico");
        std::fs::write(&ico, super::logo::ico().ok_or("could not draw the icon")?)?;
        let version = std::env::var("CARGO_PKG_VERSION")?;
        // VERSIONINFO wants four numbers: drop a pre-release suffix (0.2.0-rc.1 -> 0,2,0,0).
        let nums: Vec<&str> = version.split('-').next().unwrap_or("0.0.0").split('.').collect();
        let num = |i: usize| nums.get(i).copied().unwrap_or("0");
        let file_version = format!("{},{},{},0", num(0), num(1), num(2));
        let rc = out.join("rustgis.rc");
        std::fs::write(
            &rc,
            format!(
                r#"1 ICON "{ico}"
1 VERSIONINFO
FILEVERSION {file_version}
PRODUCTVERSION {file_version}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "The RustGIS contributors"
      VALUE "FileDescription", "HuaGIS"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "rustgis"
      VALUE "LegalCopyright", "Copyright the HuaGIS contributors. Apache-2.0."
      VALUE "OriginalFilename", "rustgis.exe"
      VALUE "ProductName", "HuaGIS"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
                ico = ico.display().to_string().replace('\\', "\\\\"),
            ),
        )?;
        // Without a resource compiler (rc.exe or windres) the exe just has no icon: warn rather
        // than fail a plain `cargo build`.
        if let Err(e) = embed_resource::compile(&rc, embed_resource::NONE).manifest_optional() {
            println!("cargo:warning=rustgis.exe gets no icon: {e}");
        }
        Ok(())
    }
}
