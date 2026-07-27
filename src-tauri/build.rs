use std::{
    env,
    fs::{self, File},
    io::{self, Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
};

const RELEASE_URL: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/latest/download";
const FFMPEG_SOURCE_URL: &str = "https://github.com/ffmpeg/ffmpeg";
const FFMPEG_SOURCE_REF: &str = "release/8.1";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    inject_build_metadata();
    if cfg!(target_os = "macos") {
        build_macos_sidecars()
            .unwrap_or_else(|error| panic!("无法编译 macOS FFmpeg sidecar：{error}"));
    } else {
        download_sidecars().unwrap_or_else(|error| panic!("无法准备 FFmpeg sidecar：{error}"));
    }
    tauri_build::build();
}

fn inject_build_metadata() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let git_hash = command_output(Command::new("git").current_dir(&manifest_dir).args([
        "rev-parse",
        "--short",
        "HEAD",
    ]))
    .unwrap_or_else(|| "unknown".to_string());
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rust_version = command_output(Command::new(rustc).arg("--version"))
        .unwrap_or_else(|| "unknown".to_string());
    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "unknown".to_string());

    let output_path = PathBuf::from(env::var("OUT_DIR").unwrap()).join("build_info.rs");
    fs::write(
        output_path,
        format!(
            "pub const BUILD_PACKAGE_VERSION: &str = {package_version:?};\npub const BUILD_GIT_HASH: &str = {git_hash:?};\npub const BUILD_RUST_VERSION: &str = {rust_version:?};\n"
        ),
    )
    .expect("无法写入构建信息");
}

fn command_output(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn build_macos_sidecars() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET")?;
    if !target.ends_with("-apple-darwin") {
        return Err(format!("macOS 主机不能为目标 {target} 编译原生 FFmpeg sidecar。").into());
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let binaries_dir = manifest_dir.join("binaries");
    let ffmpeg = binaries_dir.join(format!("ffm-{target}"));
    let ffprobe = binaries_dir.join(format!("ffp-{target}"));
    if ffmpeg.is_file() && ffprobe.is_file() {
        return Ok(());
    }

    let source_dir = PathBuf::from(env::var("OUT_DIR")?).join("ffmpeg-release-8.1");
    if !source_dir.join(".git").is_dir() {
        if source_dir.exists() {
            fs::remove_dir_all(&source_dir)?;
        }
        println!("cargo:warning=Cloning FFmpeg {FFMPEG_SOURCE_REF} for macOS sidecars");
        run_command(
            Command::new("git")
                .args([
                    "clone",
                    "--depth",
                    "1",
                    "--branch",
                    FFMPEG_SOURCE_REF,
                    FFMPEG_SOURCE_URL,
                ])
                .arg(&source_dir),
            "克隆 FFmpeg 源码",
        )?;
    }

    let install_dir = source_dir.join("dist");
    let jobs = std::thread::available_parallelism()?.get().to_string();
    println!("cargo:warning=Building native macOS FFmpeg sidecars for {target}");
    run_command(
        Command::new(source_dir.join("configure"))
            .current_dir(&source_dir)
            .args([
                "--prefix=dist/",
                "--disable-everything",
                "--extra-cflags=-march=native -mtune=native",
                "--disable-debug",
                "--disable-stripping",
                "--enable-static",
                "--disable-shared",
                "--enable-pthreads",
                "--disable-pic",
                "--disable-autodetect",
                "--disable-programs",
                "--disable-doc",
                "--disable-gpl",
                "--disable-version3",
                "--disable-nonfree",
                "--enable-avcodec",
                "--disable-avdevice",
                "--enable-avformat",
                "--disable-swresample",
                "--disable-swscale",
                "--extra-cflags=-w",
                "--enable-ffmpeg",
                "--enable-ffprobe",
                "--enable-avfilter",
                "--enable-protocol=file,pipe",
                "--enable-demuxer=matroska,ass,srt",
                "--enable-muxer=matroska,ass,srt",
                "--enable-decoder=ass,ssa,subrip",
                "--enable-encoder=ass,ssa,subrip",
            ]),
        "配置 FFmpeg",
    )?;
    run_command(
        Command::new("make")
            .current_dir(&source_dir)
            .arg(format!("-j{jobs}")),
        "编译 FFmpeg",
    )?;
    run_command(
        Command::new("make").current_dir(&source_dir).arg("install"),
        "安装 FFmpeg",
    )?;

    fs::create_dir_all(&binaries_dir)?;
    fs::copy(install_dir.join("bin/ffmpeg"), &ffmpeg)?;
    fs::copy(install_dir.join("bin/ffprobe"), &ffprobe)?;
    make_executable(&ffmpeg)?;
    make_executable(&ffprobe)?;
    ensure_binaries_exist(&ffmpeg, &ffprobe)
}

fn run_command(command: &mut Command, action: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{action}失败，退出码：{status}").into())
    }
}

fn download_sidecars() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET")?;
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let binaries_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("binaries");
    let ffmpeg = binaries_dir.join(format!("ffm-{target}{extension}"));
    let ffprobe = binaries_dir.join(format!("ffp-{target}{extension}"));

    if ffmpeg.is_file() && ffprobe.is_file() {
        return Ok(());
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

fn extract_zip(
    archive: &[u8],
    ffmpeg: &Path,
    ffprobe: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
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

fn extract_tar_xz(
    archive: &[u8],
    ffmpeg: &Path,
    ffprobe: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
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
