use std::env;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu},
};
use winit::{
    application::ApplicationHandler,
    event::{StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
};

mod bridge;
mod config_export;
mod mcp_http;
mod mcp_server;
mod search;
mod search_service;

use bridge::BridgeServer;
use config_export::{
    ClientType, cli_command, config_content, endpoint_url, render_export_text, write_config_file,
};

const DEFAULT_BRIDGE_ADDR: &str = "127.0.0.1:17373";
const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:3000";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
const EMBEDDED_TRAY_ICON_SVG: &[u8] = include_bytes!("../assets/icon.svg");
const TRAY_ICON_TARGET_SIZE: u32 = 64;

#[derive(Debug, Parser)]
#[command(
    name = "fernwright-mcp",
    version,
    about = "Fernwright MCP bridge server"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run bridge listener + HTTP MCP server.
    Serve {
        /// WebSocket bridge listener address (host:port).
        #[arg(long)]
        bridge_addr: Option<String>,
        /// HTTP MCP listener address (host:port).
        #[arg(long)]
        http_addr: Option<String>,
        /// Request timeout when forwarding to extension (ms).
        #[arg(long)]
        request_timeout_ms: Option<u64>,
        /// Disable tray icon integration.
        #[arg(long, default_value_t = false)]
        no_tray: bool,
    },
    /// Run bridge listener + stdio MCP server.
    ServeStdio {
        /// WebSocket bridge listener address (host:port).
        #[arg(long)]
        bridge_addr: Option<String>,
        /// Request timeout when forwarding to extension (ms).
        #[arg(long)]
        request_timeout_ms: Option<u64>,
    },
    /// Export MCP client configuration snippet.
    Export {
        /// Target client: claude-desktop | claude-code | droid | codex
        client: String,
        /// HTTP MCP listener address (host:port), defaults to 127.0.0.1:3000.
        #[arg(long)]
        addr: Option<String>,
        /// Optional output file path. If omitted, prints to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
enum TrayUserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    Shutdown,
}

struct TrayController {
    proxy: EventLoopProxy<TrayUserEvent>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl TrayController {
    fn request_shutdown(&self) {
        let _ = self.proxy.send_event(TrayUserEvent::Shutdown);
    }

    fn join(mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

struct TrayApp {
    bridge_addr: String,
    http_addr: String,
    shutdown_tx: mpsc::UnboundedSender<()>,
    tray_icon: Option<TrayIcon>,
    quit_id: Option<MenuId>,
    copy_http_endpoint_id: Option<MenuId>,
    copy_bridge_endpoint_id: Option<MenuId>,
    // CLI commands
    copy_claude_code_cli_id: Option<MenuId>,
    copy_droid_cli_id: Option<MenuId>,
    copy_codex_cli_id: Option<MenuId>,
    // Config files
    copy_claude_desktop_config_id: Option<MenuId>,
    copy_claude_code_config_id: Option<MenuId>,
    copy_droid_config_id: Option<MenuId>,
    copy_codex_config_id: Option<MenuId>,
}

impl TrayApp {
    fn new(bridge_addr: String, http_addr: String, shutdown_tx: mpsc::UnboundedSender<()>) -> Self {
        Self {
            bridge_addr,
            http_addr,
            shutdown_tx,
            tray_icon: None,
            quit_id: None,
            copy_http_endpoint_id: None,
            copy_bridge_endpoint_id: None,
            copy_claude_code_cli_id: None,
            copy_droid_cli_id: None,
            copy_codex_cli_id: None,
            copy_claude_desktop_config_id: None,
            copy_claude_code_config_id: None,
            copy_droid_config_id: None,
            copy_codex_config_id: None,
        }
    }

    fn create_tray(&mut self) -> Result<()> {
        let tray_menu = Menu::new();

        let status_item = MenuItem::new("Fernwright MCP: Running", false, None);
        let copy_http_endpoint_item = MenuItem::new("Copy HTTP MCP URL", true, None);
        let copy_bridge_endpoint_item = MenuItem::new("Copy Bridge WS URL", true, None);

        // CLI commands
        let copy_claude_code_cli_item = MenuItem::new("Claude Code", true, None);
        let copy_droid_cli_item = MenuItem::new("Droid", true, None);
        let copy_codex_cli_item = MenuItem::new("Codex", true, None);

        // Config files
        let copy_claude_desktop_config_item = MenuItem::new("Claude Desktop", true, None);
        let copy_claude_code_config_item = MenuItem::new("Claude Code", true, None);
        let copy_droid_config_item = MenuItem::new("Droid", true, None);
        let copy_codex_config_item = MenuItem::new("Codex", true, None);

        let quit_item = MenuItem::new("Quit", true, None);

        let endpoint_submenu = Submenu::with_items(
            "Copy Endpoint",
            true,
            &[&copy_http_endpoint_item, &copy_bridge_endpoint_item],
        )?;

        let cli_submenu = Submenu::with_items(
            "Copy CLI Command",
            true,
            &[
                &copy_claude_code_cli_item,
                &copy_droid_cli_item,
                &copy_codex_cli_item,
            ],
        )?;

        let config_submenu = Submenu::with_items(
            "Copy Config",
            true,
            &[
                &copy_claude_desktop_config_item,
                &copy_claude_code_config_item,
                &copy_droid_config_item,
                &copy_codex_config_item,
            ],
        )?;

        let separator_one = PredefinedMenuItem::separator();
        let separator_two = PredefinedMenuItem::separator();

        tray_menu.append_items(&[
            &status_item,
            &separator_one,
            &endpoint_submenu,
            &cli_submenu,
            &config_submenu,
            &separator_two,
            &quit_item,
        ])?;

        self.quit_id = Some(quit_item.id().clone());
        self.copy_http_endpoint_id = Some(copy_http_endpoint_item.id().clone());
        self.copy_bridge_endpoint_id = Some(copy_bridge_endpoint_item.id().clone());
        // CLI
        self.copy_claude_code_cli_id = Some(copy_claude_code_cli_item.id().clone());
        self.copy_droid_cli_id = Some(copy_droid_cli_item.id().clone());
        self.copy_codex_cli_id = Some(copy_codex_cli_item.id().clone());
        // Config
        self.copy_claude_desktop_config_id = Some(copy_claude_desktop_config_item.id().clone());
        self.copy_claude_code_config_id = Some(copy_claude_code_config_item.id().clone());
        self.copy_droid_config_id = Some(copy_droid_config_item.id().clone());
        self.copy_codex_config_id = Some(copy_codex_config_item.id().clone());

        let icon = load_tray_icon();
        self.tray_icon = Some(
            TrayIconBuilder::new()
                .with_menu(Box::new(tray_menu))
                .with_tooltip("Fernwright MCP Server")
                .with_icon(icon)
                .build()?,
        );

        Ok(())
    }

    fn handle_copy_action(&self, id: &MenuId) {
        // Endpoints
        if self.copy_http_endpoint_id.as_ref() == Some(id) {
            let text = endpoint_url(&self.http_addr);
            copy_to_clipboard(&text);
            return;
        }
        if self.copy_bridge_endpoint_id.as_ref() == Some(id) {
            let text = format!("ws://{}", self.bridge_addr);
            copy_to_clipboard(&text);
            return;
        }

        // CLI commands
        if self.copy_claude_code_cli_id.as_ref() == Some(id) {
            if let Some(cmd) = cli_command(ClientType::ClaudeCode, &self.http_addr) {
                copy_to_clipboard(&cmd);
            }
            return;
        }
        if self.copy_droid_cli_id.as_ref() == Some(id) {
            if let Some(cmd) = cli_command(ClientType::Droid, &self.http_addr) {
                copy_to_clipboard(&cmd);
            }
            return;
        }
        if self.copy_codex_cli_id.as_ref() == Some(id) {
            if let Some(cmd) = cli_command(ClientType::Codex, &self.http_addr) {
                copy_to_clipboard(&cmd);
            }
            return;
        }

        // Config files
        if self.copy_claude_desktop_config_id.as_ref() == Some(id) {
            match config_content(ClientType::ClaudeDesktop, &self.http_addr) {
                Ok(text) => copy_to_clipboard(&text),
                Err(err) => eprintln!("Failed to build Claude Desktop config: {err:#}"),
            }
            return;
        }
        if self.copy_claude_code_config_id.as_ref() == Some(id) {
            match config_content(ClientType::ClaudeCode, &self.http_addr) {
                Ok(text) => copy_to_clipboard(&text),
                Err(err) => eprintln!("Failed to build Claude Code config: {err:#}"),
            }
            return;
        }
        if self.copy_droid_config_id.as_ref() == Some(id) {
            match config_content(ClientType::Droid, &self.http_addr) {
                Ok(text) => copy_to_clipboard(&text),
                Err(err) => eprintln!("Failed to build Droid config: {err:#}"),
            }
            return;
        }
        if self.copy_codex_config_id.as_ref() == Some(id) {
            match config_content(ClientType::Codex, &self.http_addr) {
                Ok(text) => copy_to_clipboard(&text),
                Err(err) => eprintln!("Failed to build Codex config: {err:#}"),
            }
        }
    }
}

impl ApplicationHandler<TrayUserEvent> for TrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: WindowEvent,
    ) {
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if cause == StartCause::Init
            && let Err(err) = self.create_tray()
        {
            eprintln!("Failed to initialize tray: {err:#}");
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: TrayUserEvent) {
        match event {
            TrayUserEvent::Menu(menu_event) => {
                if self.quit_id.as_ref() == Some(&menu_event.id) {
                    let _ = self.shutdown_tx.send(());
                    self.tray_icon.take();
                    event_loop.exit();
                    return;
                }

                self.handle_copy_action(&menu_event.id);
            }
            TrayUserEvent::Tray(_tray_event) => {}
            TrayUserEvent::Shutdown => {
                self.tray_icon.take();
                event_loop.exit();
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Export {
            client,
            addr,
            output,
        }) => run_export(&client, addr, output),
        Some(Commands::Serve {
            bridge_addr,
            http_addr,
            request_timeout_ms,
            no_tray,
        }) => run_server(bridge_addr, http_addr, request_timeout_ms, no_tray).await,
        Some(Commands::ServeStdio {
            bridge_addr,
            request_timeout_ms,
        }) => run_stdio_bridge_server(bridge_addr, request_timeout_ms).await,
        None => run_server(None, None, None, false).await,
    }
}

fn run_export(client: &str, addr: Option<String>, output: Option<PathBuf>) -> Result<()> {
    let client = ClientType::parse(client)?;
    let http_addr = addr.unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_owned());

    if let Some(path) = output {
        let path = write_config_file(client, &http_addr, Some(path))?;
        println!("{}", path.display());
    } else {
        let rendered = render_export_text(client, &http_addr)?;
        println!("{rendered}");
    }

    Ok(())
}

async fn run_server(
    bridge_addr: Option<String>,
    http_addr: Option<String>,
    request_timeout_ms: Option<u64>,
    no_tray: bool,
) -> Result<()> {
    let bridge_addr = resolve_value(
        bridge_addr,
        "PLAYWRIGHT_MCP_BRIDGE_ADDR",
        DEFAULT_BRIDGE_ADDR,
    );
    let http_addr = resolve_value(http_addr, "PLAYWRIGHT_MCP_HTTP_ADDR", DEFAULT_HTTP_ADDR);
    let request_timeout_ms = request_timeout_ms
        .or_else(|| {
            env::var("PLAYWRIGHT_MCP_REQUEST_TIMEOUT_MS")
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok())
        })
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);

    let bridge = BridgeServer::new(Duration::from_millis(request_timeout_ms));
    let bridge_for_ws = bridge.clone();
    let bridge_for_http = bridge.clone();

    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel::<()>();
    let shutdown_ct = CancellationToken::new();

    let bridge_addr_for_ws = bridge_addr.clone();
    let ws_task = tokio::spawn(async move {
        if let Err(err) = bridge_for_ws.run_ws_listener(&bridge_addr_for_ws).await {
            eprintln!("WebSocket listener failed: {err:#}");
        }
    });

    let http_addr_for_task = http_addr.clone();
    let http_task_ct = shutdown_ct.child_token();
    let mut http_task = tokio::spawn(async move {
        if let Err(err) =
            mcp_http::run_http_server(bridge_for_http, &http_addr_for_task, http_task_ct).await
        {
            eprintln!("HTTP MCP server failed: {err:#}");
        }
    });

    let tray_controller = if no_tray {
        None
    } else {
        match spawn_tray(bridge_addr.clone(), http_addr.clone(), shutdown_tx.clone()) {
            Ok(controller) => Some(controller),
            Err(err) => {
                eprintln!("Tray disabled: {err:#}");
                None
            }
        }
    };

    eprintln!("Fernwright MCP server started");
    eprintln!("  WebSocket bridge: ws://{}", bridge_addr);
    eprintln!("  HTTP MCP endpoint: {}", endpoint_url(&http_addr));
    eprintln!("  Export example: fernwright-mcp export claude-desktop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Shutdown requested by Ctrl+C");
        }
        _ = shutdown_rx.recv() => {
            eprintln!("Shutdown requested by tray menu");
        }
    }

    shutdown_ct.cancel();
    ws_task.abort();

    if tokio::time::timeout(Duration::from_secs(3), &mut http_task)
        .await
        .is_err()
    {
        http_task.abort();
    }

    if let Some(tray) = tray_controller {
        tray.request_shutdown();
        tray.join();
    }

    Ok(())
}

