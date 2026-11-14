//! Packs the release TypeFlow exe into the installer and gives the installer
//! the app icon and a manifest.
//!
//! The app exe comes from `TYPEFLOW_EXE` if set, else `../target/release/typeflow.exe`.
//! tools/build-installer.ps1 builds the app first and sets it.

include!("../tools/winres.rs");

/// `asInvoker`: installs for the current user, so no admin prompt. Without a
/// manifest Windows would guess from the name "setup" that it needs admin.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="TypeFlow.Setup" version="1.0.0.0"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security><requestedPrivileges>
      <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
    </requestedPrivileges></security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application><supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/></application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </windowsSettings>
  </application>
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
      processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=../tools/winres.rs");
    println!("cargo:rerun-if-env-changed=TYPEFLOW_EXE");
    embed_resources("../assets/icons/typeflow.ico", Some(MANIFEST));

    let app = std::env::var("TYPEFLOW_EXE").unwrap_or_else(|_| "../target/release/typeflow.exe".into());
    println!("cargo:rerun-if-changed={app}");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("typeflow.exe");
    std::fs::copy(&app, &out).unwrap_or_else(|e| {
        panic!("TypeFlow exe not found at {app} ({e}). Build it first with tools/build-installer.ps1")
    });

    // the app's version, from its Cargo.toml
    let manifest = std::fs::read_to_string("../Cargo.toml").expect("read ../Cargo.toml");
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("version in ../Cargo.toml");
    println!("cargo:rerun-if-changed=../Cargo.toml");
    println!("cargo:rustc-env=TYPEFLOW_VERSION={version}");
}
