fn main() {
    println!("cargo:rerun-if-changed=AppIcon.ico");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=locales");

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("AppIcon.ico");
        resource.set_manifest_file("app.manifest");
        resource.compile().expect("failed to compile MGEXEgui Windows resources");
    }
}
