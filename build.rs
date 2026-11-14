//! Puts assets/icons/typeflow.ico into the exe as its icon (what Explorer, the
//! taskbar and Alt+Tab show). See tools/winres.rs.

include!("tools/winres.rs");

fn main() {
    println!("cargo:rerun-if-changed=tools/winres.rs");
    embed_resources("assets/icons/typeflow.ico", None);
}
