use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tauri_plugin_dialog::DialogExt;

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
    stream_type: MediaStreamType,
    codec_name: Option<String>,
    codec_description: Option<String>,
    title: Option<String>,
    language: Option<String>,
    default_stream: bool,
    forced: bool,
    editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum MediaStreamType {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Data,
    Unknown,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleDocument {
    content: String,
    format: String,
    codec_name: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubtitleEdit {
    stream_index: u32,
    content: String,
}

struct SubtitleServer {
    base_url: String,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SubtitleServer {
    fn start(edits: &[SubtitleEdit]) -> Result<Self, String> {
        let sources = edits
            .iter()
            .map(|edit| (format!("/{}", edit.stream_index), edit.content.clone()))
            .collect::<HashMap<_, _>>();
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|error| format!("无法创建字幕输入：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法配置字幕输入：{error}"))?;
        let base_url = format!(
            "http://{}",
            listener
                .local_addr()
                .map_err(|error| format!("无法读取字幕输入地址：{error}"))?
        );
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread = thread::Builder::new()
            .name("subtitle-input-server".to_string())
            .spawn(move || {
                while thread_running.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            let mut request = [0_u8; 2048];
                            let path = socket
                                .read(&mut request)
                                .ok()
                                .and_then(|length| std::str::from_utf8(&request[..length]).ok())
                                .and_then(|request| request.lines().next())
                                .and_then(|request_line| request_line.split_whitespace().nth(1));
                            let content = path.and_then(|path| sources.get(path));
                            let response = match content {
                                Some(content) => format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    content.len(),
                                    content
                                ),
                                None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                            };
                            let _ = socket.write_all(response.as_bytes());
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| format!("无法启动字幕输入：{error}"))?;

        Ok(Self {
            base_url,
            running,
            thread: Some(thread),
        })
    }

    fn url(&self, stream_index: u32) -> String {
        format!("{}/{stream_index}", self.base_url)
    }
}

impl Drop for SubtitleServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
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
    codec_type: MediaStreamType,
    codec_name: Option<String>,
    codec_long_name: Option<String>,
    tags: Option<StreamTags>,
    disposition: Option<Disposition>,
}

#[derive(Debug, Default, serde::Deserialize)]
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
    fn remux_subtitles(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
    ) -> Result<(), String>;
}

#[derive(Clone)]
struct CliFfmpegService;

#[cfg(feature = "embe")]
#[derive(Clone)]
struct FFIFfmpegService;

#[cfg(feature = "embe")]
type ActiveFfmpegService = FFIFfmpegService;

#[cfg(not(feature = "embe"))]
type ActiveFfmpegService = CliFfmpegService;
#[cfg(any(not(feature = "embe"), test))]
impl CliFfmpegService {
    fn new() -> Self {
        Self
    }

    fn sidecar_command(&self, sidecar: &str) -> Result<Command, String> {
        if sidecar != "ffm" {
            return Err(format!("不支持的内嵌命令：{sidecar}"));
        }

        let executable_name = if cfg!(target_os = "windows") {
           format!("{}.exe", sidecar)
        } else {
            sidecar.to_string()
        };
        let executable_dir = std::env::current_exe()
            .map_err(|error| format!("无法获取当前可执行文件路径：{error}"))?
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "无法获取当前可执行文件所在目录".to_string())?;
        let destination = executable_dir.join(executable_name);

        if !destination.is_file() {
            fs::write(&destination, include_bytes!("../binaries/ffm")).map_err(|error| {
                format!(
                    "无法释放内嵌 {} 到 {}：{error}",
                    sidecar,
                    destination.display()
                )
            })?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&destination, fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("无法设置 FFmpeg 执行权限：{error}"))?;
        }
        #[cfg(target_os = "windows")]
        let mut cmd = Command::new(destination);
        #[cfg(not(target_os = "windows"))]
        let cmd = Command::new(destination);

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            // 0x08000000 是 CREATE_NO_WINDOW 的标志值
            cmd.creation_flags(0x08000000);
        }
        Ok(cmd)
    }

    fn probe(&self, path: &Path) -> Result<ProbeResult, String> {
        let output = self
            .sidecar_command("ffm")?
            .args(["-hide_banner", "-i"])
            .arg(path)
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        // `ffmpeg -i` prints the input metadata then exits unsuccessfully because it has no output.
        // A successfully parsed input description is therefore the success condition for probing.
        if let Some(probe) = parse_ffmpeg_probe_output(&output.stderr) {
            return Ok(probe);
        }

        Err(command_error("ffmpeg", &output.stderr))
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

        if stream.codec_type != MediaStreamType::Subtitle
            || subtitle_specification(codec_name).is_none()
        {
            return Err("目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流；图形字幕会保留但不可编辑。".to_string());
        }
        Ok(stream)
    }
}

