//! Build script: forwards the ESP-IDF build environment to the linker.

fn main() {
    // Exports the ESP-IDF build environment (linker args, cfgs) produced by `esp-idf-sys`.
    embuild::espidf::sysenv::output();
}
