//! Compiles the GLSL shaders to SPIR-V with `glslc` from the Vulkan SDK and
//! builds the vendored stb_truetype.
use std::{env, path::PathBuf, process::Command};

const SHADERS: [&str; 4] = ["shape.vert", "shape.frag", "text.vert", "text.frag"];

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let glslc = env::var("VULKAN_SDK")
        .map(|sdk| PathBuf::from(sdk).join("bin").join("glslc"))
        .unwrap_or_else(|_| "glslc".into());
    for name in SHADERS {
        println!("cargo:rerun-if-changed=shaders/{name}");
        let status = Command::new(&glslc)
            .arg(format!("shaders/{name}"))
            .arg("-o")
            .arg(out.join(format!("{name}.spv")))
            .status()
            .expect("glslc not found: install the Vulkan SDK or put glslc on PATH");
        assert!(status.success(), "failed to compile shaders/{name}");
    }

    println!("cargo:rerun-if-changed=thirdparty/stb_truetype.h");
    println!("cargo:rerun-if-changed=thirdparty/stb_truetype_impl.c");
    cc::Build::new()
        .file("thirdparty/stb_truetype_impl.c")
        .include("thirdparty")
        .warnings(false)
        .compile("stb_truetype");
}