async fn run_stdio_bridge_server(
    bridge_addr: Option<String>,
    request_timeout_ms: Option<u64>,
) -> Result<()> {
    let bridge_addr = resolve_value(
        bridge_addr,
        "PLAYWRIGHT_MCP_BRIDGE_ADDR",
        DEFAULT_BRIDGE_ADDR,
    );
    let request_timeout_ms = request_timeout_ms
        .or_else(|| {
            env::var("PLAYWRIGHT_MCP_REQUEST_TIMEOUT_MS")
                .ok()
                .and_then(|raw| raw.parse::<u64>().ok())
        })
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);

    let bridge = BridgeServer::new(Duration::from_millis(request_timeout_ms));
    let bridge_for_ws = bridge.clone();
    let bridge_addr_for_ws = bridge_addr.clone();

    let ws_task = tokio::spawn(async move {
        if let Err(err) = bridge_for_ws.run_ws_listener(&bridge_addr_for_ws).await {
            eprintln!("WebSocket listener failed: {err:#}");
        }
    });

    let result = mcp_server::run_stdio_server(bridge).await;
    ws_task.abort();
    result
}

fn resolve_value(cli_value: Option<String>, env_name: &str, default: &str) -> String {
    cli_value
        .or_else(|| env::var(env_name).ok())
        .unwrap_or_else(|| default.to_owned())
}

