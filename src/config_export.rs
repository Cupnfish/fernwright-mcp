use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
pub enum ClientType {
    ClaudeDesktop,
    ClaudeCode,
    Droid,
    Codex,
}

impl ClientType {
    pub fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "claude-desktop" | "claude_desktop" | "claude" => Ok(Self::ClaudeDesktop),
            "claude-code" | "claude_code" => Ok(Self::ClaudeCode),
            "droid" | "droid-cli" | "droid_cli" => Ok(Self::Droid),
            "codex" | "codex-cli" => Ok(Self::Codex),
            other => Err(anyhow!(
                "unknown client type '{other}', expected: claude-desktop | claude-code | droid | codex"
            )),
        }
    }
}

pub fn endpoint_url(http_addr: &str) -> String {
    let trimmed = http_addr.trim().trim_end_matches('/');
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        format!("{trimmed}/mcp")
    } else if let Some(rest) = trimmed.strip_prefix("127.0.0.1") {
        format!("http://localhost{rest}/mcp")
    } else {
        format!("http://{}/mcp", trimmed)
    }
}

fn json_config(http_addr: &str) -> Value {
    json!({
        "mcpServers": {
            "playwright-tab-bridge": {
                "type": "http",
                "url": endpoint_url(http_addr),
            }
        }
    })
}

pub fn json_config_pretty(http_addr: &str) -> Result<String> {
    Ok(serde_json::to_string_pretty(&json_config(http_addr))?)
}

fn toml_config(http_addr: &str) -> String {
    let endpoint = endpoint_url(http_addr);
    format!(
        r#"[mcp_servers.playwright-tab-bridge]
url = "{endpoint}""#
    )
}

/// Returns CLI command for adding MCP server, or None if client doesn't support CLI
pub fn cli_command(client: ClientType, http_addr: &str) -> Option<String> {
    let endpoint = endpoint_url(http_addr);
    match client {
        ClientType::ClaudeCode => Some(format!(
            "claude mcp add playwright-tab-bridge --transport http {endpoint}"
        )),
        ClientType::Droid => Some(format!(
            "droid mcp add playwright-tab-bridge {endpoint} --type http"
        )),
        ClientType::Codex => {
            Some("codex mcp add playwright-tab-bridge -- playright-mcp serve-stdio".to_owned())
        }
        ClientType::ClaudeDesktop => None,
    }
}

/// Returns config file content for the client
pub fn config_content(client: ClientType, http_addr: &str) -> Result<String> {
    match client {
        ClientType::Codex => Ok(toml_config(http_addr)),
        _ => json_config_pretty(http_addr),
    }
}

pub fn render_export_text(client: ClientType, http_addr: &str) -> Result<String> {
    let config = config_content(client, http_addr)?;
    let cli = cli_command(client, http_addr);

    let cli_section = cli
        .map(|cmd| format!("\nCLI 命令:\n{cmd}\n"))
        .unwrap_or_default();

    let text = match client {
        ClientType::ClaudeDesktop => format!(
            "# Claude Desktop\n\n\
             配置文件位置:\n\
             - Windows: %APPDATA%\\Claude\\claude_desktop_config.json\n\
             - macOS: ~/Library/Application Support/Claude/claude_desktop_config.json\n\
             - Linux: ~/.config/Claude/claude_desktop_config.json\n\n\
             建议配置:\n{config}\n"
        ),
        ClientType::ClaudeCode => format!(
            "# Claude Code\n\n{cli_section}\
             项目 .mcp.json / 全局 ~/.claude.json 配置:\n{config}\n"
        ),
        ClientType::Droid => format!(
            "# Droid CLI\n\n{cli_section}\
             项目 .factory/mcp.json / 用户 ~/.factory/mcp.json 配置:\n{config}\n"
        ),
        ClientType::Codex => format!(
            "# Codex\n\n{cli_section}\
             配置文件位置:\n\
             - Windows: %USERPROFILE%\\.codex\\config.toml\n\
             - macOS/Linux: ~/.codex/config.toml\n\
             - 项目级别: .codex/config.toml\n\n\
             建议优先使用上面的 CLI 命令（stdio transport）。\n\
             若你需要 HTTP 配置，默认使用 `localhost` 而不是 `127.0.0.1`，这样在部分 Codex / proxy 场景下更稳定：\n{config}\n"
        ),
    };

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_url_prefers_localhost_for_bare_loopback_addr() {
        assert_eq!(endpoint_url("127.0.0.1:3000"), "http://localhost:3000/mcp");
    }

    #[test]
    fn endpoint_url_preserves_explicit_http_url() {
        assert_eq!(
            endpoint_url("http://127.0.0.1:3000"),
            "http://127.0.0.1:3000/mcp"
        );
    }
}

pub fn write_config_file(
    client: ClientType,
    http_addr: &str,
    output_path: Option<PathBuf>,
) -> Result<PathBuf> {
    let path = output_path.unwrap_or_else(|| {
        let name = match client {
            ClientType::ClaudeDesktop => "claude_desktop_config.json",
            ClientType::ClaudeCode => "claude_code_mcp_config.json",
            ClientType::Droid => "droid_mcp_config.json",
            ClientType::Codex => "codex_config.toml",
        };
        PathBuf::from(name)
    });

    let content = config_content(client, http_addr)?;
    fs::write(&path, content)?;
    Ok(path)
}
