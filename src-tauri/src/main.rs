// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

fn main() {
    if std::env::args().skip(1).any(|argument| argument == "-v") {
        println!("mkv {BUILD_PACKAGE_VERSION}");
        println!("git: {BUILD_GIT_HASH}");
        println!("rust: {BUILD_RUST_VERSION}");
        return;
    }
    mkv_lib::run()
}