fn spawn_tray(
    bridge_addr: String,
    http_addr: String,
    shutdown_tx: mpsc::UnboundedSender<()>,
) -> Result<TrayController> {
    let (proxy_tx, proxy_rx) = std_mpsc::channel::<EventLoopProxy<TrayUserEvent>>();

    let join = std::thread::Builder::new()
        .name("fernwright-mcp-tray".to_owned())
        .spawn(move || {
            if let Err(err) = run_tray_loop(bridge_addr, http_addr, shutdown_tx, proxy_tx) {
                eprintln!("Tray loop terminated: {err:#}");
            }
        })?;

    let proxy = proxy_rx
        .recv_timeout(Duration::from_secs(3))
        .map_err(|_| anyhow!("failed to initialize tray event loop"))?;

    Ok(TrayController {
        proxy,
        join: Some(join),
    })
}

fn run_tray_loop(
    bridge_addr: String,
    http_addr: String,
    shutdown_tx: mpsc::UnboundedSender<()>,
    proxy_tx: std_mpsc::Sender<EventLoopProxy<TrayUserEvent>>,
) -> Result<()> {
    let mut event_loop_builder = EventLoop::<TrayUserEvent>::with_user_event();
    #[cfg(target_os = "windows")]
    {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        event_loop_builder.with_any_thread(true);
    }
    let event_loop = event_loop_builder.build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    proxy_tx
        .send(proxy.clone())
        .map_err(|_| anyhow!("failed to send tray proxy"))?;

    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = tray_proxy.send_event(TrayUserEvent::Tray(event));
    }));

    let menu_proxy = proxy;
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(TrayUserEvent::Menu(event));
    }));

    let mut app = TrayApp::new(bridge_addr, http_addr, shutdown_tx);
    event_loop
        .run_app(&mut app)
        .context("tray event loop failed")?;
    Ok(())
}