fn subtitle_specification(codec_name: &str) -> Option<(&'static str, &'static str)> {
    match codec_name {
        "subrip" | "srt" => Some(("srt", "srt")),
        "ass" | "ssa" => Some(("ass", "ass")),
        "webvtt" => Some(("webvtt", "webvtt")),
        _ => None,
    }
}

fn parse_ffmpeg_probe_output(stderr: &[u8]) -> Option<ProbeResult> {
    let stderr = String::from_utf8_lossy(stderr);
    let mut format = None;
    let mut streams = Vec::new();
    let mut current_stream = None;

    for line in stderr.lines() {
        let trimmed = line.trim();
        if let Some(duration) = trimmed.strip_prefix("Duration: ") {
            let duration = duration.split(',').next().unwrap_or_default().trim();
            format = Some(ProbeFormat {
                duration: ffmpeg_duration_seconds(duration),
            });
            continue;
        }

        if let Some(stream) = parse_ffmpeg_stream_line(trimmed) {
            streams.push(stream);
            current_stream = Some(streams.len() - 1);
            continue;
        }

        let Some(stream_index) = current_stream else {
            continue;
        };
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "title" => streams[stream_index].tags.get_or_insert_with(Default::default).title = Some(value.to_string()),
            "language" => streams[stream_index]
                .tags
                .get_or_insert_with(Default::default)
                .language = Some(value.to_string()),
            _ => {}
        }
    }

    (!streams.is_empty()).then_some(ProbeResult { streams, format })
}

