use anyhow::Result;
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::bridge::BridgeServer;
use crate::search::{
    ExtractStructuredDataArgs, FilterTabsArgs, FindInPageArgs, SearchPageContentArgs,
    SearchTabsArgs,
};
use crate::search_service::SearchService;

#[derive(Clone)]
pub struct BridgeMcpServer {
    bridge: BridgeServer,
    tool_router: ToolRouter<Self>,
    search_service: SearchService,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct ListTabsArgs {
    #[serde(default)]
    client_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct NavigateTabArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    url: String,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct ActivateTabArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    selector: String,
    #[serde(default)]
    timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct FillArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    selector: String,
    value: String,
    #[serde(default)]
    timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct PressKeyArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    selector: String,
    key: String,
    #[serde(default)]
    timeout_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct EvaluateJsArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    script: String,
    #[serde(default)]
    args: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct ExtractTextArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    max_length: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct WaitForArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    timeout_ms: Option<i64>,
    #[serde(default)]
    interval_ms: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct CaptureScreenshotArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    quality: Option<i64>,
    #[serde(default)]
    include_data_url: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct ExtractPageContextArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    #[serde(default)]
    context_type: Option<String>,
    #[serde(default)]
    max_elements: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct GetPageHtmlArgs {
    #[serde(default)]
    client_id: Option<String>,
    tab_id: i64,
    #[serde(default)]
    selector: Option<String>,
    #[serde(default)]
    max_length: Option<i64>,
    #[serde(default)]
    strip_scripts: Option<bool>,
    #[serde(default)]
    strip_styles: Option<bool>,
}

#[tool_router]
impl BridgeMcpServer {
    pub fn new(bridge: BridgeServer) -> Self {
        Self {
            bridge: bridge.clone(),
            tool_router: Self::tool_router(),
            search_service: SearchService::new(bridge),
        }
    }

    fn validate_non_empty(value: &str, field: &str) -> Result<(), McpError> {
        if value.trim().is_empty() {
            return Err(McpError::invalid_params(
                format!("'{field}' must be a non-empty string"),
                None,
            ));
        }
        Ok(())
    }

    fn normalize_condition(condition: Option<String>) -> Result<String, McpError> {
        let condition = condition
            .unwrap_or_else(|| "element".to_owned())
            .trim()
            .to_ascii_lowercase();
        let supported = ["element", "text", "url", "function"];
        if !supported.contains(&condition.as_str()) {
            return Err(McpError::invalid_params(
                format!("'condition' must be one of: {}", supported.join(", ")),
                None,
            ));
        }
        Ok(condition)
    }

    fn success_result(value: Value) -> Result<CallToolResult, McpError> {
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    fn tool_error_result(message: impl AsRef<str>) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::error(vec![Content::text(format!(
            "Tool error: {}",
            message.as_ref()
        ))]))
    }

    fn html_snapshot_result(client_id: &str, payload: &Value) -> Result<CallToolResult, McpError> {
        let html = payload
            .get("html")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let summary = json!({
            "client_id": client_id,
            "selector": payload.get("selector").cloned().unwrap_or(Value::Null),
            "truncated": payload.get("truncated").cloned().unwrap_or(Value::Null),
            "total_length": payload.get("totalLength").cloned().unwrap_or(Value::Null),
            "max_length": payload.get("maxLength").cloned().unwrap_or(Value::Null),
            "title": payload.get("title").cloned().unwrap_or(Value::Null),
            "url": payload.get("url").cloned().unwrap_or(Value::Null),
        });
        let summary_text =
            serde_json::to_string_pretty(&summary).unwrap_or_else(|_| summary.to_string());

        Ok(CallToolResult::success(vec![
            Content::text(format!("HTML snapshot metadata:\n{summary_text}")),
            Content::text(html),
        ]))
    }

    async fn bridge_request(
        &self,
        client_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<CallToolResult, McpError> {
        match self.bridge.request(client_id, method, params).await {
            Ok(response) => Self::success_result(json!({
                "client_id": response.client_id,
                "result": response.payload,
            })),
            Err(err) => Self::tool_error_result(format!("{err:#}")),
        }
    }

    #[tool(
        name = "list_clients",
        description = "List connected browser extension clients and cached tab snapshots."
    )]
    async fn list_clients(&self) -> Result<CallToolResult, McpError> {
        let clients = self.bridge.list_clients().await;
        Self::success_result(json!({ "clients": clients }))
    }

