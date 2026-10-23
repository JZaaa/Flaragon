fn main() {
    slint_build::compile_with_config(
        "ui/main.slint",
        slint_build::CompilerConfiguration::new().with_style("fluent-light".into()),
    )
    .expect("Slint UI compilation");
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output"));
    let version = std::env::var("CARGO_PKG_VERSION").expect("Cargo version");
    let numbers = format!("{},0", version.replace('.', ","));
    for binary in ["Flaragon", "flaragon-deploy", "flaragon-cli"] {
        let source = out_dir.join(format!("{binary}.rc"));
        std::fs::write(
            &source,
            format!(
                r#"
#include "{}/app.rc"
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904B0"
  BEGIN
   VALUE "FileDescription", "Flaragon\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "ProductName", "Flaragon\0"
   VALUE "ProductVersion", "{version}\0"
   VALUE "OriginalFilename", "{binary}.exe\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#,
                std::env::var("CARGO_MANIFEST_DIR")
                    .expect("Cargo root")
                    .replace('\\', "/")
            ),
        )
        .expect("Version resource");
        let output = out_dir.join(format!("{binary}.res"));
        let status = std::process::Command::new("rc.exe")
            .arg("/nologo")
            .arg(format!("/fo{}", output.display()))
            .arg(&source)
            .status()
            .expect("Windows SDK resource compiler rc.exe");
        assert!(status.success(), "Windows icon resource compilation failed");
        println!("cargo:rustc-link-arg-bin={binary}={}", output.display());
    }
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/flaragon.ico");
    let manifest = out_dir.join("app.manifest");
    std::fs::write(
        &manifest,
        std::fs::read_to_string("app.manifest")
            .expect("App manifest")
            .replace("0.1.0.0", &format!("{version}.0")),
    )
    .expect("Versioned manifest");
    println!("cargo:rerun-if-changed=app.manifest");
    for binary in ["Flaragon", "flaragon-deploy", "flaragon-cli"] {
        println!("cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
