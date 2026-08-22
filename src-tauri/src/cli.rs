include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

use mkv_lib::CommandLineResult;

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match mkv_lib::run_command_line(arguments) {
        Ok(CommandLineResult::Help) => print!("{}", mkv_lib::command_line_help("mkv-cli")),
        Ok(CommandLineResult::Version) => println!(
            "{}",
            mkv_lib::command_line_version(
                BUILD_PACKAGE_VERSION,
                BUILD_GIT_HASH,
                BUILD_RUST_VERSION
            )
        ),
        Ok(CommandLineResult::Converted(output)) => {
            println!("已创建：{}", output.display())
        }
        Err(error) => {
            eprintln!("错误：{error}\n\n{}", mkv_lib::command_line_help("mkv-cli"));
            std::process::exit(2);
        }
    }
}
