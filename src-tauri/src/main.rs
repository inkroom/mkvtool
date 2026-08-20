// Prevents an additional console window for the graphical application on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

use std::path::PathBuf;

const HELP: &str = "\
将 MKV 中指定的文本字幕转换为新的字幕流。新字幕位于原有字幕流之前，第一条设为默认。\n\
\n\
用法：\n\
  mkv [选项] <输入文件>\n\
\n\
选项：\n\
  -s, --subtitle <索引>       要转换的字幕流索引；可重复，也可用逗号分隔\n\
  -f, --format <ass|srt>      新字幕格式（默认：ass）\n\
  -a, --attachment <文件>     添加字体附件；可重复\n\
      --font-name <名称>      转换为 ASS 时使用的字体名（默认：Arial）\n\
      --auto-font-name        从第一个字体附件自动读取 ASS 字体名\n\
      --font-size <大小>      转换为 ASS 时使用的字体大小（默认：26）\n\
  -o, --output <文件>         输出 MKV 路径\n\
      --no-subset              不对子集化字体附件\n\
  -h, --help                   显示本帮助\n\
  -v, --version                显示版本信息\n";

fn print_version() {
    println!("mkv {BUILD_PACKAGE_VERSION}");
    println!("git: {BUILD_GIT_HASH}");
    println!("rust: {BUILD_RUST_VERSION}");
}

fn parse_indices(value: &str, indices: &mut Vec<u32>) -> Result<(), String> {
    for index in value.split(',') {
        let index = index.trim();
        if index.is_empty() {
            return Err("字幕流索引不能为空。".to_string());
        }
        indices.push(
            index
                .parse::<u32>()
                .map_err(|_| format!("无效的字幕流索引：{index}"))?,
        );
    }
    Ok(())
}

fn default_output(input: &PathBuf, format: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("output");
    input.with_file_name(format!("{stem}-converted-{format}.mkv"))
}

fn take_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{option} 需要一个参数。"))
}

fn run_command_line(arguments: Vec<String>) -> Result<PathBuf, String> {
    let mut input = None;
    let mut attachments = Vec::new();
    let mut subtitle_stream_indices = Vec::new();
    let mut target_format = "ass".to_string();
    let mut font_name = None;
    let mut automatic_font_name = false;
    let mut font_size = 26;
    let mut output = None;
    let mut subset_fonts = true;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-s" | "--subtitle" => parse_indices(
                &take_value(&mut arguments, &argument)?,
                &mut subtitle_stream_indices,
            )?,
            "-f" | "--format" => target_format = take_value(&mut arguments, &argument)?,
            "-a" | "--attachment" => {
                attachments.push(PathBuf::from(take_value(&mut arguments, &argument)?))
            }
            "--font-name" => font_name = Some(take_value(&mut arguments, &argument)?),
            "--auto-font-name" => automatic_font_name = true,
            "--font-size" => {
                let value = take_value(&mut arguments, &argument)?;
                font_size = value
                    .parse()
                    .map_err(|_| format!("无效的字体大小：{value}"))?;
            }
            "-o" | "--output" => {
                output = Some(PathBuf::from(take_value(&mut arguments, &argument)?))
            }
            "--no-subset" => subset_fonts = false,
            "-h" | "--help" => return Err(HELP.to_string()),
            "-v" | "--version" => return Err("__VERSION__".to_string()),
            _ if argument.starts_with('-') => {
                return Err(format!("未知选项：{argument}\n\n{HELP}"))
            }
            _ => {
                if input.replace(PathBuf::from(argument)).is_some() {
                    return Err("只能指定一个输入文件。".to_string());
                }
            }
        }
    }
    let input = input.ok_or_else(|| format!("缺少输入文件。\n\n{HELP}"))?;
    let target_format = target_format.to_ascii_lowercase();
    let output = output.unwrap_or_else(|| default_output(&input, &target_format));
    mkv_lib::convert_subtitle_streams_for_cli(mkv_lib::CliSubtitleConversionOptions {
        input,
        attachments,
        subtitle_stream_indices,
        target_format,
        font_name,
        automatic_font_name,
        font_size,
        output: output.clone(),
        subset_fonts,
    })?;
    Ok(output)
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.is_empty() {
        mkv_lib::run();
        return;
    }
    if arguments
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        print!("{HELP}");
        return;
    }
    if arguments
        .iter()
        .any(|argument| argument == "-v" || argument == "--version")
    {
        print_version();
        return;
    }
    match run_command_line(arguments) {
        Ok(output) => println!("已创建：{}", output.display()),
        Err(error) => {
            eprintln!("错误：{error}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{Mutex, OnceLock},
    };

    const RESOURCE_URL: &str = "https://github.com/inkroom/mkvtool/releases/download/resource/";

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("测试锁不应中毒")
    }

    fn resource(name: &str) -> PathBuf {
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("cli-test-fixtures");
        fs::create_dir_all(&directory).expect("应能创建 CLI 测试资源目录");
        let path = directory.join(name);
        if !path.is_file() {
            let response = ureq::get(&format!("{RESOURCE_URL}{name}"))
                .call()
                .expect("应能下载 CLI 测试资源");
            let mut reader = response.into_reader();
            let mut file = fs::File::create(&path).expect("应能创建测试资源文件");
            std::io::copy(&mut reader, &mut file).expect("应能保存测试资源");
        }
        path
    }

    #[test]
    fn runs_command_line_with_font_attachment_and_automatic_font_name() {
        let _lock = test_lock();
        let input = resource("test2.mkv");
        let font = resource("BlackSugarPlumCandy-Bold.ttf");
        let output = input.with_file_name("cli-converted-test2.mkv");
        let _ = fs::remove_file(&output);

        let result = run_command_line(vec![
            "--subtitle".to_string(),
            "2,3".to_string(),
            "--attachment".to_string(),
            font.display().to_string(),
            "--auto-font-name".to_string(),
            "--output".to_string(),
            output.display().to_string(),
            input.display().to_string(),
        ])
        .expect("命令行应能转换 test2.mkv 并添加字体附件");

        assert_eq!(result, output);
        assert!(
            fs::metadata(&output).expect("CLI 应创建输出文件").len() > 0,
            "输出文件不应为空"
        );
    }
}
