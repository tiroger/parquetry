//! PARQUETRY_VARIANT=preview builds Parquetry Preview (see src/variant.rs).
//! Windows: embed the app icon and version info in parquetry.exe.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PARQUETRY_VARIANT");
    println!("cargo::rustc-check-cfg=cfg(parquetry_preview)");
    let preview = std::env::var("PARQUETRY_VARIANT").is_ok_and(|v| v == "preview");
    if preview {
        println!("cargo:rustc-cfg=parquetry_preview");
    }
    let (icon, name) = if preview {
        ("../../assets/icon/Parquetry-Preview.ico", "Parquetry Preview")
    } else {
        ("../../assets/icon/Parquetry.ico", "Parquetry")
    };
    println!("cargo:rerun-if-changed={icon}");
    #[cfg(not(windows))]
    let _ = (icon, name);
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon(icon)
            .set("ProductName", name)
            .set("FileDescription", name)
            .set("LegalCopyright", "Copyright (c) 2026 Roger Lefort");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {error}");
        }
    }
}
