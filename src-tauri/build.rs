use std::{
    env,
    fs::{self, File},
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
};

const RELEASE_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    download_sidecars().unwrap_or_else(|error| panic!("无法准备 FFmpeg sidecar：{error}"));
    tauri_build::build();
}

fn download_sidecars() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET")?;
    let extension = if target.contains("windows") { ".exe" } else { "" };
    let binaries_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("binaries");
    let ffmpeg = binaries_dir.join(format!("ffmpeg-{target}{extension}"));
    let ffprobe = binaries_dir.join(format!("ffprobe-{target}{extension}"));

    if ffmpeg.is_file() && ffprobe.is_file() {
        return Ok(());
    }

    if target.contains("windows") {
        return Err(format!(
            "Windows sidecar 必须使用 Docker 交叉编译；请运行 `sh ffmpeg/build-Windows.sh` 生成 {target} 的二进制。"
        )
        .into());
    }

    fs::create_dir_all(&binaries_dir)?;
    let archive_name = archive_name(&target)?;
    let url = format!("{RELEASE_URL}/{archive_name}");
    println!("cargo:warning=Downloading FFmpeg sidecars for {target} from BtbN/FFmpeg-Builds");
    let response = ureq::get(&url).call()?;
    let mut archive = Vec::new();
    response.into_reader().read_to_end(&mut archive)?;

    if archive_name.ends_with(".zip") {
        extract_zip(&archive, &ffmpeg, &ffprobe)?;
    } else {
        extract_tar_xz(&archive, &ffmpeg, &ffprobe)?;
    }
    make_executable(&ffmpeg)?;
    make_executable(&ffprobe)?;
    Ok(())
}

fn archive_name(target: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    let name = match target {
        "x86_64-unknown-linux-gnu" => "ffmpeg-master-latest-linux64-gpl.tar.xz",
        "aarch64-unknown-linux-gnu" => "ffmpeg-master-latest-linuxarm64-gpl.tar.xz",
        "x86_64-apple-darwin" => "ffmpeg-master-latest-macos64-gpl.zip",
        "aarch64-apple-darwin" => "ffmpeg-master-latest-macosarm64-gpl.zip",
        _ => return Err(format!("不支持的目标架构：{target}").into()),
    };
    Ok(name)
}

fn extract_zip(archive: &[u8], ffmpeg: &Path, ffprobe: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let destination = match executable_destination(entry.name(), ffmpeg, ffprobe) {
            Some(destination) => destination,
            None => continue,
        };
        copy_entry(&mut entry, destination)?;
    }
    ensure_binaries_exist(ffmpeg, ffprobe)
}

fn extract_tar_xz(archive: &[u8], ffmpeg: &Path, ffprobe: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let decoder = xz2::read::XzDecoder::new(Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let destination = match executable_destination(&path.to_string_lossy(), ffmpeg, ffprobe) {
            Some(destination) => destination,
            None => continue,
        };
        copy_entry(&mut entry, destination)?;
    }
    ensure_binaries_exist(ffmpeg, ffprobe)
}

fn executable_destination<'a>(path: &str, ffmpeg: &'a Path, ffprobe: &'a Path) -> Option<&'a Path> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    match file_name {
        "ffmpeg" | "ffmpeg.exe" => Some(ffmpeg),
        "ffprobe" | "ffprobe.exe" => Some(ffprobe),
        _ => None,
    }
}

fn copy_entry(reader: &mut impl Read, destination: &Path) -> io::Result<()> {
    let mut output = File::create(destination)?;
    io::copy(reader, &mut output)?;
    Ok(())
}

fn ensure_binaries_exist(ffmpeg: &Path, ffprobe: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if ffmpeg.is_file() && ffprobe.is_file() {
        Ok(())
    } else {
        Err("下载的 FFmpeg 发布包中缺少 ffmpeg 或 ffprobe。".into())
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}
