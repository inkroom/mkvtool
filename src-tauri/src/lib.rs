use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
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
    filename: Option<String>,
    language: Option<String>,
    default_stream: bool,
    forced: bool,
    editable: bool,
    subtitle: Option<SubtitleDocument>,
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

#[derive(Debug, Clone, Serialize)]
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
    format: Option<String>,
    language: Option<String>,
    title: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct FontAttachment {
    path: String,
    #[serde(skip)]
    content: Option<Vec<u8>>,
}

trait FfmpegService: Send + Sync {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String>;
    fn read_subtitle(&self, path: &Path, stream_index: u32) -> Result<SubtitleDocument, String>;
    fn remux_subtitles(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
    ) -> Result<(), String> {
        let streams = self.inspect(input)?.streams;
        let default_subtitle_stream_index = streams
            .iter()
            .find(|stream| stream.stream_type == MediaStreamType::Subtitle && stream.default_stream)
            .map(|stream| stream.index);
        let selected_stream_indices = streams
            .into_iter()
            .map(|stream| stream.index)
            .collect::<Vec<_>>();
        self.remux_selected_streams(
            input,
            output,
            edits,
            &selected_stream_indices,
            default_subtitle_stream_index,
            &[],
        )
    }
    fn remux_selected_streams(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
        selected_stream_indices: &[u32],
        default_subtitle_stream_index: Option<u32>,
        font_attachments: &[FontAttachment],
    ) -> Result<(), String>;
}

enum ActiveFfmpegService {
    Ffi(FFIFfmpegService),
    Cli(CliFfmpegService),
}

impl ActiveFfmpegService {
    fn new() -> Self {
        CliFfmpegService::detect()
            .map(Self::Cli)
            .unwrap_or_else(|| Self::Ffi(FFIFfmpegService::new()))
    }
}

impl FfmpegService for ActiveFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        match self {
            Self::Ffi(service) => service.inspect(path),
            Self::Cli(service) => service.inspect(path),
        }
    }

    fn read_subtitle(&self, path: &Path, stream_index: u32) -> Result<SubtitleDocument, String> {
        match self {
            Self::Ffi(service) => service.read_subtitle(path, stream_index),
            Self::Cli(service) => service.read_subtitle(path, stream_index),
        }
    }

    fn remux_selected_streams(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
        selected_stream_indices: &[u32],
        default_subtitle_stream_index: Option<u32>,
        font_attachments: &[FontAttachment],
    ) -> Result<(), String> {
        match self {
            Self::Ffi(service) => service.remux_selected_streams(
                input,
                output,
                edits,
                selected_stream_indices,
                default_subtitle_stream_index,
                font_attachments,
            ),
            Self::Cli(service) => service.remux_selected_streams(
                input,
                output,
                edits,
                selected_stream_indices,
                default_subtitle_stream_index,
                font_attachments,
            ),
        }
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

fn subtitle_codec_id(format: &str) -> Option<ffmpeg_next::ffi::AVCodecID> {
    match format {
        "srt" => Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_SUBRIP),
        "ass" => Some(ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_ASS),
        _ => None,
    }
}

fn canonical_codec_name(codec_name: &str) -> String {
    subtitle_specification(codec_name)
        .map(|(_, canonical_name)| canonical_name.to_string())
        .unwrap_or_else(|| codec_name.to_string())
}

fn normalize_srt_line_endings(content: String) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
}

fn font_attachment_data(font: &FontAttachment) -> Result<(String, &'static str, Vec<u8>), String> {
    let path = Path::new(&font.path);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .ok_or_else(|| "字体文件必须使用 .ttf、.ttc 或 .otf 扩展名。".to_string())?;
    let mime_type = match extension.as_str() {
        "ttf" | "ttc" => "application/x-truetype-font",
        "otf" => "application/vnd.ms-opentype",
        _ => return Err("字体文件必须使用 .ttf、.ttc 或 .otf 扩展名。".to_string()),
    };
    let filename = path
        .file_name()
        .and_then(|filename| filename.to_str())
        .filter(|filename| !filename.is_empty())
        .ok_or_else(|| "无法确定字体文件名。".to_string())?
        .to_string();
    let content = match &font.content {
        Some(content) => content.clone(),
        None => fs::read(path).map_err(|error| format!("无法读取字体文件 {filename}：{error}"))?,
    };
    if content.is_empty() {
        return Err(format!("字体文件 {filename} 为空。"));
    }
    Ok((filename, mime_type, content))
}

fn subtitle_text_for_font_subset(
    service: &impl FfmpegService,
    input: &Path,
    edits: &[SubtitleEdit],
) -> Result<String, String> {
    let media = service.inspect(input)?;
    let mut text = String::new();
    for stream in media.streams.iter().filter(|stream| stream.editable) {
        if let Some(edit) = edits.iter().find(|edit| edit.stream_index == stream.index) {
            text.push_str(&edit.content);
        } else if let Some(subtitle) = &stream.subtitle {
            text.push_str(&subtitle.content);
        } else {
            text.push_str(&service.read_subtitle(input, stream.index)?.content);
        }
        text.push('\n');
    }
    Ok(text)
}

fn prepare_font_attachments(
    service: &impl FfmpegService,
    input: &Path,
    edits: &[SubtitleEdit],
    font_attachments: &mut [FontAttachment],
    subset_fonts: bool,
) -> Result<(), String> {
    if !subset_fonts || font_attachments.is_empty() {
        return Ok(());
    }

    let text = subtitle_text_for_font_subset(service, input, edits)?;
    for font in font_attachments {
        font.content = Some(
            font::subset_text_from_path(&font.path, &text)
                .ok_or_else(|| format!("无法子集化字体文件 {}。", font.path))?,
        );
    }
    Ok(())
}

#[derive(Clone)]
struct FFIFfmpegService;

struct FfiSubtitleReader {
    stream_index: usize,
    document: SubtitleDocument,
    ass_uses_crlf: bool,
    time_base: ffmpeg_next::Rational,
    decoder: ffmpeg_next::decoder::Subtitle,
    subtitle_number: usize,
}

impl FFIFfmpegService {
    fn new() -> Self {
        Self
    }

