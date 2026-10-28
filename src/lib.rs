use zed_extension_api::{self as zed, LanguageServerId, Result, Worktree};

struct GradleCatalog;

impl zed::Extension for GradleCatalog {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> Result<zed::Command> {
        let command = worktree
            .which("gradle-catalog-lsp")
            .ok_or("gradle-catalog-lsp not found in PATH")?;
        Ok(zed::Command {
            command,
            args: vec![],
            env: vec![],
        })
    }
}

zed::register_extension!(GradleCatalog);
