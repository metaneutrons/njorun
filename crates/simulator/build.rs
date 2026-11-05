fn main() {
    slint_build::compile("ui/main.slint").expect("compile simulator Slint UI");
    println!("cargo:rerun-if-changed=ui/main.slint");
}
