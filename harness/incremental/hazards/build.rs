fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-env=HAZ_BUILD=7");
}
