include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match mkv_lib::run_command_line(arguments) {
        Ok(result) => mkv_lib::print_command_line_result(
            result,
            "mkv-cli",
            BUILD_PACKAGE_VERSION,
            BUILD_GIT_HASH,
            BUILD_RUST_VERSION,
        ),
        Err(error) => {
            mkv_lib::print_command_line_error(&error, "mkv-cli");
        }
    }
}