    fn initialize() -> Result<(), String> {
        static INITIALIZATION: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();

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
            std::slice::from_raw_parts(parameters.extradata, parameters.extradata_size as usize)
        };
        let mut header = String::from_utf8(bytes.to_vec())
            .map_err(|_| "ASS 字幕头不是 UTF-8 文本，暂时无法在编辑器中打开。".to_string())?;
        let line_ending = if header.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        header = header.trim_end_matches(['\0', '\r', '\n']).to_string();
        header.push_str(line_ending);
        Ok(header)
    }

    fn ass_document_parts(content: &str) -> Result<(&str, &str), String> {
        let dialogue_start = content
            .find("Dialogue:")
            .ok_or_else(|| "ASS 字幕缺少 Dialogue 事件。".to_string())?;
        let header = content[..dialogue_start].trim_end_matches(['\r', '\n']);
        if !header.trim_start().starts_with("[Script Info]") || !header.contains("[Events]") {
            return Err("ASS 字幕缺少 Script Info 头信息。".to_string());
        }
        Ok((header, &content[dialogue_start..]))
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
        format!(
            "{hours}:{minutes:02}:{seconds:02}.{:02}",
            centiseconds % 100
        )
    }

    fn ass_packet_timestamp(
        timestamp: &str,
        time_base: ffmpeg_next::Rational,
    ) -> Result<i64, String> {
        let (hours, minutes, seconds_and_centiseconds) = timestamp
            .trim()
            .split_once(':')
            .and_then(|(hours, remainder)| {
                remainder
                    .split_once(':')
                    .map(|(minutes, seconds)| (hours, minutes, seconds))
            })
            .ok_or_else(|| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        let (seconds, centiseconds) = seconds_and_centiseconds
            .split_once('.')
            .ok_or_else(|| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        let hours = hours
            .parse::<i64>()
            .map_err(|_| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        let minutes = minutes
            .parse::<i64>()
            .map_err(|_| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        let seconds = seconds
            .parse::<i64>()
            .map_err(|_| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        let centiseconds = centiseconds
            .parse::<i64>()
            .map_err(|_| format!("ASS 字幕时间格式无效：{timestamp}"))?;
        if minutes >= 60 || seconds >= 60 || centiseconds >= 100 {
            return Err(format!("ASS 字幕时间格式无效：{timestamp}"));
        }
        let total_centiseconds = hours
            .saturating_mul(360_000)
            .saturating_add(minutes.saturating_mul(6_000))
            .saturating_add(seconds.saturating_mul(100))
            .saturating_add(centiseconds);
        let numerator = time_base.numerator() as i64;
        let denominator = time_base.denominator() as i64;
        if numerator <= 0 || denominator <= 0 {
            return Err("ASS 字幕流时间基无效。".to_string());
        }
        Ok(total_centiseconds
            .saturating_mul(denominator)
            .saturating_add(50 * numerator)
            / (100 * numerator))
    }

    fn ass_packets(
        content: &str,
        time_base: ffmpeg_next::Rational,
    ) -> Result<Vec<(&str, i64, i64)>, String> {
        let mut packets = Vec::new();
        for dialogue in content.lines().filter(|line| line.starts_with("Dialogue:")) {
            let fields = dialogue
                .strip_prefix("Dialogue:")
                .unwrap_or(dialogue)
                .splitn(10, ',')
                .collect::<Vec<_>>();
            let Some(start) = fields.get(1) else {
                return Err(format!("ASS 字幕事件格式无效：{dialogue}"));
            };
            let Some(end) = fields.get(2) else {
                return Err(format!("ASS 字幕事件格式无效：{dialogue}"));
            };
            let pts = Self::ass_packet_timestamp(start, time_base)?;
            let end = Self::ass_packet_timestamp(end, time_base)?;
            if end < pts {
                return Err(format!("ASS 字幕结束时间早于开始时间：{dialogue}"));
            }
            packets.push((dialogue, pts, end.saturating_sub(pts)));
        }
        if packets.is_empty() {
            return Err("ASS 字幕不包含 Dialogue 事件。".to_string());
        }
        Ok(packets)
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
        format!(
            "{hours:02}:{minutes:02}:{seconds:02},{:03}",
            milliseconds % 1_000
        )
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
            for tag in after_start[..tag_end]
                .split('\\')
                .filter(|tag| !tag.is_empty())
            {
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

    fn read_subtitles(
        input: &mut ffmpeg_next::format::context::Input,
        stream_indices: &[u32],
    ) -> Result<HashMap<u32, SubtitleDocument>, String> {
        if stream_indices.is_empty() {
            return Ok(HashMap::new());
        }
        let requested = stream_indices
            .iter()
            .map(|index| *index as usize)
            .collect::<HashSet<_>>();
        let mut readers = HashMap::new();

        for stream in input
            .streams()
            .filter(|stream| requested.contains(&stream.index()))
        {
            let parameters = stream.parameters();
            let source_codec_name = parameters.id().name();
            let codec_name = canonical_codec_name(source_codec_name);
            let (format, _) = subtitle_specification(&codec_name)
                .ok_or_else(|| "目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流。".to_string())?;
            let content = if codec_name == "ass" {
                Self::ass_header(&parameters)?
            } else {
                String::new()
            };
            let reader = FfiSubtitleReader {
                stream_index: stream.index(),
                ass_uses_crlf: content.contains("\r\n"),
                document: SubtitleDocument {
                    content,
                    format: format.to_string(),
                    codec_name,
                },
                time_base: stream.time_base(),
                decoder: ffmpeg_next::codec::context::Context::from_parameters(parameters)
                    .map_err(|error| format!("无法创建字幕解码器：{error}"))?
                    .decoder()
                    .subtitle()
                    .map_err(|error| format!("无法打开字幕解码器：{error}"))?,
                subtitle_number: 1,
            };
            readers.insert(reader.stream_index, reader);
        }
        if readers.len() != requested.len() {
            return Err("未找到所选字幕流。".to_string());
        }

        for (packet_stream, packet) in input.packets() {
            let Some(reader) = readers.get_mut(&packet_stream.index()) else {
                continue;
            };
            let mut subtitle = ffmpeg_next::Subtitle::new();
            if !reader
                .decoder
                .decode(&packet, &mut subtitle)
                .map_err(|error| format!("无法解码字幕：{error}"))?
            {
                continue;
            }
            for rect in subtitle.rects() {
                match rect {
                    ffmpeg_next::subtitle::Rect::Ass(rect) => {
                        if reader.document.codec_name == "ass" {
                            reader.document.content.push_str(&Self::ass_dialogue(
                                rect.get(),
                                packet.pts().unwrap_or_default(),
                                packet.duration(),
                                reader.time_base,
                            ));
                            reader.document.content.push_str(if reader.ass_uses_crlf {
                                "\r\n"
                            } else {
                                "\n"
                            });
                        } else if reader.document.format == "srt" {
                            reader.document.content.push_str(&format!(
                                "{}\n{} --> {}\n{}\n\n",
                                reader.subtitle_number,
                                Self::srt_timestamp(
                                    packet.pts().unwrap_or_default(),
                                    reader.time_base
                                ),
                                Self::srt_timestamp(
                                    packet
                                        .pts()
                                        .unwrap_or_default()
                                        .saturating_add(packet.duration()),
                                    reader.time_base
                                ),
                                Self::ass_text_as_html(rect.get()),
                            ));
                            reader.subtitle_number += 1;
                        } else {
                            reader.document.content.push_str(rect.get());
                        }
                        if reader.document.codec_name != "ass"
                            && !reader.document.content.ends_with('\n')
                        {
                            reader.document.content.push('\n');
                        }
                    }
                    ffmpeg_next::subtitle::Rect::Text(rect) => {
                        reader
                            .document
                            .content
                            .push_str(rect.get().trim_end_matches(['\r', '\n']));
                        reader.document.content.push('\n');
                    }
                    _ => {}
                }
            }
        }

        let mut documents = HashMap::new();
        for reader in readers.into_values() {
            let mut document = reader.document;
            if document.format == "srt" {
                document.content = normalize_srt_line_endings(document.content);
            }
            if document.content.is_empty() {
                return Err("字幕流不包含可编辑的文本事件。".to_string());
            }
            documents.insert(reader.stream_index as u32, document);
        }
        Ok(documents)
    }
}

impl FfmpegService for FFIFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        use ffmpeg_next::format::stream::Disposition;

        Self::initialize()?;
        let mut context = ffmpeg_next::format::input(path)
            .map_err(|error| format!("无法读取媒体文件：{error}"))?;
        let mut streams = context
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
                    .map(|name| canonical_codec_name(&name));
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
                    filename: metadata.get("filename").map(str::to_string),
                    language: metadata.get("language").map(str::to_string),
                    default_stream: disposition.contains(Disposition::DEFAULT),
                    forced: disposition.contains(Disposition::FORCED),
                    editable,
                    subtitle: None,
                }
            })
            .collect::<Vec<_>>();
        let subtitle_indices = streams
            .iter()
            .filter(|stream| stream.editable)
            .map(|stream| stream.index)
            .collect::<Vec<_>>();
        let subtitles = Self::read_subtitles(&mut context, &subtitle_indices)?;
        for stream in &mut streams {
            stream.subtitle = subtitles.get(&stream.index).cloned();
        }

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
        return Self::read_subtitles(&mut input, &[stream_index]).and_then(|mut subtitles| {
            subtitles
                .remove(&stream_index)
                .ok_or_else(|| "未找到所选字幕流。".to_string())
        });
    }

    fn remux_selected_streams(
        &self,
        input_path: &Path,
        output_path: &Path,
        edits: &[SubtitleEdit],
        selected_stream_indices: &[u32],
        default_subtitle_stream_index: Option<u32>,
        font_attachments: &[FontAttachment],
    ) -> Result<(), String> {
        Self::initialize()?;

        let mut input = ffmpeg_next::format::input(input_path)
            .map_err(|error| format!("无法读取媒体文件：{error}"))?;
        let selected_streams = selected_stream_indices
            .iter()
            .map(|index| *index as usize)
            .collect::<HashSet<_>>();
        if selected_streams.is_empty() {
            return Err("请至少选择一条要导出的流。".to_string());
        }
        let mut replacements = HashMap::new();
        for edit in edits {
            if replacements
                .insert(edit.stream_index as usize, edit)
                .is_some()
            {
                return Err(format!("字幕流 #{} 被重复提交。", edit.stream_index));
            }
            let stream = input
                .streams()
                .find(|stream| stream.index() == edit.stream_index as usize)
                .ok_or_else(|| "未找到所选字幕流。".to_string())?;
            if Self::stream_type(stream.parameters().medium()) != MediaStreamType::Subtitle
                || subtitle_specification(stream.parameters().id().name()).is_none()
            {
                return Err(
                    "目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流；图形字幕会保留但不可编辑。"
                        .to_string(),
                );
            }
        }
        if let Some(default_stream_index) = default_subtitle_stream_index {
            let stream = input
                .streams()
                .find(|stream| stream.index() == default_stream_index as usize)
                .ok_or_else(|| "未找到要设为默认的字幕流。".to_string())?;
            if !selected_streams.contains(&stream.index())
                || Self::stream_type(stream.parameters().medium()) != MediaStreamType::Subtitle
            {
                return Err("默认字幕必须是要导出的字幕流。".to_string());
            }
        }

        let mut output = ffmpeg_next::format::output(output_path)
            .map_err(|error| format!("无法创建输出文件：{error}"))?;
        output.set_metadata(input.metadata().to_owned());
        let mut output_indices = HashMap::new();
        for stream in input.streams() {
            if !selected_streams.contains(&stream.index()) {
                continue;
            }
            let source_codec_name = canonical_codec_name(stream.parameters().id().name());
            let mut parameters = stream.parameters().clone();
            if let Some(format) = replacements
                .get(&stream.index())
                .and_then(|edit| edit.format.as_deref())
            {
                let codec_id = subtitle_codec_id(format)
                    .ok_or_else(|| "字幕格式只能是 ass 或 srt。".to_string())?;
                unsafe {
                    (*parameters.as_mut_ptr()).codec_id = codec_id;
                }
                if format == "ass" && source_codec_name != "ass" {
                    let (header, _) = Self::ass_document_parts(
                        &replacements
                            .get(&stream.index())
                            .expect("replacement must exist")
                            .content,
                    )?;
                    let header = header.replace("\r\n", "\n").replace('\n', "\r\n");
                    unsafe {
                        let parameters = &mut *parameters.as_mut_ptr();
                        parameters.extradata_size = header.len() as i32;
                        parameters.extradata = ffmpeg_next::ffi::av_mallocz(
                            header.len() + ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
                        ) as *mut u8;
                        if parameters.extradata.is_null() {
                            return Err("无法为 ASS 字幕头分配内存。".to_string());
                        }
                        std::ptr::copy_nonoverlapping(
                            header.as_ptr(),
                            parameters.extradata,
                            header.len(),
                        );
                    }
                }
            }
            let context = ffmpeg_next::codec::context::Context::from_parameters(parameters)
                .map_err(|error| format!("无法复制流参数：{error}"))?;
            let mut output_stream = output
                .add_stream_with(&context)
                .map_err(|error| format!("无法创建输出流：{error}"))?;
            output_stream.set_time_base(stream.time_base());
            let mut metadata = stream.metadata().to_owned();
            if let Some(language) = replacements
                .get(&stream.index())
                .and_then(|edit| edit.language.as_deref())
            {
                metadata.set("language", language);
            }
            if let Some(title) = replacements
                .get(&stream.index())
                .and_then(|edit| edit.title.as_deref())
            {
                metadata.set("title", title);
            }
            output_stream.set_metadata(metadata);
            if Self::stream_type(stream.parameters().medium()) == MediaStreamType::Subtitle {
                unsafe {
                    let output_stream = &mut *output_stream.as_mut_ptr();
                    output_stream.disposition &= !ffmpeg_next::ffi::AV_DISPOSITION_DEFAULT;
                    if default_subtitle_stream_index == Some(stream.index() as u32) {
                        output_stream.disposition |= ffmpeg_next::ffi::AV_DISPOSITION_DEFAULT;
                    }
                }
            }
            output_indices.insert(stream.index(), output_stream.index());
        }
        for font in font_attachments {
            let (filename, mime_type, content) = font_attachment_data(font)?;
            let mut parameters = ffmpeg_next::codec::Parameters::new();
            unsafe {
                let parameters = &mut *parameters.as_mut_ptr();
                parameters.codec_type = ffmpeg_next::ffi::AVMediaType::AVMEDIA_TYPE_ATTACHMENT;
                parameters.codec_id = match mime_type {
                    "application/vnd.ms-opentype" => ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_OTF,
                    _ => ffmpeg_next::ffi::AVCodecID::AV_CODEC_ID_TTF,
                };
                parameters.extradata_size = content.len() as i32;
                parameters.extradata = ffmpeg_next::ffi::av_mallocz(
                    content.len() + ffmpeg_next::ffi::AV_INPUT_BUFFER_PADDING_SIZE as usize,
                ) as *mut u8;
                if parameters.extradata.is_null() {
                    return Err("无法为字体附件分配内存。".to_string());
                }
                std::ptr::copy_nonoverlapping(
                    content.as_ptr(),
                    parameters.extradata,
                    content.len(),
                );
            }
            let mut output_stream = output
                .add_stream_with(
                    &ffmpeg_next::codec::context::Context::from_parameters(parameters)
                        .map_err(|error| format!("无法创建字体附件流：{error}"))?,
                )
                .map_err(|error| format!("无法创建字体附件流：{error}"))?;
            let mut metadata = ffmpeg_next::Dictionary::new();
            metadata.set("filename", &filename);
            metadata.set("mimetype", mime_type);
            output_stream.set_metadata(metadata);
        }
        output
            .write_header()
            .map_err(|error| format!("无法写入 MKV 头：{error}"))?;

        let mut emitted_replacements = HashSet::new();
        for (stream, packet) in input.packets() {
            let index = stream.index();
            let Some(&output_index) = output_indices.get(&index) else {
                continue;
            };
            if let Some(edit) = replacements.get(&index) {
                if emitted_replacements.insert(index) {
                    let target_format = edit.format.as_deref().unwrap_or_else(|| {
                        if canonical_codec_name(stream.parameters().id().name()) == "ass" {
                            "ass"
                        } else {
                            "srt"
                        }
                    });
                    let content = if target_format == "ass" {
                        Self::ass_document_parts(&edit.content)?.1
                    } else {
                        edit.content.as_str()
                    };
                    if target_format == "ass" {
                        for (dialogue, pts, duration) in
                            Self::ass_packets(content, stream.time_base())?
                        {
                            let mut replacement = ffmpeg_next::Packet::copy(dialogue.as_bytes());
                            replacement.set_stream(output_index);
                            replacement.set_pts(Some(pts));
                            replacement.set_dts(Some(pts));
                            replacement.set_duration(duration);
                            replacement
                                .write_interleaved(&mut output)
                                .map_err(|error| format!("无法写入编辑后的 ASS 字幕：{error}"))?;
                        }
                    } else {
                        let mut replacement = ffmpeg_next::Packet::copy(content.as_bytes());
                        replacement.set_stream(output_index);
                        replacement.set_pts(Some(0));
                        replacement.set_dts(Some(0));
                        replacement.set_duration(1);
                        replacement
                            .write_interleaved(&mut output)
                            .map_err(|error| format!("无法写入编辑后的字幕：{error}"))?;
                    }
                }
                continue;
            }
            let mut packet = packet;
            packet.set_stream(output_index);
            packet
                .write_interleaved(&mut output)
                .map_err(|error| format!("无法写入媒体数据：{error}"))?;
        }
        output
            .write_trailer()
            .map_err(|error| format!("无法完成 MKV 写入：{error}"))
    }
}

struct CliFfmpegService {
    executable: PathBuf,
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
    filename: Option<String>,
    language: Option<String>,
}
#[derive(Debug, Default, serde::Deserialize)]
struct Disposition {
    default: Option<u8>,
    forced: Option<u8>,
}

use std::process::{Command, Stdio};

impl CliFfmpegService {
    fn detect() -> Option<Self> {
        let executable_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        ["ffmpeg", "ffmpeg.exe"]
            .into_iter()
            .map(|name| executable_dir.join(name))
            .find(|path| path.is_file())
            .map(|executable| Self { executable })
    }

    fn new() -> Self {
        Self::detect().unwrap_or_else(|| Self {
            executable: PathBuf::from("ffmpeg"),
        })
    }

    fn ffmpeg_stream_type(&self, value: &str) -> Option<MediaStreamType> {
        match value {
            "Video" => Some(MediaStreamType::Video),
            "Audio" => Some(MediaStreamType::Audio),
            "Subtitle" => Some(MediaStreamType::Subtitle),
            "Attachment" => Some(MediaStreamType::Attachment),
            "Data" => Some(MediaStreamType::Data),
            _ => None,
        }
    }

    fn ffmpeg_duration_seconds(&self, value: &str) -> Option<String> {
        let mut parts = value.split(':');
        let hours: f64 = parts.next()?.parse().ok()?;
        let minutes: f64 = parts.next()?.parse().ok()?;
        let seconds: f64 = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some((hours * 3600.0 + minutes * 60.0 + seconds).to_string())
    }
    fn parse_ffmpeg_probe_output(&self, stderr: &[u8]) -> Option<ProbeResult> {
        let stderr = String::from_utf8_lossy(stderr);
        let mut format = None;
        let mut streams = Vec::new();
        let mut current_stream = None;

        for line in stderr.lines() {
            let trimmed = line.trim();
            if let Some(duration) = trimmed.strip_prefix("Duration: ") {
                let duration = duration.split(',').next().unwrap_or_default().trim();
                format = Some(ProbeFormat {
                    duration: self.ffmpeg_duration_seconds(duration),
                });
                continue;
            }

            if let Some(stream) = self.parse_ffmpeg_stream_line(trimmed) {
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
                "title" => {
                    streams[stream_index]
                        .tags
                        .get_or_insert_with(Default::default)
                        .title = Some(value.to_string())
                }
                "filename" => {
                    streams[stream_index]
                        .tags
                        .get_or_insert_with(Default::default)
                        .filename = Some(value.to_string())
                }
                "language" => {
                    streams[stream_index]
                        .tags
                        .get_or_insert_with(Default::default)
                        .language = Some(value.to_string())
                }
                _ => {}
            }
        }

        (!streams.is_empty()).then_some(ProbeResult { streams, format })
    }

    fn parse_ffmpeg_stream_line(&self, line: &str) -> Option<ProbeStream> {
        let stream = line.strip_prefix("Stream #")?;
        let (_, stream) = stream.split_once(':')?;
        let index_end = stream.find(|character: char| !character.is_ascii_digit())?;
        let index = stream[..index_end].parse().ok()?;
        let remainder = &stream[index_end..];
        let (stream_tags, description) = remainder.split_once(':')?;
        let language = stream_tags
            .rsplit_once('(')
            .and_then(|(_, language)| language.strip_suffix(')'))
            .filter(|language| {
                !language.is_empty()
                    && language.chars().all(|character| {
                        character.is_ascii_alphanumeric() || character == '-' || character == '_'
                    })
            })
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
            codec_type: self.ffmpeg_stream_type(codec_type.trim())?,
            codec_name,
            codec_long_name: Some(codec_description.to_string()),
            tags: language.map(|language| StreamTags {
                title: None,
                filename: None,
                language: Some(language),
            }),
            disposition: Some(Disposition {
                default: Some(line.contains("(default)").into()),
                forced: Some(line.contains("(forced)").into()),
            }),
        })
    }

    fn ffmpeg_command(&self) -> Command {
        let mut cmd = Command::new(&self.executable);

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            // 0x08000000 是 CREATE_NO_WINDOW 的标志值
            cmd.creation_flags(0x08000000);
        }
        cmd
    }
    fn command_error(&self, command: &str, stderr: &[u8]) -> String {
        let detail = String::from_utf8_lossy(stderr).trim().to_string();
        if detail.is_empty() {
            format!("{command} 执行失败。")
        } else {
            format!("{command} 执行失败：{detail}")
        }
    }
    fn probe(&self, path: &Path) -> Result<ProbeResult, String> {
        let output = self
            .ffmpeg_command()
            .args(["-hide_banner", "-i"])
            .arg(path)
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        // `ffmpeg -i` prints the input metadata then exits unsuccessfully because it has no output.
        // A successfully parsed input description is therefore the success condition for probing.
        if let Some(probe) = self.parse_ffmpeg_probe_output(&output.stderr) {
            return Ok(probe);
        }

        Err(self.command_error("ffmpeg", &output.stderr))
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
            return Err(
                "目前可编辑 SRT、ASS/SSA 和 WebVTT 字幕流；图形字幕会保留但不可编辑。".to_string(),
            );
        }
        Ok(stream)
    }
}

