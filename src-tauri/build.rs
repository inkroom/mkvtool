use std::{env, fs, path::PathBuf, process::Command};

use std::{
    fs::File,
    io::{self, Cursor, Read},
    path::Path,
};

const RELEASE_URL: &str = "https://github.com/inkroom/mkvtool/releases/download/resource/";

#[cfg(all(windows, not(feature = "gui")))]
fn embed_windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let icon = manifest_dir.join("icons").join("icon.ico");
    let output_dir = PathBuf::from(env::var("OUT_DIR")?);
    let resource_script = output_dir.join("mkv-cli-icon.rc");
    let package_name = env::var("CARGO_PKG_NAME")?;
    let package_description = env::var("CARGO_PKG_DESCRIPTION")?;
    let package_authors = env::var("CARGO_PKG_AUTHORS")?;
    let package_version = env::var("CARGO_PKG_VERSION")?;
    let version_major: u16 = env::var("CARGO_PKG_VERSION_MAJOR")?.parse()?;
    let version_minor: u16 = env::var("CARGO_PKG_VERSION_MINOR")?.parse()?;
    let version_patch: u16 = env::var("CARGO_PKG_VERSION_PATCH")?.parse()?;
    let icon = windows_resource_string(&icon.to_string_lossy());

    println!("cargo:rerun-if-changed=icons/icon.ico");
    let resource = format!(
        r##"1 VERSIONINFO
FILEVERSION {version_major},{version_minor},{version_patch},0
PRODUCTVERSION {version_major},{version_minor},{version_patch},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "{package_authors}"
            VALUE "FileDescription", "{package_description}"
            VALUE "FileVersion", "{package_version}"
            VALUE "InternalName", "mkv-cli.exe"
            VALUE "OriginalFilename", "mkv-cli.exe"
            VALUE "ProductName", "{package_name}"
            VALUE "ProductVersion", "{package_version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
1 ICON "{icon}"
"##,
        package_authors = windows_resource_string(&package_authors),
        package_description = windows_resource_string(&package_description),
        package_version = windows_resource_string(&package_version),
        package_name = windows_resource_string(&package_name),
    );
    fs::write(&resource_script, resource)?;
    embed_resource::compile_for(&resource_script, ["mkv-cli"], embed_resource::NONE)
        .manifest_required()
        .map_err(|error| format!("无法编译 Windows 图标资源：{error}"))?;
    Ok(())
}

#[cfg(all(windows, not(feature = "gui")))]
fn windows_resource_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\"\"")
}
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        println!("cargo:rustc-link-lib=ncrypt"); // 修复 NCryptOpenStorageProvider
        println!("cargo:rustc-link-lib=crypt32");
        println!("cargo:rustc-link-lib=gdi32");
        println!("cargo:rustc-link-lib=user32");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=oleaut32");
        println!("cargo:rustc-link-lib=uuid");
        println!("cargo:rustc-link-lib=shell32");
        println!("cargo:rustc-link-lib=advapi32");
        println!("cargo:rustc-link-lib=bcrypt");
        println!("cargo:rustc-link-lib=secur32");
        println!("cargo:rustc-link-lib=mfplat");
        println!("cargo:rustc-link-lib=strmiids");
        println!("cargo:rustc-link-lib=mfuuid");
    }
    #[cfg(all(windows, not(feature = "gui")))]
    embed_windows_icon().expect("嵌入logo失败");
    inject_build_metadata();
    if cfg!(not(target_os = "macos")) {
        download_sidecars().unwrap_or_else(|error| panic!("无法准备 FFmpeg sidecar：{error}"));
    }
    copy_resource().unwrap_or_else(|error| panic!("copy ffmpeg fail：{error}"));
    #[cfg(feature = "gui")]
    {
        tauri_build::build();
    }
}