fn copy_to_clipboard(text: &str) {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.to_owned())) {
        Ok(()) => eprintln!("Copied to clipboard"),
        Err(err) => eprintln!("Clipboard copy failed: {err}"),
    }
}

fn load_tray_icon() -> Icon {
    match load_svg_tray_icon(EMBEDDED_TRAY_ICON_SVG) {
        Ok(icon) => return icon,
        Err(err) => eprintln!("Failed to render embedded SVG tray icon: {err:#}"),
    }

    #[cfg(windows)]
    {
        for path in icon_candidates() {
            if !path.exists() {
                continue;
            }
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            if ext == "svg" {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(icon) = load_svg_tray_icon(&bytes) {
                        return icon;
                    }
                }
            } else if let Ok(icon) = Icon::from_path(&path, None) {
                return icon;
            }
        }
    }

    // Fallback 1x1 icon.
    Icon::from_rgba(vec![0x2D, 0x9C, 0xDB, 0xFF], 1, 1).expect("valid fallback tray icon")
}

fn load_svg_tray_icon(svg_bytes: &[u8]) -> Result<Icon> {
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg_bytes, &opt)
        .map_err(|err| anyhow!("invalid SVG content: {err}"))?;

    let size = TRAY_ICON_TARGET_SIZE;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| anyhow!("failed to create pixmap"))?;

    let svg_size = tree.size();
    let scale = (size as f32 / svg_size.width()).min(size as f32 / svg_size.height());
    let scaled_width = svg_size.width() * scale;
    let scaled_height = svg_size.height() * scale;
    let transform = resvg::tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (size as f32 - scaled_width) / 2.0,
        (size as f32 - scaled_height) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();

    Icon::from_rgba(rgba, size, size)
        .map_err(|err| anyhow!("failed to create tray icon from SVG raster: {err}"))
}