impl FfmpegService for CliFfmpegService {
    fn inspect(&self, path: &Path) -> Result<MediaFile, String> {
        let probe = self.probe(path)?;
        let streams = probe
            .streams
            .into_iter()
            .map(|stream| {
                let codec_name = stream.codec_name.map(|name| canonical_codec_name(&name));
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
                    filename: tags.as_ref().and_then(|tags| tags.filename.clone()),
                    language: tags.and_then(|tags| tags.language),
                    default_stream: disposition.default.unwrap_or(0) != 0,
                    forced: disposition.forced.unwrap_or(0) != 0,
                    editable,
                    subtitle: None,
                }
            })
            .collect::<Vec<_>>();

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
        let source_codec_name = stream.codec_name.as_deref().unwrap_or_default();
        let codec_name = canonical_codec_name(source_codec_name);
        let (format, _) = subtitle_specification(&codec_name).unwrap();
        let output = self
            .ffmpeg_command()
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-map", &format!("0:{stream_index}"), "-f", format, "-"])
            .output()
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        if !output.status.success() {
            return Err(self.command_error("ffmpeg", &output.stderr));
        }

        let content = String::from_utf8(output.stdout)
            .map_err(|_| "字幕不是 UTF-8 文本，暂时无法在编辑器中打开。".to_string())?;
        let content = if format == "srt" {
            normalize_srt_line_endings(content)
        } else {
            content
        };
        Ok(SubtitleDocument {
            content,
            format: format.to_string(),
            codec_name,
        })
    }

    fn remux_selected_streams(
        &self,
        input: &Path,
        output: &Path,
        edits: &[SubtitleEdit],
        selected_stream_indices: &[u32],
        default_subtitle_stream_index: Option<u32>,
        font_attachments: &[FontAttachment],
    ) -> Result<(), String> {
        let probe = self.probe(input)?;
        let selected_streams = selected_stream_indices
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        if selected_streams.is_empty() {
            return Err("请至少选择一条要导出的流。".to_string());
        }
        if let Some(default_stream_index) = default_subtitle_stream_index {
            if !selected_streams.contains(&default_stream_index)
                || !probe.streams.iter().any(|stream| {
                    stream.index == default_stream_index
                        && stream.codec_type == MediaStreamType::Subtitle
                })
            {
                return Err("默认字幕必须是要导出的字幕流。".to_string());
            }
        }
        let mut edited_streams = HashSet::new();
        let mut formats = Vec::with_capacity(edits.len());
        for edit in edits {
            if !edited_streams.insert(edit.stream_index) {
                return Err(format!("字幕流 #{} 被重复提交。", edit.stream_index));
            }
            let stream = self.editable_stream(&probe, edit.stream_index)?;
            let codec_name = stream.codec_name.as_deref().unwrap_or_default();
            let source_format = subtitle_specification(codec_name).unwrap().0;
            let format = edit.format.as_deref().unwrap_or(source_format);
            if subtitle_codec_id(format).is_none() {
                return Err("字幕格式只能是 ass 或 srt。".to_string());
            }
            formats.push(format);
        }

        let subtitle_server = (!edits.is_empty())
            .then(|| SubtitleServer::start(edits))
            .transpose()?;

        let mut command = self.ffmpeg_command();
        command.args(["-v", "error", "-i"]).arg(input);
        for (edit, format) in edits.iter().zip(formats) {
            command
                .args(["-f", format, "-i"])
                .arg(subtitle_server.as_ref().unwrap().url(edit.stream_index));
        }

        // Keep each source stream in its original order, replacing edited subtitle streams in place.
        for candidate in &probe.streams {
            if !selected_streams.contains(&candidate.index) {
                continue;
            }
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
        let mut subtitle_output_index = 0;
        for (output_index, candidate) in probe
            .streams
            .iter()
            .filter(|candidate| selected_streams.contains(&candidate.index))
            .enumerate()
        {
            command
                .arg(format!("-map_metadata:s:{output_index}"))
                .arg(format!("0:s:{}", candidate.index));
            if let Some(language) = edits
                .iter()
                .find(|edit| edit.stream_index == candidate.index)
                .and_then(|edit| edit.language.as_deref())
            {
                command
                    .arg(format!("-metadata:s:{output_index}"))
                    .arg(format!("language={language}"));
            }
            if let Some(title) = edits
                .iter()
                .find(|edit| edit.stream_index == candidate.index)
                .and_then(|edit| edit.title.as_deref())
            {
                command
                    .arg(format!("-metadata:s:{output_index}"))
                    .arg(format!("title={title}"));
            }
            if candidate.codec_type == MediaStreamType::Subtitle {
                command
                    .arg(format!("-disposition:s:{subtitle_output_index}"))
                    .arg(if default_subtitle_stream_index == Some(candidate.index) {
                        "+default"
                    } else {
                        "-default"
                    });
                subtitle_output_index += 1;
            }
        }

        let selected_attachment_count = probe
            .streams
            .iter()
            .filter(|stream| {
                selected_streams.contains(&stream.index)
                    && stream.codec_type == MediaStreamType::Attachment
            })
            .count();
        let mut temporary_font_files = Vec::new();
        for (font_index, font) in font_attachments.iter().enumerate() {
            let (filename, mime_type, _) = font_attachment_data(font)?;
            let attachment_path = if let Some(content) = &font.content {
                let temporary_path = std::env::temp_dir().join(format!(
                    "mkvtool-font-{}-{}-{}",
                    std::process::id(),
                    font_index,
                    filename
                ));
                fs::write(&temporary_path, content)
                    .map_err(|error| format!("无法写入临时字体文件：{error}"))?;
                temporary_font_files.push(temporary_path.clone());
                temporary_path
            } else {
                PathBuf::from(&font.path)
            };
            command.arg("-attach").arg(attachment_path);
            command
                .arg(format!(
                    "-metadata:s:t:{}",
                    selected_attachment_count + font_index
                ))
                .arg(format!("filename={filename}"));
            command
                .arg(format!(
                    "-metadata:s:t:{}",
                    selected_attachment_count + font_index
                ))
                .arg(format!("mimetype={mime_type}"));
        }

        command
            .args(["-map_chapters", "0", "-c", "copy"])
            .arg("-y")
            .arg(output)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let output_result = command.output();
        for temporary_path in temporary_font_files {
            let _ = fs::remove_file(temporary_path);
        }
        let output_result = output_result
            .map_err(|error| format!("无法启动 ffmpeg，请确认已安装 FFmpeg：{error}"))?;

        if !output_result.status.success() {
            return Err(self.command_error("ffmpeg", &output_result.stderr));
        }
        Ok(())
    }
}

