//! Windows: embed the app icon and version info in parquetry.exe.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon/Parquetry.ico");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../../assets/icon/Parquetry.ico")
            .set("ProductName", "Parquetry")
            .set("FileDescription", "Parquetry")
            .set("LegalCopyright", "Copyright (c) 2026 Roger Lefort");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {error}");
        }
    }
}
