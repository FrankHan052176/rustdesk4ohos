fn main() {
    println!("cargo:rustc-check-cfg=cfg(dsh_windows_typecheck)");
    println!("cargo:rerun-if-changed=native/windows-capture/rd_desktop_duplication.cpp");
    println!("cargo:rerun-if-changed=native/windows-capture/rd_desktop_duplication.h");
    println!("cargo:rerun-if-changed=native/windows-nvenc/windows_nvenc.cpp");
    println!("cargo:rerun-if-changed=native/windows-nvenc/windows_nvenc.h");
    println!("cargo:rerun-if-changed=native/windows-nvenc/include/nvEncodeAPI.h");

    let windows = std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some("windows".as_ref());
    let native_default = std::env::var_os("CARGO_FEATURE_WINDOWS_MODERN_PRODUCER").is_some();
    if !windows || !native_default {
        return;
    }

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("native/windows-capture/rd_desktop_duplication.cpp")
        .file("native/windows-nvenc/windows_nvenc.cpp")
        .include("native/windows-capture")
        .include("native/windows-nvenc")
        .include("native/windows-nvenc/include")
        .compile("rd_engine_windows_native");

    for library in ["d3d11", "dxgi", "dxguid", "user32"] {
        println!("cargo:rustc-link-lib={library}");
    }
}