fn parse_ffmpeg_stream_line(line: &str) -> Option<ProbeStream> {
    let stream = line.strip_prefix("Stream #")?;
    let (_, stream) = stream.split_once(':')?;
    let index_end = stream.find(|character: char| !character.is_ascii_digit())?;
    let index = stream[..index_end].parse().ok()?;
    let remainder = &stream[index_end..];
    let (stream_tags, description) = remainder.split_once(':')?;
    let language = stream_tags
        .rsplit_once('(')
        .and_then(|(_, language)| language.strip_suffix(')'))
        .filter(|language| !language.is_empty() && language.chars().all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_'))
        .map(str::to_string);
    let (codec_type, codec_description) = description.trim().split_once(':')?;
    let codec_description = codec_description.trim();
    let codec_name = codec_description
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .next()
        .map(str::trim)
        .filter(|codec| !codec.is_empty())
        .map(str::to_string);

    Some(ProbeStream {
        index,
        codec_type: ffmpeg_stream_type(codec_type.trim())?,
        codec_name,
        codec_long_name: Some(codec_description.to_string()),
        tags: language.map(|language| StreamTags {
            title: None,
            language: Some(language),
        }),
        disposition: Some(Disposition {
            default: Some(line.contains("(default)").into()),
            forced: Some(line.contains("(forced)").into()),
        }),
    })
}

fn ffmpeg_stream_type(value: &str) -> Option<MediaStreamType> {
    match value {
        "Video" => Some(MediaStreamType::Video),
        "Audio" => Some(MediaStreamType::Audio),
        "Subtitle" => Some(MediaStreamType::Subtitle),
        "Attachment" => Some(MediaStreamType::Attachment),
        "Data" => Some(MediaStreamType::Data),
        _ => None,
    }
}

fn ffmpeg_duration_seconds(value: &str) -> Option<String> {
    let mut parts = value.split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hours * 3600.0 + minutes * 60.0 + seconds).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        error::Error,
        fs::File,
        sync::{Mutex, MutexGuard, OnceLock},
    };

    const TEST_MKV_URL: &str = "https://github.com/inkroom/mkvtool/releases/download/resource/test.mkv";
    static FFMPEG_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn ffmpeg_test_lock() -> MutexGuard<'static, ()> {
        FFMPEG_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn test_target_dir() -> Result<PathBuf, Box<dyn Error>> {
        std::env::current_exe()?
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .ok_or_else(|| "无法定位 Cargo target 目录".into())
    }

    fn download_test_mkv() -> Result<PathBuf, Box<dyn Error>> {
        let fixture_dir = test_target_dir()?.join("ffmpeg-test-fixtures");
        let fixture = fixture_dir.join("test.mkv");
        if fixture.is_file() && fixture.metadata()?.len() > 0 {
            return Ok(fixture);
        }

        fs::create_dir_all(&fixture_dir)?;
        let temporary = fixture.with_extension(format!("mkv-{}", std::process::id()));
        let response = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build()
            .get(TEST_MKV_URL)
            .call()?;
        let mut reader = response.into_reader();
        let mut output = File::create(&temporary)?;
        std::io::copy(&mut reader, &mut output)?;
        if output.metadata()?.len() == 0 {
            return Err("下载的 FFmpeg 测试文件为空".into());
        }
        fs::rename(&temporary, &fixture)?;
        Ok(fixture)
    }

    fn first_editable_subtitle(
        service: &ActiveFfmpegService,
        input: &Path,
    ) -> Result<u32, String> {
        service
            .inspect(input)?
            .streams
            .iter()
            .find(|stream| stream.editable)
            .map(|stream| stream.index)
            .ok_or_else(|| "测试文件中没有可编辑字幕流。".to_string())
    }

    #[test]
    fn assert_inspects_downloaded_test_file() {
        let _lock = ffmpeg_test_lock();
        let input = download_test_mkv().expect("测试文件应下载到 target 目录");
        let service = ActiveFfmpegService::new();
        let media = service.inspect(&input).expect("FFmpeg 应能探测测试文件");

        assert!(!media.streams.is_empty());
        assert!(media
            .streams
            .iter()
            .any(|stream| stream.stream_type == MediaStreamType::Video));
        assert!(media
            .streams
            .iter()
            .any(|stream| stream.stream_type == MediaStreamType::Audio));
        assert!(media
            .streams
            .iter()
            .any(|stream| stream.stream_type == MediaStreamType::Subtitle));
        let subtitle = media
            .streams
            .iter()
            .find(|stream| stream.editable)
            .expect("测试文件应包含可编辑字幕流");
        assert_eq!(subtitle.codec_name.as_deref(), Some("ass"));
    }

    #[test]
    fn assert_reads_subtitle_from_downloaded_test_file() {
        let _lock = ffmpeg_test_lock();
        let input = download_test_mkv().expect("测试文件应下载到 target 目录");
        let service = ActiveFfmpegService::new();
        let ffmpeg = CliFfmpegService::new();
        let streams = service
            .inspect(&input)
            .expect("FFmpeg 应能探测测试文件")
            .streams;
        let editable_streams = streams.iter().filter(|stream| stream.editable);

        assert!(editable_streams.clone().next().is_some(), "测试文件应包含可编辑字幕流");
        for stream in editable_streams {
            let subtitle = service
                .read_subtitle(&input, stream.index)
                .expect("FFI FFmpeg 应能读取测试字幕");
            let ffmpeg_subtitle = ffmpeg
                .read_subtitle(&input, stream.index)
                .expect("FFmpeg CLI 应能读取测试字幕");

            assert_eq!(subtitle.format, ffmpeg_subtitle.format, "字幕流 #{} 格式不一致", stream.index);
            assert_eq!(subtitle.codec_name, ffmpeg_subtitle.codec_name, "字幕流 #{} 编码不一致", stream.index);
            assert_eq!(subtitle.content, ffmpeg_subtitle.content, "字幕流 #{} 文本不一致", stream.index);
        }
    }

    #[test]
    fn assert_remuxes_subtitle_from_downloaded_test_file() {
        let _lock = ffmpeg_test_lock();
        let input = download_test_mkv().expect("测试文件应下载到 target 目录");
        let service = ActiveFfmpegService::new();
        let stream_index = first_editable_subtitle(&service, &input)
            .expect("测试文件应包含可编辑字幕流");
        let subtitle = service
            .read_subtitle(&input, stream_index)
            .expect("FFmpeg 应能读取测试字幕");
        let output = test_target_dir()
            .expect("应能定位 Cargo target 目录")
            .join("ffmpeg-test-fixtures")
            .join("remuxed-test.mkv");

        service
            .remux_subtitles(
                &input,
                &output,
                &[SubtitleEdit {
                    stream_index,
                    content: subtitle.content,
                }],
            )
            .expect("FFmpeg 应能重新混流测试字幕");

        let remuxed = service.inspect(&output).expect("FFmpeg 应能探测重混流结果");
        assert_eq!(remuxed.streams.len(), service.inspect(&input).unwrap().streams.len());
    }

    #[test]
    fn parses_ffmpeg_input_description() {
        let output = br#"
Input #0, matroska,webm, from 'sample.mkv':
  Duration: 01:02:03.500, start: 0.000000, bitrate: 800 kb/s
  Stream #0:0: Video: h264 (High), yuv420p
  Stream #0:1(eng): Subtitle: ass (default) (forced)
    Metadata:
      title           : English subtitles
"#;

        let probe = parse_ffmpeg_probe_output(output).expect("FFmpeg output should parse");
        assert_eq!(probe.format.and_then(|format| format.duration), Some("3723.5".to_string()));
        assert_eq!(probe.streams.len(), 2);

        let subtitle = &probe.streams[1];
        assert_eq!(subtitle.index, 1);
        assert_eq!(subtitle.codec_type, MediaStreamType::Subtitle);
        assert_eq!(subtitle.codec_name.as_deref(), Some("ass"));
        assert_eq!(subtitle.tags.as_ref().and_then(|tags| tags.language.as_deref()), Some("eng"));
        assert_eq!(subtitle.tags.as_ref().and_then(|tags| tags.title.as_deref()), Some("English subtitles"));
        assert_eq!(subtitle.disposition.as_ref().and_then(|value| value.default), Some(1));
        assert_eq!(subtitle.disposition.as_ref().and_then(|value| value.forced), Some(1));
    }

}
#[cfg(any(not(feature = "embe"), test))]
impl FfmpegService for CliFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        let probe = self.probe(path)?;
        let streams = probe
            .streams
            .into_iter()
            .map(|stream| {
                let codec_name = stream.codec_name;
                let editable = stream.codec_type == MediaStreamType::Subtitle
                    && codec_name
                        .as_deref()
                        .and_then(subtitle_specification)
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
        let (format, _) = subtitle_specification(codec_name).unwrap();
        let output = self
            .sidecar_command("ffm")?
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", &format!("0:{stream_index}"), "-f", format, "-"])
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        if !output.status.success() {
            return Err(command_error("ffmpeg", &output.stderr));
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|_| "字幕不是 UTF-8 文本，暂时无法在编辑器中打开。".to_string())?;
        Ok(SubtitleDocument {
            content,
            format: format.to_string(),
            codec_name: codec_name.to_string(),
        })
    }

    fn remux_subtitles(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
    ) -> Result<(), String> {
        if edits.is_empty() {
            return Err("没有需要导出的字幕修改。".to_string());
        }

        let probe = self.probe(input)?;
        let mut edited_streams = HashSet::new();
        let mut formats = Vec::with_capacity(edits.len());
        for edit in edits {
            if !edited_streams.insert(edit.stream_index) {
                return Err(format!("字幕流 #{} 被重复提交。", edit.stream_index));
            }
            let stream = self.editable_stream(&probe, edit.stream_index)?;
            let codec_name = stream.codec_name.as_deref().unwrap_or_default();
            let (format, _) = subtitle_specification(codec_name).unwrap();
            formats.push(format);
        }

        let subtitle_server = SubtitleServer::start(edits)?;

        let mut command = self.sidecar_command("ffm")?;
        command
            .args(["-v", "error",  "-i"])
            .arg(input);
        for (edit, format) in edits.iter().zip(formats) {
            command
                .args(["-f", format, "-i"])
                .arg(subtitle_server.url(edit.stream_index));
        }

        // Keep each source stream in its original order, replacing edited subtitle streams in place.
        for candidate in &probe.streams {
            command.arg("-map");
            if let Some((input_index, _)) = edits
                .iter()
                .enumerate()
                .find(|(_, edit)| edit.stream_index == candidate.index)
            {
                command.arg(format!("{}:0", input_index + 1));
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
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let output_result = command
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        if !output_result.status.success() {
            return Err(command_error("ffmpeg", &output_result.stderr));
        }
        Ok(())
    }
}

#[cfg(feature = "embe")]
impl FFIFfmpegService {
    fn new() -> Self {
        Self
    }

    fn initialize() -> Result<(), String> {
        static INITIALIZATION: std::sync::OnceLock<Result<(), String>> =
            std::sync::OnceLock::new();

        INITIALIZATION
            .get_or_init(|| {
                ffmpeg_next::init().map_err(|error| format!("无法初始化 FFmpeg 库：{error}"))
            })
            .as_ref()
            .map_err(Clone::clone)
            .copied()
    }

    fn stream_type(medium: ffmpeg_next::media::Type) -> MediaStreamType {
        match medium {
            ffmpeg_next::media::Type::Video => MediaStreamType::Video,
            ffmpeg_next::media::Type::Audio => MediaStreamType::Audio,
            ffmpeg_next::media::Type::Subtitle => MediaStreamType::Subtitle,
            ffmpeg_next::media::Type::Attachment => MediaStreamType::Attachment,
            ffmpeg_next::media::Type::Data => MediaStreamType::Data,
            ffmpeg_next::media::Type::Unknown => MediaStreamType::Unknown,
        }
    }
    fn ass_header(parameters: &ffmpeg_next::codec::Parameters) -> Result<String, String> {
        let parameters = unsafe { &*parameters.as_ptr() };
        if parameters.extradata.is_null() || parameters.extradata_size <= 0 {
            return Err("ASS 字幕流缺少 Script Info 头信息。".to_string());
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                parameters.extradata,
                parameters.extradata_size as usize,
            )
        };
        let mut header = String::from_utf8(bytes.to_vec())
            .map_err(|_| "ASS 字幕头不是 UTF-8 文本，暂时无法在编辑器中打开。".to_string())?;
        let line_ending = if header.contains("\r\n") { "\r\n" } else { "\n" };
        header = header.trim_end_matches(['\0', '\r', '\n']).to_string();
        header.push_str(line_ending);
        Ok(header)
    }

    fn ass_dialogue(
        dialogue: &str,
        pts: i64,
        duration: i64,
        time_base: ffmpeg_next::Rational,
    ) -> String {
        let dialogue = dialogue
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("Dialogue: ")
            .unwrap_or(dialogue.trim_end_matches(['\r', '\n']));
        let mut fields = dialogue
            .splitn(10, ',')
            .map(str::to_string)
            .collect::<Vec<_>>();
        if fields.len() >= 3 {
            fields[1] = Self::ass_timestamp(pts, time_base);
            fields[2] = Self::ass_timestamp(pts.saturating_add(duration), time_base);
        }
        format!("Dialogue: {}", fields.join(","))
    }

    fn ass_timestamp(timestamp: i64, time_base: ffmpeg_next::Rational) -> String {
        let scaled_timestamp = timestamp
            .saturating_mul(time_base.numerator() as i64)
            .saturating_mul(100);
        let denominator = time_base.denominator() as i64;
        // `ffmpeg -f ass` uses FFmpeg's nearest-integer timestamp rescaling.
        // Matroska subtitle timestamps commonly fall on half-centiseconds.
        let centiseconds = if scaled_timestamp >= 0 {
            scaled_timestamp.saturating_add(denominator / 2) / denominator
        } else {
            scaled_timestamp.saturating_sub(denominator / 2) / denominator
        };
        let hours = centiseconds / 360_000;
        let minutes = (centiseconds / 6_000) % 60;
        let seconds = (centiseconds / 100) % 60;
        format!("{hours}:{minutes:02}:{seconds:02}.{:02}", centiseconds % 100)
    }

    fn srt_timestamp(timestamp: i64, time_base: ffmpeg_next::Rational) -> String {
        let scaled_timestamp = timestamp
            .saturating_mul(time_base.numerator() as i64)
            .saturating_mul(1_000);
        let denominator = time_base.denominator() as i64;
        let milliseconds = if scaled_timestamp >= 0 {
            scaled_timestamp.saturating_add(denominator / 2) / denominator
        } else {
            scaled_timestamp.saturating_sub(denominator / 2) / denominator
        }
        // The FFmpeg SRT muxer offsets packet boundaries by one millisecond.
        .saturating_add(1);
        let hours = milliseconds / 3_600_000;
        let minutes = (milliseconds / 60_000) % 60;
        let seconds = (milliseconds / 1_000) % 60;
        format!("{hours:02}:{minutes:02}:{seconds:02},{:03}", milliseconds % 1_000)
    }

    fn ass_text_as_html(dialogue: &str) -> String {
        let text = dialogue
            .trim_end_matches(['\r', '\n'])
            .splitn(10, ',')
            .nth(9)
            .unwrap_or(dialogue);
        let mut html = String::new();
        let mut remaining = text;

        while let Some(tag_start) = remaining.find('{') {
            html.push_str(&remaining[..tag_start]);
            let after_start = &remaining[tag_start + 1..];
            let Some(tag_end) = after_start.find('}') else {
                html.push_str(&remaining[tag_start..]);
                break;
            };
            for tag in after_start[..tag_end].split('\\').filter(|tag| !tag.is_empty()) {
                match tag {
                    "b1" => html.push_str("<b>"),
                    "b0" => html.push_str("</b>"),
                    "i1" => html.push_str("<i>"),
                    "i0" => html.push_str("</i>"),
                    "u1" => html.push_str("<u>"),
                    "u0" => html.push_str("</u>"),
                    "s1" => html.push_str("<s>"),
                    "s0" => html.push_str("</s>"),
                    "fn" | "fs" => html.push_str("</font>"),
                    tag if tag.starts_with("fn") => {
                        html.push_str("<font face=\"");
                        html.push_str(&tag[2..]);
                        html.push_str("\">");
                    }
                    tag if tag.starts_with("fs") => {
                        html.push_str("<font size=\"");
                        html.push_str(&tag[2..]);
                        html.push_str("\">");
                    }
                    _ => {}
                }
            }
            remaining = &after_start[tag_end + 1..];
        }
        html.push_str(remaining);
        html.replace("\\N", "\n").replace("\\n", "\n")
    }

}

#[cfg(feature = "embe")]
impl FfmpegService for FFIFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        use ffmpeg_next::format::stream::Disposition;

        Self::initialize()?;
        let context = ffmpeg_next::format::input(path)
            .map_err(|error| format!("无法读取媒体文件：{error}"))?;
        let streams = context
            .streams()
            .map(|stream| {
                let parameters = stream.parameters();
                let codec_id = parameters.id();
                let codec = ffmpeg_next::decoder::find(codec_id);
                let codec_name = codec
                    .as_ref()
                    .map(|codec| codec.name().to_string())
                    .or_else(|| {
                        let name = codec_id.name();
                        (name != "none").then(|| name.to_string())
                    })
                    .map(|name| {
                        subtitle_specification(&name)
                            .map(|(_, canonical_name)| canonical_name.to_string())
                            .unwrap_or(name)
                    });
                let codec_description = codec
                    .as_ref()
                    .map(|codec| codec.description().to_string())
                    .filter(|description| !description.is_empty());
                let metadata = stream.metadata();
                let disposition = stream.disposition();
                let stream_type = Self::stream_type(parameters.medium());
                let editable = stream_type == MediaStreamType::Subtitle
                    && codec_name
                        .as_deref()
                        .and_then(subtitle_specification)
                        .is_some();

                MediaStream {
                    index: stream.index() as u32,
                    stream_type,
                    codec_name,
                    codec_description,
                    title: metadata.get("title").map(str::to_string),
                    language: metadata.get("language").map(str::to_string),
                    default_stream: disposition.contains(Disposition::DEFAULT),
                    forced: disposition.contains(Disposition::FORCED),
                    editable,
                }
            })
            .collect();
        let duration = (context.duration() >= 0)
            .then(|| (context.duration() as f64 / 1_000_000.0).to_string());

        Ok(MediaFile {
            path: path.display().to_string(),
            name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("未知文件")
                .to_string(),
            duration,
            streams,
        })
    }

    fn read_subtitle(&self, path: &Path, stream_index: u32) -> Result<SubtitleDocument, String> {
        Self::initialize()?;
        
        let mut input = ffmpeg_next::format::input(path)
            .map_err(|error| format!("无法读取媒体文件：{error}"))?;
        let stream = input
            .streams()
            .find(|stream| stream.index() == stream_index as usize)
            .ok_or_else(|| "未找到所选字幕流。".to_string())?;
        let time_base = stream.time_base();
        let parameters = stream.parameters();
        let codec_name = parameters.id().name().to_string();
        let mut content = if codec_name == "ass" {
            Self::ass_header(&parameters)?
        } else {
            String::new()
        };
        let ass_uses_crlf = content.contains("\r\n");
        let (format, _) = subtitle_specification(&codec_name).ok_or_else(|| {
            "目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流。"
                .to_string()
        })?;
        let mut decoder = ffmpeg_next::codec::context::Context::from_parameters(parameters)
            .map_err(|error| format!("无法创建字幕解码器：{error}"))?
            .decoder()
            .subtitle()
            .map_err(|error| format!("无法打开字幕解码器：{error}"))?;
        let mut subtitle_number = 1;

        for (packet_stream, packet) in input.packets() {
            if packet_stream.index() != stream_index as usize {
                continue;
            }
            let mut subtitle = ffmpeg_next::Subtitle::new();
            if decoder
                .decode(&packet, &mut subtitle)
                .map_err(|error| format!("无法解码字幕：{error}"))?
            {
                for rect in subtitle.rects() {
                    match rect {
                        ffmpeg_next::subtitle::Rect::Ass(rect) => {
                            if codec_name == "ass" {
                                content.push_str(&Self::ass_dialogue(
                                    rect.get(),
                                    packet.pts().unwrap_or_default(),
                                    packet.duration(),
                                    time_base,
                                ));
                                if rect.get().ends_with("\r\n") {
                                    content.push_str("\r\n");
                                } else if rect.get().ends_with('\n') {
                                    content.push('\n');
                                }
                            } else if format == "srt" {
                                content.push_str(&format!(
                                    "{}\n{} --> {}\n{}\n\n",
                                    subtitle_number,
                                    Self::srt_timestamp(packet.pts().unwrap_or_default(), time_base),
                                    Self::srt_timestamp(
                                        packet.pts().unwrap_or_default().saturating_add(packet.duration()),
                                        time_base,
                                    ),
                                    Self::ass_text_as_html(rect.get()),
                                ));
                                subtitle_number += 1;
                            } else {
                                content.push_str(rect.get());
                            }
                            if codec_name != "ass" && !content.ends_with('\n') {
                                content.push('\n');
                            }
                        }
                        ffmpeg_next::subtitle::Rect::Text(rect) => {
                            content.push_str(rect.get());
                            content.push('\n');
                        }
                        _ => {}
                    }
                }
            }
        }

        if codec_name == "ass" && !content.is_empty() {
            content.push_str(if ass_uses_crlf { "\r\n" } else { "\n" });
        }

        if content.is_empty() {
            return Err("字幕流不包含可编辑的文本事件。".to_string());
        }
        Ok(SubtitleDocument {
            content,
            format: format.to_string(),
            codec_name,
        })
    }

    fn remux_subtitles(
        &self,
        input_path: &Path,
        output_path: &Path,
        edits: &[SubtitleEdit],
    ) -> Result<(), String> {
        Self::initialize()?;
        
        if edits.is_empty() {
            return Err("没有需要导出的字幕修改。".to_string());
        }
        let mut input = ffmpeg_next::format::input(input_path)
            .map_err(|error| format!("无法读取媒体文件：{error}"))?;
        let mut replacements = HashMap::new();
        for edit in edits {
            if replacements.insert(edit.stream_index as usize, edit).is_some() {
                return Err(format!("字幕流 #{} 被重复提交。", edit.stream_index));
            }
            let stream = input
                .streams()
                .find(|stream| stream.index() == edit.stream_index as usize)
                .ok_or_else(|| "未找到所选字幕流。".to_string())?;
            if Self::stream_type(stream.parameters().medium()) != MediaStreamType::Subtitle
                || subtitle_specification(stream.parameters().id().name()).is_none()
            {
                return Err("目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流；图形字幕会保留但不可编辑。".to_string());
            }
        }

        let mut output = ffmpeg_next::format::output(output_path)
            .map_err(|error| format!("无法创建输出文件：{error}"))?;
        output.set_metadata(input.metadata().to_owned());
        for stream in input.streams() {
            let parameters = stream.parameters();
            let context = ffmpeg_next::codec::context::Context::from_parameters(parameters)
                .map_err(|error| format!("无法复制流参数：{error}"))?;
            let mut output_stream = output
                .add_stream_with(&context)
                .map_err(|error| format!("无法创建输出流：{error}"))?;
            output_stream.set_time_base(stream.time_base());
            output_stream.set_metadata(stream.metadata().to_owned());
        }
        output
            .write_header()
            .map_err(|error| format!("无法写入 MKV 头：{error}"))?;

        let mut emitted_replacements = HashSet::new();
        for (stream, packet) in input.packets() {
            let index = stream.index();
            if let Some(edit) = replacements.get(&index) {
                if emitted_replacements.insert(index) {
                    let mut replacement = ffmpeg_next::Packet::copy(edit.content.as_bytes());
                    replacement.set_stream(index);
                    replacement.set_pts(Some(0));
                    replacement.set_dts(Some(0));
                    replacement.set_duration(1);
                    replacement
                        .write_interleaved(&mut output)
                        .map_err(|error| format!("无法写入编辑后的字幕：{error}"))?;
                }
                continue;
            }
            packet
                .write_interleaved(&mut output)
                .map_err(|error| format!("无法写入媒体数据：{error}"))?;
        }
        output
            .write_trailer()
            .map_err(|error| format!("无法完成 MKV 写入：{error}"))
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
async fn inspect_mkv(_app: tauri::AppHandle, path: String) -> Result<MediaFile, String> {
    let path = mkv_path(&path)?;
    let service = ActiveFfmpegService::new();
    tauri::async_runtime::spawn_blocking(move || service.inspect(&path))
        .await
        .map_err(|error| format!("处理媒体文件时出错：{error}"))?
}

#[tauri::command]
async fn read_subtitle(
    _app: tauri::AppHandle,
    path: String,
    stream_index: u32,
) -> Result<SubtitleDocument, String> {
    let path = mkv_path(&path)?;
    let service = ActiveFfmpegService::new();
    tauri::async_runtime::spawn_blocking(move || service.read_subtitle(&path, stream_index))
    .await
    .map_err(|error| format!("读取字幕时出错：{error}"))?
}

#[tauri::command]
async fn save_subtitles(
    _app: tauri::AppHandle,
    input_path: String,
    output_path: String,
    edits: Vec<SubtitleEdit>,
) -> Result<(), String> {
    let input = mkv_path(&input_path)?;
    let output = PathBuf::from(output_path);
    if output.as_os_str().is_empty() || output.extension().and_then(|extension| extension.to_str()) != Some("mkv") {
        return Err("输出文件必须使用 .mkv 扩展名。".to_string());
    }
    let service = ActiveFfmpegService::new();
    tauri::async_runtime::spawn_blocking(move || service.remux_subtitles(&input, &output, &edits))
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
async fn pick_output_file(app: tauri::AppHandle, suggested_name: String) -> Option<String> {
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
            save_subtitles,
            pick_mkv_file,
            pick_output_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
