// Prevents an additional console window for the GUI on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

include!(concat!(env!("OUT_DIR"), "/build_info.rs"));

use mkv_lib::{CommandLineResult, FontAttachment, SubtitleEdit};
use std::path::PathBuf;
use tauri_plugin_dialog::DialogExt;

fn mkv_path(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    if !path.is_file() {
        return Err("所选路径不是可读取的文件。".to_string());
    }
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mkv"))
    {
        return Err("请选择 MKV 文件。".to_string());
    }
    Ok(path)
}

#[tauri::command]
async fn inspect_mkv(_app: tauri::AppHandle, path: String) -> Result<mkv_lib::MediaFile, String> {
    let path = mkv_path(&path)?;
    tauri::async_runtime::spawn_blocking(move || mkv_lib::inspect_mkv_path(&path))
        .await
        .map_err(|error| format!("处理媒体文件时出错：{error}"))?
}

#[tauri::command]
async fn read_subtitle(
    _app: tauri::AppHandle,
    path: String,
    stream_index: u32,
) -> Result<mkv_lib::SubtitleDocument, String> {
    let path = mkv_path(&path)?;
    tauri::async_runtime::spawn_blocking(move || mkv_lib::read_subtitle_path(&path, stream_index))
        .await
        .map_err(|error| format!("读取字幕时出错：{error}"))?
}

#[tauri::command]
async fn convert_subtitle_format(
    content: String,
    source_format: String,
    target_format: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        mkv_lib::convert_subtitle_format(content, &source_format, &target_format)
    })
    .await
    .map_err(|error| format!("转换字幕格式时出错：{error}"))?
}

#[tauri::command]
async fn save_subtitles(
    _app: tauri::AppHandle,
    input_path: String,
    output_path: String,
    edits: Vec<SubtitleEdit>,
    selected_stream_indices: Vec<u32>,
    default_subtitle_stream_index: Option<u32>,
    font_attachments: Vec<FontAttachment>,
    subset_fonts: bool,
    subtitle_text: Option<String>,
) -> Result<(), String> {
    let input = mkv_path(&input_path)?;
    let output = PathBuf::from(output_path);
    tauri::async_runtime::spawn_blocking(move || {
        mkv_lib::save_subtitles_path(
            &input,
            &output,
            &edits,
            &selected_stream_indices,
            default_subtitle_stream_index,
            font_attachments,
            subset_fonts,
            subtitle_text,
        )
    })
    .await
    .map_err(|error| format!("重新混流时出错：{error}"))?
}

#[tauri::command]
async fn pick_mkv_file(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("Matroska 视频", &["mkv"])
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

#[tauri::command]
async fn read_font_name(path: String) -> Option<String> {
    mkv_lib::read_font_name(&PathBuf::from(path))
}

#[tauri::command]
async fn pick_font_file(app: tauri::AppHandle) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("字体文件", &["ttf", "otf", "ttc"])
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

#[tauri::command]
async fn pick_output_file(app: tauri::AppHandle, suggested_name: String) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("Matroska 视频", &["mkv"])
        .set_file_name(&suggested_name)
        .blocking_save_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if !arguments.is_empty() {
        match mkv_lib::run_command_line(arguments) {
            Ok(CommandLineResult::Help) => print!("{}", mkv_lib::command_line_help("mkv")),
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
                eprintln!("错误：{error}\n\n{}", mkv_lib::command_line_help("mkv"));
                std::process::exit(2);
            }
        }
        return;
    }

    run_gui();
}

fn run_gui() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            inspect_mkv,
            read_subtitle,
            convert_subtitle_format,
            save_subtitles,
            pick_mkv_file,
            read_font_name,
            pick_font_file,
            pick_output_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
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

        let result = mkv_lib::run_command_line(vec![
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

        assert!(matches!(
            result,
            CommandLineResult::Converted(path) if path == output
        ));
        assert!(
            fs::metadata(&output).expect("CLI 应创建输出文件").len() > 0,
            "输出文件不应为空"
        );
    }
}