fn copy_resource() -> Result<(), Box<dyn std::error::Error>> {
    // ffmpeg 作为资源文件 而非侧车进程的处理逻辑
    let target = env::var("TARGET")?;
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let binaries_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("binaries");
    let ffmpeg = binaries_dir.join(format!("ffm-{target}{extension}"));
    if ffmpeg.exists() {
        fs::copy(ffmpeg, binaries_dir.join("ffm"))?;
        return Ok(());
    }
    let ffmpeg = binaries_dir.join(format!("ffmpeg{extension}"));
    if ffmpeg.exists() {
        fs::copy(ffmpeg, binaries_dir.join("ffm"))?;
    }
    Ok(())
}

fn inject_build_metadata() {
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/packed-refs");
    println!("cargo:rerun-if-env-changed=RUSTC");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:warning=manifest_dir = {}", manifest_dir.display());
    let repository_dir = manifest_dir.parent().unwrap_or(manifest_dir.as_path());
    let git_hash = command_output(Command::new("git").current_dir(&manifest_dir).args([
        "-c",
        &format!("safe.directory={}", repository_dir.display()),
        "rev-parse",
        "--short",
        "HEAD",
    ]))
    .expect("无法获取 Git commit 信息");
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rust_version = command_output(Command::new(rustc).arg("--version")).unwrap();
    let package_version = env::var("CARGO_PKG_VERSION").unwrap();

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

fn download_sidecars() -> Result<(), Box<dyn std::error::Error>> {
    let target = env::var("TARGET")?;
    let extension = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };
    let binaries_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("binaries");
    let ffmpeg = binaries_dir.join(format!("ffm-{target}{extension}"));

    if ffmpeg.is_file() {
        return Ok(());
    }

    fs::create_dir_all(&binaries_dir)?;
    let archive_name = archive_name(&target)?;
    let url = format!("{RELEASE_URL}/{archive_name}");
    println!(
        "cargo:warning=Downloading FFmpeg sidecars for {target} from Github, url = {}",
        url
    );
    let response = ureq::get(&url).call()?;
    let mut archive = Vec::new();
    response.into_reader().read_to_end(&mut archive)?;

    if archive_name.ends_with(".zip") {
        extract_zip(&archive, &ffmpeg)?;
    } else {
        extract_tar_xz(&archive, &ffmpeg)?;
    }
    make_executable(&ffmpeg)?;
    Ok(())
}

fn archive_name(target: &str) -> Result<&'static str, Box<dyn std::error::Error>> {
    let name = match target {
        "x86_64-unknown-linux-gnu" => "ffmpeg-n8.1-latest-linux64.zip",
        "aarch64-unknown-linux-gnu" => "ffmpeg-n8.1-latest-linuxarm64.zip",
        "x86_64-pc-windows-msvc" => "ffmpeg-n8.1-latest-win64.zip",
        "x86_64-pc-windows-gnu" => "ffmpeg-n8.1-latest-win64.zip",
        _ => return Err(format!("不支持的目标架构：{target}").into()),
    };
    Ok(name)
}

fn extract_zip(archive: &[u8], ffmpeg: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let destination = match executable_destination(entry.name(), ffmpeg) {
            Some(destination) => destination,
            None => continue,
        };
        copy_entry(&mut entry, destination)?;
    }
    ensure_binaries_exist(ffmpeg)
}

fn extract_tar_xz(archive: &[u8], ffmpeg: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let decoder = xz2::read::XzDecoder::new(Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        let destination = match executable_destination(&path.to_string_lossy(), ffmpeg) {
            Some(destination) => destination,
            None => continue,
        };
        copy_entry(&mut entry, destination)?;
    }
    ensure_binaries_exist(ffmpeg)
}

fn executable_destination<'a>(path: &str, ffmpeg: &'a Path) -> Option<&'a Path> {
    let file_name = Path::new(path).file_name()?.to_str()?;
    match file_name {
        "ffmpeg" | "ffmpeg.exe" => Some(ffmpeg),
        _ => None,
    }
}

fn copy_entry(reader: &mut impl Read, destination: &Path) -> io::Result<()> {
    let mut output = File::create(destination)?;
    io::copy(reader, &mut output)?;
    Ok(())
}

fn ensure_binaries_exist(ffmpeg: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if ffmpeg.is_file() {
        Ok(())
    } else {
        Err("下载的 FFmpeg 发布包中缺少 ffmpeg 。".into())
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