mod font {
    use std::convert::TryFrom;
    use std::io::Write;
    use std::path::PathBuf;
    use std::str;

    use allsorts::gsub::{GlyphOrigin, RawGlyph, RawGlyphFlags};
    use allsorts::subset::SubsetProfile;

    use allsorts::binary::read::{ReadScope, ReadScopeOwned};
    use allsorts::error::ParseError;
    use allsorts::font_data::FontData;
    use allsorts::tables::{FontTableProvider, NameTable, OffsetTable, OpenTypeData, TTCHeader};
    use allsorts::tag::{self};
    use allsorts::woff::WoffFont;
    use allsorts::woff2::Woff2Font;

    pub type BoxError = Box<dyn std::error::Error>;
    ///
    /// 字体子集化
    ///
    pub(crate) fn subset_text_from_path(input: &str, text: &str) -> Option<Vec<u8>> {
        let data = std::fs::read(input).ok()?;
        let font_file = ReadScope::new(&data).read::<FontData>().ok()?;
        let provider = font_file.table_provider(0).ok()?;
        subset_text(&provider, text)
    }

    ///
    /// 字体子集化
    ///
    pub(crate) fn subset_text<F: FontTableProvider>(
        font_provider: &F,
        text: &str,
    ) -> Option<Vec<u8>> {
        let text = format!("{text}?◻"); // 添加两个占位符，用于字符不存在时渲染，避免完全不渲染的空白

        match do_subset_text(font_provider, remove_duplicate_chars(&text).as_str()) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!("subset fail: {e:?}");
                None
            }
        }
    }
    /// 文本去重
    fn remove_duplicate_chars(input: &str) -> String {
        let mut seen = std::collections::HashSet::new();
        let mut result = String::new();

        for c in input.chars() {
            if !seen.contains(&c) {
                seen.insert(c);
                result.push(c);
            }
        }

        result
    }

    /// 随机数算法
    fn lcg(seed: u32) -> u32 {
        let a: u64 = 1664525;
        let c: u64 = 1013904223;
        let m: u64 = 1 << 32;
        ((a * seed as u64 + c) % m) as u32
    }

    fn do_subset_text<F: FontTableProvider>(
        font_provider: &F,
        text: &str,
    ) -> Result<Vec<u8>, BoxError> {
        // Work out the glyphs we want to keep from the text
        let mut glyphs = chars_to_glyphs(font_provider, text)?;
        let notdef = RawGlyph {
            unicodes: allsorts::tinyvec::tiny_vec![],
            glyph_index: 0,
            liga_component_pos: 0,
            glyph_origin: GlyphOrigin::Direct,
            flags: RawGlyphFlags::empty(),
            variation: None,
            extra_data: (),
        };
        glyphs.insert(0, Some(notdef));

        let mut glyphs: Vec<RawGlyph<()>> = glyphs.into_iter().flatten().collect();
        glyphs.sort_by(|a, b| a.glyph_index.cmp(&b.glyph_index));
        let mut glyph_ids = glyphs
            .iter()
            .map(|glyph| glyph.glyph_index)
            .collect::<Vec<_>>();
        glyph_ids.dedup();
        if glyph_ids.is_empty() {
            panic!("no glyphs left in font");
        }

        // Subset
        let mut new_font = allsorts::subset::subset(
            font_provider,
            &glyph_ids,
            &SubsetProfile::Minimal,
            allsorts::subset::CmapTarget::Unrestricted,
        )?;

        Ok(new_font)
    }

    fn chars_to_glyphs<F: FontTableProvider>(
        font_provider: &F,
        text: &str,
    ) -> Result<Vec<Option<RawGlyph<()>>>, BoxError> {
        let cmap_data = font_provider.read_table_data(allsorts::tag::CMAP)?;
        let cmap = allsorts::binary::read::ReadScope::new(&cmap_data)
            .read::<allsorts::tables::cmap::Cmap>()?;
        let (_, cmap_subtable) = allsorts::font::read_cmap_subtable(&cmap)?.ok_or("fail")?;

        let glyphs = text
            .chars()
            .map(|ch| map(&cmap_subtable, ch, None))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(glyphs)
    }
    fn map(
        cmap_subtable: &allsorts::tables::cmap::CmapSubtable,
        ch: char,
        variation: Option<allsorts::unicode::VariationSelector>,
    ) -> Result<Option<RawGlyph<()>>, allsorts::error::ParseError> {
        if let Some(glyph_index) = cmap_subtable.map_glyph(ch as u32)? {
            let glyph = make(ch, glyph_index, variation);
            Ok(Some(glyph))
        } else {
            Ok(None)
        }
    }
    fn make(
        ch: char,
        glyph_index: u16,
        variation: Option<allsorts::unicode::VariationSelector>,
    ) -> RawGlyph<()> {
        RawGlyph {
            unicodes: allsorts::tinyvec::tiny_vec![[char; 1] => ch],
            glyph_index,
            liga_component_pos: 0,
            glyph_origin: GlyphOrigin::Char(ch),
            flags: RawGlyphFlags::empty(),
            variation,
            extra_data: (),
        }
    }
    pub(crate) fn dump(data: &[u8]) -> String {
        match do_dump(data) {
            Ok(v) => v.0,
            Err(e) => {
                eprintln!("dump error: {e:?}");
                String::new()
            }
        }
    }
    fn do_dump(data: &[u8]) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        let scope = ReadScope::new(data);
        let font_file = scope.read::<FontData>()?;

        match &font_file {
            FontData::OpenType(font_file) => match &font_file.data {
                OpenTypeData::Single(ttf) => dump_ttf(&font_file.scope, ttf),
                OpenTypeData::Collection(ttc) => dump_ttc(&font_file.scope, ttc),
            },
            FontData::Woff(woff_file) => dump_woff(woff_file),
            FontData::Woff2(woff_file) => dump_woff2(woff_file, 0),
        }
    }

    fn dump_ttc<'a>(
        scope: &ReadScope<'a>,
        ttc: &TTCHeader<'a>,
    ) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        if let Some(offset_table_offset) = (&ttc.offset_tables).into_iter().next() {
            let offset_table_offset =
                usize::try_from(offset_table_offset).map_err(ParseError::from)?;
            let offset_table = scope.offset(offset_table_offset).read::<OffsetTable>()?;
            return dump_ttf(scope, &offset_table);
        }
        Ok((String::new(), None))
    }

    fn dump_ttf<'a>(
        scope: &ReadScope<'a>,
        ttf: &OffsetTable<'a>,
    ) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        if let Some(name_table_data) = ttf.read_table(scope, tag::NAME)? {
            let name_table = name_table_data.read::<NameTable>()?;
            return dump_name_table(&name_table);
        }

        Ok((String::new(), None))
    }

    fn dump_woff(woff: &WoffFont<'_>) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        if let Some(entry) = woff
            .table_directory
            .iter()
            .find(|entry| entry.tag == tag::NAME)
        {
            let table = entry.read_table(&woff.scope)?;
            let name_table = table.scope().read::<NameTable>()?;
            return dump_name_table(&name_table);
        }

        Ok((String::new(), None))
    }

    fn dump_woff2<'a>(
        woff: &Woff2Font<'a>,
        index: usize,
    ) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        if let Some(table) = woff.read_table(tag::NAME, index)? {
            let name_table = table.scope().read::<NameTable>()?;
            return dump_name_table(&name_table);
        }

        Ok((String::new(), None))
    }
    fn dump_name_table(
        name_table: &allsorts::tables::NameTable,
    ) -> Result<(String, Option<ReadScopeOwned>), BoxError> {
        use encoding_rs::{MACINTOSH, UTF_16BE};
        for name_record in &name_table.name_records {
            let platform = name_record.platform_id;
            let encoding = name_record.encoding_id;
            let language = name_record.language_id;
            let offset = usize::from(name_record.offset);
            let length = usize::from(name_record.length);
            let name_scope = name_table.string_storage.offset_length(offset, length)?;
            let name_data = name_scope.data();

            // s_info!(
            //     "offset={}, length = {length},{:?}",
            //     name_table.string_storage.base + offset,
            //     name_data
            // );
            let name = match (platform, encoding) {
                (0, _) => decode(UTF_16BE, name_data),
                (1, 0) => decode(MACINTOSH, name_data),
                (3, 0) => decode(UTF_16BE, name_data),
                (3, 1) => decode(UTF_16BE, name_data),
                (3, 10) => decode(UTF_16BE, name_data),
                _ => format!(
                    "(unknown platform={} encoding={} language={})",
                    platform, encoding, language
                ),
            };
            if let NameTable::FULL_FONT_NAME = name_record.name_id {
                return Ok((name, Some(ReadScopeOwned::new(name_scope))));
            }
        }
        Ok((String::new(), None))
    }

    fn decode(encoding: &'static encoding_rs::Encoding, data: &[u8]) -> String {
        let mut decoder = encoding.new_decoder();
        if let Some(size) = decoder.max_utf8_buffer_length(data.len()) {
            let mut s = String::with_capacity(size);
            let (_res, _read, _repl) = decoder.decode_to_string(data, &mut s, true);
            s
        } else {
            String::new() // can only happen if buffer is enormous
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        error::Error,
        fs::{self, File},
        sync::{Mutex, MutexGuard, OnceLock},
    };

    const TEST_MKV_URL: &str = "https://github.com/inkroom/mkvtool/releases/download/resource/";
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

    fn download_test_mkv(mkv: &str) -> Result<PathBuf, Box<dyn Error>> {
        let fixture_dir = test_target_dir()?.join("ffmpeg-test-fixtures");
        let fixture = fixture_dir.join(mkv);
        if fixture.is_file() && fixture.metadata()?.len() > 0 {
            return Ok(fixture);
        }

        fs::create_dir_all(&fixture_dir)?;
        let temporary = fixture.with_extension(format!("mkv-{}", std::process::id()));
        let response = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(60))
            .build()
            .get(format!("{}{}", TEST_MKV_URL, mkv).as_str())
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

    fn first_editable_subtitle(service: &ActiveFfmpegService, input: &Path) -> Result<u32, String> {
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
        let res = vec!["test.mkv", "test2.mkv"];
        for m in res {
            let input = download_test_mkv(m).expect("测试文件应下载到 target 目录");
            let ffi = FFIFfmpegService::new();
            let cli = CliFfmpegService::new();
            let ffi_media = ffi.inspect(&input).expect("FFI FFmpeg 应能探测测试文件");
            let cli_media = cli.inspect(&input).expect("FFmpeg CLI 应能探测测试文件");

            assert!(!ffi_media.streams.is_empty());
            assert_eq!(
                ffi_media.streams.len(),
                cli_media.streams.len(),
                "两种 FFmpeg 实现返回的 stream 数量不一致"
            );
            assert!(ffi_media
                .streams
                .iter()
                .any(|stream| stream.stream_type == MediaStreamType::Video));
            assert!(ffi_media
                .streams
                .iter()
                .any(|stream| stream.stream_type == MediaStreamType::Audio));
            assert!(ffi_media
                .streams
                .iter()
                .any(|stream| stream.stream_type == MediaStreamType::Subtitle));
            for (ffi_stream, cli_stream) in ffi_media.streams.iter().zip(&cli_media.streams) {
                assert_eq!(
                    ffi_stream.index, cli_stream.index,
                    "stream 顺序或 index 不一致"
                );
                assert_eq!(
                    ffi_stream.stream_type, cli_stream.stream_type,
                    "stream #{} 类型不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.codec_name, cli_stream.codec_name,
                    "stream #{} codecname 不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.title, cli_stream.title,
                    "stream #{} title 不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.language, cli_stream.language,
                    "stream #{} language 不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.default_stream, cli_stream.default_stream,
                    "stream #{} default 标记不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.forced, cli_stream.forced,
                    "stream #{} forced 标记不一致",
                    ffi_stream.index
                );
                assert_eq!(
                    ffi_stream.editable, cli_stream.editable,
                    "stream #{} editable 标记不一致",
                    ffi_stream.index
                );
            }

            assert_eq!(
                cli_media
                    .streams
                    .iter()
                    .any(|stream| stream.editable && stream.codec_name.as_deref() == Some("ass")),
                ffi_media
                    .streams
                    .iter()
                    .any(|stream| stream.editable && stream.codec_name.as_deref() == Some("ass"))
            );
        }
    }
    fn assert_reads_subtitle_from_downloaded_test_file(input: &Path) {
        let ffi = FFIFfmpegService::new();
        let cli = CliFfmpegService::new();
        let streams = ffi
            .inspect(input)
            .expect("FFI FFmpeg 应能探测测试文件")
            .streams;
        let editable_streams = streams.iter().filter(|stream| stream.editable);

        assert!(
            editable_streams.clone().next().is_some(),
            "测试文件应包含可编辑字幕流"
        );
        for stream in editable_streams {
            let subtitle = ffi
                .read_subtitle(input, stream.index)
                .expect("FFI FFmpeg 应能读取测试字幕");
            let ffmpeg_subtitle = cli
                .read_subtitle(input, stream.index)
                .expect("FFmpeg CLI 应能读取测试字幕");

            assert_eq!(
                subtitle.format, ffmpeg_subtitle.format,
                "字幕流 #{} 格式不一致",
                stream.index
            );
            assert_eq!(
                subtitle.codec_name, ffmpeg_subtitle.codec_name,
                "字幕流 #{} 编码不一致",
                stream.index
            );
            assert_eq!(
                subtitle.content, ffmpeg_subtitle.content,
                "字幕流 #{} 文本不一致",
                stream.index
            );
        }
    }

    #[test]
    fn assert_reads_subtitles_from_downloaded_test_files() {
        let _lock = ffmpeg_test_lock();
        for mkv in ["test.mkv", "test2.mkv"] {
            let input = download_test_mkv(mkv).expect("测试文件应下载到 target 目录");
            assert_reads_subtitle_from_downloaded_test_file(&input);
        }
    }

    #[test]
    fn assert_remuxes_subtitle_from_downloaded_test_file() {
        let _lock = ffmpeg_test_lock();
        let res = vec!["test.mkv", "test2.mkv"];
        for m in res {
            let input = download_test_mkv(m).expect("测试文件应下载到 target 目录");
            let service = ActiveFfmpegService::new();
            let stream_index =
                first_editable_subtitle(&service, &input).expect("测试文件应包含可编辑字幕流");
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
                        format: None,
                        language: None,
                        title: None,
                    }],
                )
                .expect("FFmpeg 应能重新混流测试字幕");

            let remuxed = service.inspect(&output).expect("FFmpeg 应能探测重混流结果");
            assert_eq!(
                remuxed.streams.len(),
                service.inspect(&input).unwrap().streams.len()
            );
        }
    }

    #[test]
    fn remuxes_srt_as_ass_with_script_info_header() {
        let _lock = ffmpeg_test_lock();
        let input = download_test_mkv("test2.mkv").expect("测试文件应下载到 target 目录");
        let service = FFIFfmpegService::new();
        let media = service
            .inspect(&input)
            .expect("FFI FFmpeg 应能探测测试文件");
        let stream = media
            .streams
            .iter()
            .find(|stream| stream.editable && stream.codec_name.as_deref() == Some("srt"))
            .expect("测试文件应包含 SRT 字幕流");
        let output = test_target_dir()
            .expect("应能定位 Cargo target 目录")
            .join("ffmpeg-test-fixtures")
            .join("remuxed-srt-as-ass.mkv");
        let selected_stream_indices = media
            .streams
            .iter()
            .map(|stream| stream.index)
            .collect::<Vec<_>>();

        service
            .remux_selected_streams(
                &input,
                &output,
                &[SubtitleEdit {
                    stream_index: stream.index,
                    content: "[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,第一条字幕\nDialogue: 0,0:00:02.00,0:00:03.00,Default,,0,0,0,,第二条字幕\n".to_string(),
                    format: Some("ass".to_string()),
                    language: None,
                    title: None,
                }],
                &selected_stream_indices,
                Some(stream.index),
                &[],
            )
            .expect("FFI FFmpeg 应能将 SRT 字幕重混流为 ASS");

        let remuxed = service
            .read_subtitle(&output, stream.index)
            .expect("转换后的 ASS 字幕应可读取");
        assert_eq!(remuxed.format, "ass");
        assert!(remuxed.content.starts_with("[Script Info]"));
        let default_subtitle_streams = service
            .inspect(&output)
            .expect("转换后的文件应可探测")
            .streams
            .into_iter()
            .filter(|stream| {
                stream.stream_type == MediaStreamType::Subtitle && stream.default_stream
            })
            .map(|stream| stream.index)
            .collect::<Vec<_>>();
        assert_eq!(default_subtitle_streams, vec![stream.index]);
        assert_reads_subtitle_from_downloaded_test_file(&output);
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

        let probe = CliFfmpegService::new()
            .parse_ffmpeg_probe_output(output)
            .expect("FFmpeg output should parse");
        assert_eq!(
            probe.format.and_then(|format| format.duration),
            Some("3723.5".to_string())
        );
        assert_eq!(probe.streams.len(), 2);

        let subtitle = &probe.streams[1];
        assert_eq!(subtitle.index, 1);
        assert_eq!(subtitle.codec_type, MediaStreamType::Subtitle);
        assert_eq!(subtitle.codec_name.as_deref(), Some("ass"));
        assert_eq!(
            subtitle
                .tags
                .as_ref()
                .and_then(|tags| tags.language.as_deref()),
            Some("eng")
        );
        assert_eq!(
            subtitle
                .tags
                .as_ref()
                .and_then(|tags| tags.title.as_deref()),
            Some("English subtitles")
        );
        assert_eq!(
            subtitle
                .disposition
                .as_ref()
                .and_then(|value| value.default),
            Some(1)
        );
        assert_eq!(
            subtitle.disposition.as_ref().and_then(|value| value.forced),
            Some(1)
        );
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
    selected_stream_indices: Vec<u32>,
    default_subtitle_stream_index: Option<u32>,
    mut font_attachments: Vec<FontAttachment>,
    subset_fonts: bool,
) -> Result<(), String> {
    let input = mkv_path(&input_path)?;
    let output = PathBuf::from(output_path);
    if output.as_os_str().is_empty()
        || output.extension().and_then(|extension| extension.to_str()) != Some("mkv")
    {
        return Err("输出文件必须使用 .mkv 扩展名。".to_string());
    }
    let service = ActiveFfmpegService::new();
    tauri::async_runtime::spawn_blocking(move || {
        prepare_font_attachments(
            &service,
            &input,
            &edits,
            &mut font_attachments,
            subset_fonts,
        )?;
        service.remux_selected_streams(
            &input,
            &output,
            &edits,
            &selected_stream_indices,
            default_subtitle_stream_index,
            &font_attachments,
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
    let data = fs::read(path).ok()?;
    let name = font::dump(&data);
    (!name.trim().is_empty()).then_some(name)
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
            read_font_name,
            pick_font_file,
            pick_output_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tauri application");
}