fn icon_candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    paths.push(PathBuf::from("icon.svg"));
    paths.push(PathBuf::from("icon.ico"));
    paths.push(PathBuf::from("icon.png"));
    paths.push(PathBuf::from("assets\\icon.svg"));
    paths.push(PathBuf::from("assets\\icon.ico"));
    paths.push(PathBuf::from("assets\\icon.png"));
    paths.push(PathBuf::from("tray-icon\\examples\\icon.png"));

    if let Some(data_dir) = dirs::data_dir() {
        paths.push(data_dir.join("fernwright-mcp").join("icon.svg"));
        paths.push(data_dir.join("fernwright-mcp").join("icon.ico"));
        paths.push(data_dir.join("fernwright-mcp").join("icon.png"));
    }
    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("fernwright-mcp").join("icon.svg"));
        paths.push(config_dir.join("fernwright-mcp").join("icon.ico"));
        paths.push(config_dir.join("fernwright-mcp").join("icon.png"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_icon_pixels_are_demultiplied_before_creating_icon() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">
            <rect width="10" height="10" fill="#ffffff80" />
        </svg>"##;

        let opt = usvg::Options::default();
        let tree = usvg::Tree::from_data(svg, &opt).expect("valid svg");
        let size = TRAY_ICON_TARGET_SIZE;
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size).expect("valid pixmap");

        let svg_size = tree.size();
        let scale = (size as f32 / svg_size.width()).min(size as f32 / svg_size.height());
        let scaled_width = svg_size.width() * scale;
        let scaled_height = svg_size.height() * scale;
        let transform = resvg::tiny_skia::Transform::from_row(
            scale,
            0.0,
            0.0,
            scale,
            (size as f32 - scaled_width) / 2.0,
            (size as f32 - scaled_height) / 2.0,
        );

        resvg::render(&tree, transform, &mut pixmap.as_mut());

        let rgba: Vec<u8> = pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| {
                let color = pixel.demultiply();
                [color.red(), color.green(), color.blue(), color.alpha()]
            })
            .collect();

        let center = ((size as usize / 2) * size as usize + size as usize / 2) * 4;
        assert!(rgba[center] > 240, "red channel should stay bright");
        assert!(rgba[center + 1] > 240, "green channel should stay bright");
        assert!(rgba[center + 2] > 240, "blue channel should stay bright");
        assert!(rgba[center + 3] > 110 && rgba[center + 3] < 145);
    }
}