    #[tool(
        name = "list_tabs",
        description = "List all tabs available from a connected browser client."
    )]
    async fn list_tabs(
        &self,
        Parameters(args): Parameters<ListTabsArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.bridge_request(args.client_id.as_deref(), "listTabs", json!({}))
            .await
    }

    #[tool(
        name = "navigate_tab",
        description = "Navigate an existing tab to a URL."
    )]
    async fn navigate_tab(
        &self,
        Parameters(args): Parameters<NavigateTabArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.url, "url")?;

        self.bridge_request(
            args.client_id.as_deref(),
            "navigate",
            json!({
                "tabId": args.tab_id,
                "url": args.url,
            }),
        )
        .await
    }

    #[tool(name = "activate_tab", description = "Focus a tab and its window.")]
    async fn activate_tab(
        &self,
        Parameters(args): Parameters<ActivateTabArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.bridge_request(
            args.client_id.as_deref(),
            "activateTab",
            json!({ "tabId": args.tab_id }),
        )
        .await
    }

    #[tool(name = "click", description = "Wait for a selector and click it.")]
    async fn click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.selector, "selector")?;

        let mut params = json!({
            "tabId": args.tab_id,
            "selector": args.selector,
        });
        if let Some(timeout_ms) = args.timeout_ms {
            params["timeoutMs"] = json!(timeout_ms);
        }

        self.bridge_request(args.client_id.as_deref(), "click", params)
            .await
    }

    #[tool(
        name = "fill",
        description = "Set value for an input-like element and dispatch change events."
    )]
    async fn fill(
        &self,
        Parameters(args): Parameters<FillArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.selector, "selector")?;
        Self::validate_non_empty(&args.value, "value")?;

        let mut params = json!({
            "tabId": args.tab_id,
            "selector": args.selector,
            "value": args.value,
        });
        if let Some(timeout_ms) = args.timeout_ms {
            params["timeoutMs"] = json!(timeout_ms);
        }

        self.bridge_request(args.client_id.as_deref(), "fill", params)
            .await
    }

    #[tool(
        name = "press_key",
        description = "Dispatch keydown/keyup on an element."
    )]
    async fn press_key(
        &self,
        Parameters(args): Parameters<PressKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.selector, "selector")?;
        Self::validate_non_empty(&args.key, "key")?;

        let mut params = json!({
            "tabId": args.tab_id,
            "selector": args.selector,
            "key": args.key,
        });
        if let Some(timeout_ms) = args.timeout_ms {
            params["timeoutMs"] = json!(timeout_ms);
        }

        self.bridge_request(args.client_id.as_deref(), "press", params)
            .await
    }

    #[tool(
        name = "evaluate_js",
        description = "Evaluate JavaScript in the tab's DOM context."
    )]
    async fn evaluate_js(
        &self,
        Parameters(args): Parameters<EvaluateJsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.script, "script")?;

        self.bridge_request(
            args.client_id.as_deref(),
            "evaluate",
            json!({
                "tabId": args.tab_id,
                "script": args.script,
                "args": args.args,
            }),
        )
        .await
    }

    #[tool(
        name = "extract_text",
        description = "Extract visible text from an element selector."
    )]
    async fn extract_text(
        &self,
        Parameters(args): Parameters<ExtractTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let selector = args.selector.unwrap_or_else(|| "body".to_owned());
        let mut params = json!({
            "tabId": args.tab_id,
            "selector": selector,
        });
        if let Some(max_length) = args.max_length {
            params["maxLength"] = json!(max_length);
        }

        self.bridge_request(args.client_id.as_deref(), "extractText", params)
            .await
    }

    #[tool(
        name = "wait_for",
        description = "Wait for a page condition to be met. condition: element | text | url | function."
    )]
    async fn wait_for(
        &self,
        Parameters(args): Parameters<WaitForArgs>,
    ) -> Result<CallToolResult, McpError> {
        let condition = Self::normalize_condition(args.condition)?;
        if condition == "element" && args.selector.as_deref().unwrap_or("").trim().is_empty() {
            return Err(McpError::invalid_params(
                "'selector' is required when condition is 'element'",
                None,
            ));
        }
        if condition == "text" && args.text.as_deref().unwrap_or("").is_empty() {
            return Err(McpError::invalid_params(
                "'text' is required when condition is 'text'",
                None,
            ));
        }
        if condition == "url" && args.text.as_deref().unwrap_or("").is_empty() {
            return Err(McpError::invalid_params(
                "'text' is required when condition is 'url'",
                None,
            ));
        }
        if condition == "function" && args.script.as_deref().unwrap_or("").trim().is_empty() {
            return Err(McpError::invalid_params(
                "'script' is required when condition is 'function'",
                None,
            ));
        }

        let mut params = json!({
            "tabId": args.tab_id,
            "condition": condition,
        });

        if let Some(selector) = args.selector {
            params["selector"] = json!(selector);
        }
        if let Some(text) = args.text {
            params["text"] = json!(text);
        }
        if let Some(script) = args.script {
            params["script"] = json!(script);
        }
        if let Some(timeout_ms) = args.timeout_ms {
            params["timeoutMs"] = json!(timeout_ms);
        }
        if let Some(interval_ms) = args.interval_ms {
            params["intervalMs"] = json!(interval_ms);
        }

        self.bridge_request(args.client_id.as_deref(), "waitFor", params)
            .await
    }

    #[tool(
        name = "capture_screenshot",
        description = "Capture a screenshot from the target tab (activates the tab first)."
    )]
    async fn capture_screenshot(
        &self,
        Parameters(args): Parameters<CaptureScreenshotArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(format) = args.format.as_deref() {
            let normalized = format.trim().to_ascii_lowercase();
            if normalized != "png" && normalized != "jpeg" {
                return Err(McpError::invalid_params(
                    "'format' must be 'png' or 'jpeg'",
                    None,
                ));
            }
        }

        let mut params = json!({
            "tabId": args.tab_id,
        });
        if let Some(format) = args.format {
            params["format"] = json!(format);
        }
        if let Some(quality) = args.quality {
            params["quality"] = json!(quality);
        }
        if let Some(include_data_url) = args.include_data_url {
            params["includeDataUrl"] = json!(include_data_url);
        }

        self.bridge_request(args.client_id.as_deref(), "captureScreenshot", params)
            .await
    }

    #[tool(
        name = "extract_page_context",
        description = "Extract structured page context. context_type: all | links | buttons | inputs | forms | metadata."
    )]
    async fn extract_page_context(
        &self,
        Parameters(args): Parameters<ExtractPageContextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut params = json!({
            "tabId": args.tab_id,
        });

        if let Some(context_type) = args.context_type {
            let normalized = context_type.trim().to_ascii_lowercase();
            let supported = ["all", "links", "buttons", "inputs", "forms", "metadata"];
            if !supported.contains(&normalized.as_str()) {
                return Err(McpError::invalid_params(
                    format!("'context_type' must be one of: {}", supported.join(", ")),
                    None,
                ));
            }
            params["contextType"] = json!(normalized);
        }

        if let Some(max_elements) = args.max_elements {
            params["maxElements"] = json!(max_elements);
        }

        self.bridge_request(args.client_id.as_deref(), "extractPageContext", params)
            .await
    }

    #[tool(
        name = "get_page_html",
        description = "Get HTML snapshot for a selector (default 'html'). Supports max_length and optional script/style stripping."
    )]
    async fn get_page_html(
        &self,
        Parameters(args): Parameters<GetPageHtmlArgs>,
    ) -> Result<CallToolResult, McpError> {
        if let Some(selector) = args.selector.as_deref() {
            if selector.trim().is_empty() {
                return Err(McpError::invalid_params("'selector' cannot be empty", None));
            }
        }

        if let Some(max_length) = args.max_length {
            if max_length <= 0 {
                return Err(McpError::invalid_params(
                    "'max_length' must be a positive integer",
                    None,
                ));
            }
        }

        let mut params = json!({
            "tabId": args.tab_id,
        });
        if let Some(selector) = args.selector {
            params["selector"] = json!(selector);
        }
        if let Some(max_length) = args.max_length {
            params["maxLength"] = json!(max_length);
        }
        if let Some(strip_scripts) = args.strip_scripts {
            params["stripScripts"] = json!(strip_scripts);
        }
        if let Some(strip_styles) = args.strip_styles {
            params["stripStyles"] = json!(strip_styles);
        }

        match self
            .bridge
            .request(args.client_id.as_deref(), "getPageHtml", params)
            .await
        {
            Ok(response) => Self::html_snapshot_result(&response.client_id, &response.payload),
            Err(err) => Self::tool_error_result(format!("{err:#}")),
        }
    }

    #[tool(
        name = "search_tabs",
        description = "Search cached tabs metadata (title/url) with regex and relevance ranking."
    )]
    async fn search_tabs(
        &self,
        Parameters(args): Parameters<SearchTabsArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.query, "query")?;

        if args.max_results == 0 {
            return Err(McpError::invalid_params(
                "'max_results' must be a positive integer",
                None,
            ));
        }

        match self.search_service.search_tabs(args).await {
            Ok(results) => Self::success_result(json!({ "results": results })),
            Err(err) => Self::tool_error_result(format!("Search failed: {err:#}")),
        }
    }

    #[tool(
        name = "search_page_content",
        description = "Search in tab content. scope=cached uses tab snapshot, scope=full fetches HTML via getPageHtml."
    )]
    async fn search_page_content(
        &self,
        Parameters(args): Parameters<SearchPageContentArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.query, "query")?;

        if args.max_matches == 0 {
            return Err(McpError::invalid_params(
                "'max_matches' must be a positive integer",
                None,
            ));
        }

        match self.search_service.search_page_content(args).await {
            Ok(results) => Self::success_result(json!({ "results": results })),
            Err(err) => Self::tool_error_result(format!("Page search failed: {err:#}")),
        }
    }

    #[tool(
        name = "filter_tabs",
        description = "Filter tabs by URL/title pattern, domain and active/pinned/incognito status."
    )]
    async fn filter_tabs(
        &self,
        Parameters(args): Parameters<FilterTabsArgs>,
    ) -> Result<CallToolResult, McpError> {
        match self.search_service.filter_tabs(args).await {
            Ok(result) => Self::success_result(json!(result)),
            Err(err) => Self::tool_error_result(format!("Filter failed: {err:#}")),
        }
    }

    #[tool(
        name = "extract_structured_data",
        description = "Extract structured tables/lists/headings from page HTML using existing getPageHtml."
    )]
    async fn extract_structured_data(
        &self,
        Parameters(args): Parameters<ExtractStructuredDataArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.max_items == 0 {
            return Err(McpError::invalid_params(
                "'max_items' must be a positive integer",
                None,
            ));
        }

        match self.search_service.extract_structured_data(args).await {
            Ok(data) => Self::success_result(data),
            Err(err) => Self::tool_error_result(format!("Extraction failed: {err:#}")),
        }
    }

    #[tool(
        name = "find_in_page",
        description = "Combined retrieval: search cached tabs first, optionally deep-search HTML content of matched tabs."
    )]
    async fn find_in_page(
        &self,
        Parameters(args): Parameters<FindInPageArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::validate_non_empty(&args.query, "query")?;

        let search_args = SearchTabsArgs {
            client_id: args.client_id.clone(),
            query: args.query.clone(),
            fields: args.fields.clone(),
            use_regex: args.use_regex,
            case_sensitive: args.case_sensitive,
            max_results: args.max_results,
        };

        let mut results = match self.search_service.search_tabs(search_args).await {
            Ok(results) => results,
            Err(err) => return Self::tool_error_result(format!("Find failed: {err:#}")),
        };

        let mut warnings = Vec::new();
        if args.deep_search {
            for result in &mut results {
                let content_args = SearchPageContentArgs {
                    client_id: Some(result.client_id.clone()),
                    tab_id: result.tab_id,
                    query: args.query.clone(),
                    scope: "full".to_owned(),
                    selector: args.selector.clone(),
                    use_regex: args.use_regex,
                    case_sensitive: args.case_sensitive,
                    context_chars: args.context_chars,
                    max_matches: args.max_content_matches,
                };

                match self.search_service.search_page_content(content_args).await {
                    Ok(mut content_results) => {
                        if let Some(content_result) = content_results.pop() {
                            result.relevance_score += content_result.matches.len() as f64;
                            result.matches.extend(content_result.matches);
                        }
                    }
                    Err(err) => warnings.push(json!({
                        "client_id": result.client_id,
                        "tab_id": result.tab_id,
                        "error": format!("{err:#}"),
                    })),
                }
            }

            results.sort_by(|a, b| {
                b.relevance_score
                    .partial_cmp(&a.relevance_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        Self::success_result(json!({
            "results": results,
            "deep_search": args.deep_search,
            "warnings": warnings,
        }))
    }
}

#[tool_handler]
impl ServerHandler for BridgeMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "fernwright-mcp",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
    }
}

#[allow(dead_code)]
pub async fn run_stdio_server(bridge: BridgeServer) -> Result<()> {
    let server = BridgeMcpServer::new(bridge);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
