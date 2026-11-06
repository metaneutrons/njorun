fn main() {
    embuild::espidf::sysenv::output();
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("../../crates/simulator/ui/main.slint", config)
        .expect("compile shared commissioning UI for the software renderer");
    println!("cargo:rerun-if-changed=../../crates/simulator/ui/main.slint");
    println!("cargo:rerun-if-changed=../../crates/simulator/ui/virtual_keyboard.slint");
}
