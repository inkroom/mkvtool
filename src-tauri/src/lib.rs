use serde::Serialize;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_shell::ShellExt;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaFile {
    path: String,
    name: String,
    duration: Option<String>,
    streams: Vec<MediaStream>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MediaStream {
    index: u32,
    stream_type: String,
    codec_name: Option<String>,
    codec_description: Option<String>,
    title: Option<String>,
    language: Option<String>,
    default_stream: bool,
    forced: bool,
    editable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleDocument {
    content: String,
    format: String,
    codec_name: String,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeResult {
    streams: Vec<ProbeStream>,
    format: Option<ProbeFormat>,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeFormat {
    duration: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ProbeStream {
    index: u32,
    codec_type: String,
    codec_name: Option<String>,
    codec_long_name: Option<String>,
    tags: Option<StreamTags>,
    disposition: Option<Disposition>,
}

#[derive(Debug, serde::Deserialize)]
struct StreamTags {
    title: Option<String>,
    language: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Disposition {
    default: Option<u8>,
    forced: Option<u8>,
}

trait FfmpegService: Send + Sync {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String>;
    fn read_subtitle(&self, path: &Path, stream_index: u32) -> Result<SubtitleDocument, String>;
    fn remux_subtitle(
        &self,
        input: &Path,
        output: &Path,
        stream_index: u32,
        content: &str,
    ) -> Result<(), String>;
}

#[derive(Clone)]
struct SystemFfmpegService {
    app: tauri::AppHandle,
}

impl SystemFfmpegService {
    fn new(app: tauri::AppHandle) -> Self {
        Self { app }
    }

    fn sidecar_command(&self, sidecar: &str) -> Result<Command, String> {
        self.app
            .shell()
            .sidecar(sidecar)
            .map(Into::into)
            .map_err(|error| format!("无法启动 FFmpeg sidecar：{error}"))
    }

    fn probe(&self, path: &Path) -> Result<ProbeResult, String> {
        let output = self
            .sidecar_command("ffprobe")?
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .map_err(|error| format!("无法启动 ffprobe，请确认已安装 FFmpeg：{error}"))?;

        if !output.status.success() {
            return Err(command_error("ffprobe", &output.stderr));
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("无法读取 ffprobe 的结果：{error}"))
    }

    fn subtitle_specification(codec_name: &str) -> Option<(&'static str, &'static str)> {
        match codec_name {
            "subrip" | "srt" => Some(("srt", "srt")),
            "ass" | "ssa" => Some(("ass", "ass")),
            "webvtt" => Some(("webvtt", "webvtt")),
            _ => None,
        }
    }

    fn editable_stream<'a>(
        &self,
        probe: &'a ProbeResult,
        stream_index: u32,
    ) -> Result<&'a ProbeStream, String> {
        let stream = probe
            .streams
            .iter()
            .find(|stream| stream.index == stream_index)
            .ok_or_else(|| "未找到所选字幕流。".to_string())?;
        let codec_name = stream
            .codec_name
            .as_deref()
            .ok_or_else(|| "所选字幕流没有可识别的编码。".to_string())?;

        if stream.codec_type != "subtitle" || Self::subtitle_specification(codec_name).is_none() {
            return Err("目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流；图形字幕会保留但不可编辑。".to_string());
        }
        Ok(stream)
    }
}

impl FfmpegService for SystemFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        let probe = self.probe(path)?;
        let streams = probe
            .streams
            .into_iter()
            .map(|stream| {
                let codec_name = stream.codec_name;
                let editable = stream.codec_type == "subtitle"
                    && codec_name
                        .as_deref()
                        .and_then(Self::subtitle_specification)
                        .is_some();
                let tags = stream.tags;
                let disposition = stream.disposition.unwrap_or_default();
                MediaStream {
                    index: stream.index,
                    stream_type: stream.codec_type,
                    codec_name,
                    codec_description: stream.codec_long_name,
                    title: tags.as_ref().and_then(|tags| tags.title.clone()),
                    language: tags.and_then(|tags| tags.language),
                    default_stream: disposition.default.unwrap_or(0) != 0,
                    forced: disposition.forced.unwrap_or(0) != 0,
                    editable,
                }
            })
            .collect();

        Ok(MediaFile {
            path: path.display().to_string(),
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("未知文件")
                .to_string(),
            duration: probe.format.and_then(|format| format.duration),
            streams,
        })
    }

    fn read_subtitle(&self, path: &Path, stream_index: u32) -> Result<SubtitleDocument, String> {
        let probe = self.probe(path)?;
        let stream = self.editable_stream(&probe, stream_index)?;
        let codec_name = stream.codec_name.as_deref().unwrap_or_default();
        let (format, _) = Self::subtitle_specification(codec_name).unwrap();
        let output = self
            .sidecar_command("ffmpeg")?
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", &format!("0:{stream_index}"), "-f", format, "-"])
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        if !output.status.success() {
            return Err(command_error("ffmpeg", &output.stderr));
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|_| "字幕不是 UTF-8 文本，暂时无法在内存编辑器中打开。".to_string())?;
        Ok(SubtitleDocument {
            content,
            format: format.to_string(),
            codec_name: codec_name.to_string(),
        })
    }

    fn remux_subtitle(
        &self,
        input: &Path,
        output: &Path,
        stream_index: u32,
        content: &str,
    ) -> Result<(), String> {
        let probe = self.probe(input)?;
        let stream = self.editable_stream(&probe, stream_index)?;
        let codec_name = stream.codec_name.as_deref().unwrap_or_default();
        let (format, _) = Self::subtitle_specification(codec_name).unwrap();

        let mut command = self.sidecar_command("ffmpeg")?;
        command
            .args(["-v", "error",  "-i"])
            .arg(input)
            .args(["-f", format, "-i", "pipe:0"]);

        // Map each source stream in its original order, replacing only the edited subtitle.
        for candidate in &probe.streams {
            command.arg("-map");
            if candidate.index == stream_index {
                command.arg("1:0");
            } else {
                command.arg(format!("0:{}", candidate.index));
            }
        }

        command.args(["-map_metadata", "0"]);
        for (output_index, candidate) in probe.streams.iter().enumerate() {
            command
                .arg(format!("-map_metadata:s:{output_index}"))
                .arg(format!("0:s:{}", candidate.index));
        }

        command
            .args(["-map_chapters", "0", "-c", "copy"])
            .arg("-y")
            .arg(output)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "无法向 ffmpeg 写入内存字幕。".to_string())?
            .write_all(content.as_bytes())
            .map_err(|error| format!("写入字幕数据失败：{error}"))?;
        let output_result = child
            .wait_with_output()
            .map_err(|error| format!("等待 ffmpeg 完成时出错：{error}"))?;

        if !output_result.status.success() {
            return Err(command_error("ffmpeg", &output_result.stderr));
        }
        Ok(())
    }
}

fn command_error(command: &str, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    if detail.is_empty() {
        format!("{command} 执行失败。")
    } else {
        format!("{command} 执行失败：{detail}")
    }
}

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
async fn inspect_mkv(app: tauri::AppHandle, path: String) -> Result<MediaFile, String> {
    let path = mkv_path(&path)?;
    let service = SystemFfmpegService::new(app);
    tauri::async_runtime::spawn_blocking(move || service.inspect(&path))
        .await
        .map_err(|error| format!("处理媒体文件时出错：{error}"))?
}

#[tauri::command]
async fn read_subtitle(
    app: tauri::AppHandle,
    path: String,
    stream_index: u32,
) -> Result<SubtitleDocument, String> {
    let path = mkv_path(&path)?;
    let service = SystemFfmpegService::new(app);
    tauri::async_runtime::spawn_blocking(move || service.read_subtitle(&path, stream_index))
    .await
    .map_err(|error| format!("读取字幕时出错：{error}"))?
}

#[tauri::command]
async fn save_subtitle(
    app: tauri::AppHandle,
    input_path: String,
    output_path: String,
    stream_index: u32,
    content: String,
) -> Result<(), String> {
    let input = mkv_path(&input_path)?;
    let output = PathBuf::from(output_path);
    if output.as_os_str().is_empty() || output.extension().and_then(|extension| extension.to_str()) != Some("mkv") {
        return Err("输出文件必须使用 .mkv 扩展名。".to_string());
    }
    let service = SystemFfmpegService::new(app);
    tauri::async_runtime::spawn_blocking(move || {
        service.remux_subtitle(&input, &output, stream_index, &content)
    })
    .await
    .map_err(|error| format!("重新混流时出错：{error}"))?
}

#[tauri::command]
fn pick_mkv_file(app: tauri::AppHandle) -> Option<String> {
    println!("pick_mkv_file");
    app.dialog()
        .file()
        .add_filter("Matroska 视频", &["mkv"])
        .blocking_pick_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

#[tauri::command]
fn pick_output_file(app: tauri::AppHandle, suggested_name: String) -> Option<String> {
    app.dialog()
        .file()
        .add_filter("Matroska 视频", &["mkv"])
        .set_file_name(&suggested_name)
        .blocking_save_file()
        .and_then(|file| file.into_path().ok())
        .map(|path| path.display().to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            inspect_mkv,
            read_subtitle,
            save_subtitle,
            pick_mkv_file,
            pick_output_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
