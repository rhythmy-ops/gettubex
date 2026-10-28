fn main() {
    println!("cargo:rerun-if-env-changed=GETTUBE_UPDATE_REPO");
    let engine = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../engines/hongguo");
    let mut assets = Vec::new();
    for name in [
        "server.cjs",
        "core.cjs",
        "package.json",
        "NOTICE",
        "UPSTREAM-NOTICE",
        "vendor",
        "node_modules",
    ] {
        collect(&engine, &engine.join(name), &mut assets);
    }
    assert!(
        assets
            .iter()
            .any(|(p, _)| p == "node_modules/axios/package.json"),
        "请先执行 npm run setup:hongguo"
    );
    assets.sort();
    let mut generated = String::from("const ENGINE_ASSETS: &[(&str, &[u8])] = &[\n");
    for (relative, file) in assets {
        println!("cargo:rerun-if-changed={}", file.display());
        generated.push_str(&format!("({relative:?}, include_bytes!({:?})),\n", file));
    }
    generated.push_str("];\n");
    std::fs::write(
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("hongguo_assets.rs"),
        generated,
    )
    .unwrap();
    tauri_build::build()
}

fn collect(
    root: &std::path::Path,
    path: &std::path::Path,
    files: &mut Vec<(String, std::path::PathBuf)>,
) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if meta.is_dir() {
        let name = path.file_name().unwrap().to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "test" | "tests" | "docs" | "examples")
        {
            return;
        }
        for entry in std::fs::read_dir(path).unwrap().flatten() {
            collect(root, &entry.path(), files);
        }
    } else {
        files.push((
            path.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/"),
            path.to_path_buf(),
        ));
    }
}
