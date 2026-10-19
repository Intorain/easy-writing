fn main() {
    // A release cannot quietly package a worker for another architecture.
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let target = std::env::var("TARGET").expect("missing Rust target");
        let executable = if target.contains("windows") { "ew-agent-runtime.exe" } else { "ew-agent-runtime" };
        let worker = std::path::Path::new("binaries/agent").join(&target).join(executable);
        println!("cargo:rerun-if-changed=binaries/agent/manifest.json");
        assert!(worker.is_file(), "缺少 {target} 的 Agent 运行时，请先按该 target 运行 scripts/package-agent-runtime.py");
    }
    tauri_build::build()
}
