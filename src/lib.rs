use std::fs;

use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, GithubReleaseOptions, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

const SERVER: &str = "gradle-catalog-lsp";
const REPOSITORY: &str = "husur/gradle-catalog-lsp";

struct GradleCatalog {
    downloaded: Option<String>,
}

impl zed::Extension for GradleCatalog {
    fn new() -> Self {
        Self { downloaded: None }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let command = match worktree.which(SERVER) {
            Some(path) => path,
            None => self.downloaded_server(language_server_id)?,
        };
        Ok(zed::Command {
            command,
            args: vec![],
            env: vec![],
        })
    }
}

impl GradleCatalog {
    fn downloaded_server(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        if let Some(path) = self.downloaded.as_ref().filter(|path| is_file(path)) {
            return Ok(path.clone());
        }
        let path = download_latest(language_server_id)
            .or_else(|error| previous_download().ok_or(error))?;
        self.downloaded = Some(path.clone());
        Ok(path)
    }
}

fn download_latest(language_server_id: &LanguageServerId) -> Result<String> {
    let status = |status| zed::set_language_server_installation_status(language_server_id, &status);
    status(LanguageServerInstallationStatus::CheckingForUpdate);
    let release = zed::latest_github_release(
        REPOSITORY,
        GithubReleaseOptions {
            require_assets: true,
            pre_release: false,
        },
    )?;
    let asset_name = format!("{SERVER}-{}.gz", target()?);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| format!("{asset_name} is missing from release {}", release.version))?;

    let directory = format!("{SERVER}-{}", release.version);
    let path = format!("{directory}/{}", executable());
    if !is_file(&path) {
        status(LanguageServerInstallationStatus::Downloading);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        zed::download_file(&asset.download_url, &path, DownloadedFileType::Gzip)?;
        zed::make_file_executable(&path)?;
        for old in server_directories().filter(|old| *old != directory) {
            let _ = fs::remove_dir_all(old);
        }
    }
    status(LanguageServerInstallationStatus::None);
    Ok(path)
}

fn previous_download() -> Option<String> {
    server_directories()
        .map(|directory| format!("{directory}/{}", executable()))
        .find(|path| is_file(path))
}

fn server_directories() -> impl Iterator<Item = String> {
    fs::read_dir(".")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(&format!("{SERVER}-")))
}

fn target() -> Result<&'static str> {
    match zed::current_platform() {
        (Os::Mac, Architecture::Aarch64) => Ok("aarch64-apple-darwin"),
        (Os::Mac, Architecture::X8664) => Ok("x86_64-apple-darwin"),
        (Os::Linux, Architecture::Aarch64) => Ok("aarch64-unknown-linux-musl"),
        (Os::Linux, Architecture::X8664) => Ok("x86_64-unknown-linux-musl"),
        (Os::Windows, Architecture::X8664) => Ok("x86_64-pc-windows-msvc"),
        (os, architecture) => Err(format!(
            "no {SERVER} build for {os:?} {architecture:?}: build it from source and put it in your PATH"
        )),
    }
}

fn executable() -> String {
    match zed::current_platform().0 {
        Os::Windows => format!("{SERVER}.exe"),
        _ => SERVER.to_string(),
    }
}

fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

zed::register_extension!(GradleCatalog);
