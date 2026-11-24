fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Apple clang's runtime archive provides `___isPlatformVersionAtLeast`
        // required by `@available(...)` checks inside ggml-metal-device.m when
        // rustc links release binaries with `-nodefaultlibs`.
        if let Ok(output) = std::process::Command::new("clang")
            .arg("--print-file-name=libclang_rt.osx.a")
            .output()
            && output.status.success()
        {
            let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let rt_path = std::path::Path::new(&raw);
            if rt_path.is_file()
                && let Some(dir) = rt_path.parent()
            {
                println!("cargo:rustc-link-search=native={}", dir.display());
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }

    tauri_build::build()
}
