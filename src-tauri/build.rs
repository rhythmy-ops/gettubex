fn main() {
    println!("cargo:rerun-if-env-changed=GETTUBE_UPDATE_REPO");
    tauri_build::build()
}
