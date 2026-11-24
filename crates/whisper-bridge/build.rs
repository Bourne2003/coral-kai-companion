fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // Provide clang runtime for `@available(...)` in ggml-metal-device.m when linked as cdylib.
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

        // Export ONLY our tiny C ABI so whisper.cpp's internal ggml symbols
        // stay completely isolated from llama.cpp in the host process.
        println!("cargo:rustc-cdylib-link-arg=-Wl,-exported_symbol,_soulmate_whisper_*");
    }
}
