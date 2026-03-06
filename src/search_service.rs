use std::cmp::Ordering;

use anyhow::{Result, anyhow};
use regex::{Regex, RegexBuilder};
use scraper::{ElementRef, Html, Selector};
use serde_json::{Value, json};
use url::Url;

use crate::bridge::BridgeServer;
use crate::search::{
    ContentMatch, ExtractStructuredDataArgs, FilterTabsArgs, FilteredTab, FilteredTabsResult,
    SearchPageContentArgs, SearchResult, SearchTabsArgs,
};

const MAX_HTML_LENGTH: i64 = 2_000_000;
const TAB_MATCH_CONTEXT_CHARS: usize = 50;
const MAX_TAB_MATCHES_PER_RESULT: usize = 30;

#[derive(Clone)]
pub struct SearchService {
    bridge: BridgeServer,
}

impl SearchService {
    pub fn new(bridge: BridgeServer) -> Self {
        Self { bridge }
    }

    pub async fn search_tabs(&self, args: SearchTabsArgs) -> Result<Vec<SearchResult>> {
        if args.max_results == 0 {
            return Ok(Vec::new());
        }

        let clients = self.bridge.list_clients().await;
        if clients.is_empty() {
            return Err(anyhow!(
                "No extension clients connected. Load the extension and verify the WebSocket URL."
            ));
        }

        let pattern = Self::build_pattern(&args.query, args.use_regex, args.case_sensitive)?;
        let fields = SearchField::parse_many(&args.fields)?;
        let mut all_results = Vec::new();

        for client in clients {
            if let Some(target_client) = args.client_id.as_deref()
                && client.client_id != target_client
            {
                continue;
            }

            for tab in &client.tabs {
                let tab = match TabInfo::from_value(tab, &client.client_id) {
                    Some(tab) => tab,
                    None => continue,
                };

                let mut matches = Vec::new();
                let mut relevance = 0.0_f64;

                for field in &fields {
                    if matches.len() >= MAX_TAB_MATCHES_PER_RESULT {
                        break;
                    }

                    let remaining = MAX_TAB_MATCHES_PER_RESULT - matches.len();
                    let (value, field_name, weight) = field.value_name_weight(&tab);
                    let field_matches = Self::find_matches(
                        value,
                        &pattern,
                        field_name,
                        TAB_MATCH_CONTEXT_CHARS,
                        remaining,
                    );
                    if !field_matches.is_empty() {
                        relevance += field_matches.len() as f64 * weight;
                        matches.extend(field_matches);
                    }
                }

                if !matches.is_empty() {
                    all_results.push(SearchResult {
                        client_id: tab.client_id,
                        tab_id: tab.tab_id,
                        window_id: tab.window_id,
                        title: tab.title,
                        url: tab.url,
                        matches,
                        relevance_score: relevance,
                    });
                }
            }
        }

        all_results.sort_by(|a, b| {
            b.relevance_score
                .partial_cmp(&a.relevance_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.client_id.cmp(&b.client_id))
                .then_with(|| a.tab_id.cmp(&b.tab_id))
        });
        all_results.truncate(args.max_results);
        Ok(all_results)
    }

    pub async fn search_page_content(
        &self,
        args: SearchPageContentArgs,
    ) -> Result<Vec<SearchResult>> {
        if args.max_matches == 0 {
            return Ok(Vec::new());
        }

        let scope = args.scope.trim().to_ascii_lowercase();
        let pattern = Self::build_pattern(&args.query, args.use_regex, args.case_sensitive)?;

        match scope.as_str() {
            "cached" => self.search_page_content_cached(args, &pattern).await,
            "full" => self.search_page_content_full(args, &pattern).await,
            _ => Err(anyhow!(
                "Unsupported scope '{}'. Expected one of: cached, full",
                args.scope
            )),
        }
    }

    pub async fn filter_tabs(&self, args: FilterTabsArgs) -> Result<FilteredTabsResult> {
        let clients = self.bridge.list_clients().await;
        if clients.is_empty() {
            return Err(anyhow!(
                "No extension clients connected. Load the extension and verify the WebSocket URL."
            ));
        }

        let url_pattern = match args.url_pattern.as_deref().map(str::trim) {
            Some("") => {
                return Err(anyhow!("'url_pattern' cannot be empty when provided"));
            }
            Some(pattern) => Some(Self::build_pattern(
                pattern,
                args.use_regex,
                args.case_sensitive,
            )?),
            None => None,
        };

        let title_pattern = match args.title_pattern.as_deref().map(str::trim) {
            Some("") => {
                return Err(anyhow!("'title_pattern' cannot be empty when provided"));
            }
            Some(pattern) => Some(Self::build_pattern(
                pattern,
                args.use_regex,
                args.case_sensitive,
            )?),
            None => None,
        };

        let target_domain = args
            .domain
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());

        let mut filtered_tabs = Vec::new();
        let mut total_tabs = 0usize;

        for client in clients {
            if let Some(target_client) = args.client_id.as_deref()
                && client.client_id != target_client
            {
                continue;
            }

            for tab in &client.tabs {
                let tab = match TabInfo::from_value(tab, &client.client_id) {
                    Some(tab) => tab,
                    None => continue,
                };
                total_tabs += 1;

                let mut reasons = Vec::new();

                if let Some(pattern) = &url_pattern {
                    if pattern.regex.is_match(&tab.url) {
                        reasons.push("url_pattern".to_owned());
                    } else {
                        continue;
                    }
                }

                if let Some(pattern) = &title_pattern {
                    if pattern.regex.is_match(&tab.title) {
                        reasons.push("title_pattern".to_owned());
                    } else {
                        continue;
                    }
                }

                if let Some(domain) = target_domain.as_deref() {
                    if Self::domain_matches(&tab.domain, domain) {
                        reasons.push("domain".to_owned());
                    } else {
                        continue;
                    }
                }

                if args.active_only && !tab.active {
                    continue;
                }
                if args.active_only {
                    reasons.push("active_only".to_owned());
                }

                if args.pinned_only && !tab.pinned {
                    continue;
                }
                if args.pinned_only {
                    reasons.push("pinned_only".to_owned());
                }

                if args.incognito_only && !tab.incognito {
                    continue;
                }
                if args.incognito_only {
                    reasons.push("incognito_only".to_owned());
                }

                if reasons.is_empty() {
                    reasons.push("no_pattern_filter".to_owned());
                }

                filtered_tabs.push(FilteredTab {
                    client_id: tab.client_id,
                    tab_id: tab.tab_id,
                    window_id: tab.window_id,
                    title: tab.title,
                    url: tab.url,
                    domain: tab.domain,
                    active: tab.active,
                    pinned: tab.pinned,
                    incognito: tab.incognito,
                    match_reason: reasons,
                });
            }
        }

        filtered_tabs.sort_by(|a, b| {
            a.client_id
                .cmp(&b.client_id)
                .then_with(|| a.window_id.cmp(&b.window_id))
                .then_with(|| a.tab_id.cmp(&b.tab_id))
        });

        let applied_filters = json!({
            "url_pattern": args.url_pattern,
            "title_pattern": args.title_pattern,
            "domain": args.domain,
            "active_only": args.active_only,
            "pinned_only": args.pinned_only,
            "incognito_only": args.incognito_only,
            "use_regex": args.use_regex,
            "case_sensitive": args.case_sensitive,
        });

        Ok(FilteredTabsResult {
            client_id: args.client_id.unwrap_or_else(|| "all".to_owned()),
            total_tabs,
            filtered_tabs,
            applied_filters,
        })
    }

    pub async fn extract_structured_data(&self, args: ExtractStructuredDataArgs) -> Result<Value> {
        let extract_type = args.extract_type.trim().to_ascii_lowercase();
        let supported = ["tables", "lists", "headings", "all"];
        if !supported.contains(&extract_type.as_str()) {
            return Err(anyhow!(
                "'extract_type' must be one of: {}",
                supported.join(", ")
            ));
        }

        let html_response = self
            .bridge
            .request(
                args.client_id.as_deref(),
                "getPageHtml",
                json!({
                    "tabId": args.tab_id,
                    "selector": args.selector.as_deref().unwrap_or("body"),
                    "maxLength": MAX_HTML_LENGTH,
                    "stripScripts": true,
                    "stripStyles": true,
                }),
            )
            .await?;

        let html = html_response
            .payload
            .get("html")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("No HTML content returned"))?;

        let document = Html::parse_document(html);
        let mut result = json!({
            "client_id": html_response.client_id,
            "tab_id": args.tab_id,
            "extract_type": extract_type,
        });

        if extract_type == "tables" || extract_type == "all" {
            result["tables"] = json!(Self::extract_tables(
                &document,
                args.max_items,
                args.include_metadata
            ));
        }

        if extract_type == "lists" || extract_type == "all" {
            result["lists"] = json!(Self::extract_lists(
                &document,
                args.max_items,
                args.include_metadata
            ));
        }

        if extract_type == "headings" || extract_type == "all" {
            result["headings"] = json!(Self::extract_headings(&document, args.max_items));
        }

        if args.include_metadata {
            let element_count = document
                .root_element()
                .descendants()
                .filter(|node| node.value().as_element().is_some())
                .count();
            result["metadata"] = json!({
                "title": Self::extract_title(&document),
                "url": html_response.payload.get("url").cloned().unwrap_or(Value::Null),
                "selector": args.selector.unwrap_or_else(|| "body".to_owned()),
                "element_count": element_count,
                "truncated": html_response.payload.get("truncated").cloned().unwrap_or(Value::Null),
                "total_length": html_response.payload.get("totalLength").cloned().unwrap_or(Value::Null),
            });
        }

        Ok(result)
    }

    async fn search_page_content_cached(
        &self,
        args: SearchPageContentArgs,
        pattern: &CompiledPattern,
    ) -> Result<Vec<SearchResult>> {
        let tab = self
            .get_cached_tab(args.client_id.as_deref(), args.tab_id)
            .await
            .ok_or_else(|| {
                anyhow!(
                    "Tab {} not found in cached snapshots. Use scope='full' to fetch directly.",
                    args.tab_id
                )
            })?;

        let title_matches = Self::find_matches(
            &tab.title,
            pattern,
            "title",
            args.context_chars,
            args.max_matches,
        );
        let remaining = args.max_matches.saturating_sub(title_matches.len());
        let url_matches = if remaining > 0 {
            Self::find_matches(&tab.url, pattern, "url", args.context_chars, remaining)
        } else {
            Vec::new()
        };

        let mut matches = title_matches;
        matches.extend(url_matches);

        if matches.is_empty() {
            return Ok(Vec::new());
        }

        Ok(vec![SearchResult {
            client_id: tab.client_id,
            tab_id: tab.tab_id,
            window_id: tab.window_id,
            title: tab.title,
            url: tab.url,
            relevance_score: matches.len() as f64,
            matches,
        }])
    }

    async fn search_page_content_full(
        &self,
        args: SearchPageContentArgs,
        pattern: &CompiledPattern,
    ) -> Result<Vec<SearchResult>> {
        let resolved_client_id = match args.client_id.clone() {
            Some(client_id) => Some(client_id),
            None => self.resolve_client_for_tab(args.tab_id).await?,
        };

        if resolved_client_id.is_none() && self.bridge.list_clients().await.len() > 1 {
            return Err(anyhow!(
                "Multiple extension clients connected. Pass 'client_id' explicitly."
            ));
        }

        let html_response = self
            .bridge
            .request(
                resolved_client_id.as_deref(),
                "getPageHtml",
                json!({
                    "tabId": args.tab_id,
                    "selector": args.selector.as_deref().unwrap_or("body"),
                    "maxLength": MAX_HTML_LENGTH,
                    "stripScripts": true,
                    "stripStyles": true,
                }),
            )
            .await?;

        let html = html_response
            .payload
            .get("html")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("No HTML content returned"))?;

        let cached = self
            .get_cached_tab(Some(&html_response.client_id), args.tab_id)
            .await;
        let document = Html::parse_document(html);
        let text_content = Self::collect_text_content(&document);
        let matches = Self::find_matches(
            &text_content,
            pattern,
            "content",
            args.context_chars,
            args.max_matches,
        );

        if matches.is_empty() {
            return Ok(Vec::new());
        }

        let title = cached
            .as_ref()
            .map(|tab| tab.title.clone())
            .or_else(|| {
                html_response
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let url = cached
            .as_ref()
            .map(|tab| tab.url.clone())
            .or_else(|| {
                html_response
                    .payload
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let window_id = cached.map(|tab| tab.window_id).unwrap_or(0);

        Ok(vec![SearchResult {
            client_id: html_response.client_id,
            tab_id: args.tab_id,
            window_id,
            title,
            url,
            relevance_score: matches.len() as f64,
            matches,
        }])
    }

    async fn get_cached_tab(&self, client_id: Option<&str>, tab_id: i64) -> Option<TabInfo> {
        let clients = self.bridge.list_clients().await;
        for client in clients {
            if let Some(target_client) = client_id
                && client.client_id != target_client
            {
                continue;
            }

            for tab in &client.tabs {
                let Some(info) = TabInfo::from_value(tab, &client.client_id) else {
                    continue;
                };
                if info.tab_id == tab_id {
                    return Some(info);
                }
            }
        }
        None
    }

    async fn resolve_client_for_tab(&self, tab_id: i64) -> Result<Option<String>> {
        let clients = self.bridge.list_clients().await;
        let mut owners = Vec::new();
        for client in clients {
            if client
                .tabs
                .iter()
                .any(|tab| tab.get("id").and_then(Value::as_i64) == Some(tab_id))
            {
                owners.push(client.client_id);
            }
        }

        match owners.len() {
            0 => Ok(None),
            1 => Ok(owners.into_iter().next()),
            _ => Err(anyhow!(
                "Tab id {tab_id} exists in multiple clients. Pass 'client_id' explicitly."
            )),
        }
    }

    fn build_pattern(
        query: &str,
        use_regex: bool,
        case_sensitive: bool,
    ) -> Result<CompiledPattern> {
        if query.trim().is_empty() {
            return Err(anyhow!("'query' must be a non-empty string"));
        }

        let source = if use_regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };

        let regex = RegexBuilder::new(&source)
            .case_insensitive(!case_sensitive)
            .build()?;

        Ok(CompiledPattern { regex })
    }

    fn find_matches(
        content: &str,
        pattern: &CompiledPattern,
        field: &str,
        context_chars: usize,
        max_matches: usize,
    ) -> Vec<ContentMatch> {
        if max_matches == 0 || content.is_empty() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        for matched in pattern.regex.find_iter(content) {
            if matched.start() == matched.end() {
                continue;
            }

            let (line, column) = Self::line_column(content, matched.start());
            matches.push(ContentMatch {
                field: field.to_owned(),
                context: Self::context_snippet(
                    content,
                    matched.start(),
                    matched.end(),
                    context_chars,
                ),
                line: Some(line),
                column: Some(column),
                matched_text: matched.as_str().to_owned(),
            });

            if matches.len() >= max_matches {
                break;
            }
        }
        matches
    }

    fn context_snippet(text: &str, start: usize, end: usize, context_chars: usize) -> String {
        let start = start.min(text.len());
        let end = end.min(text.len());
        if start >= end {
            return String::new();
        }

        let raw_context_start = start.saturating_sub(context_chars);
        let raw_context_end = (end + context_chars).min(text.len());
        let context_start = Self::prev_char_boundary(text, raw_context_start);
        let context_end = Self::next_char_boundary(text, raw_context_end);

        let mut context = String::new();
        if context_start > 0 {
            context.push_str("...");
        }
        context.push_str(&text[context_start..start]);
        context.push_str("**");
        context.push_str(&text[start..end]);
        context.push_str("**");
        context.push_str(&text[end..context_end]);
        if context_end < text.len() {
            context.push_str("...");
        }

        context.replace('\n', " ")
    }

    fn prev_char_boundary(text: &str, mut idx: usize) -> usize {
        idx = idx.min(text.len());
        while idx > 0 && !text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    fn next_char_boundary(text: &str, mut idx: usize) -> usize {
        idx = idx.min(text.len());
        while idx < text.len() && !text.is_char_boundary(idx) {
            idx += 1;
        }
        idx
    }

    fn line_column(text: &str, byte_offset: usize) -> (usize, usize) {
        let byte_offset = byte_offset.min(text.len());
        let prefix = &text[..byte_offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix
            .rsplit('\n')
            .next()
            .map(|line_text| line_text.chars().count() + 1)
            .unwrap_or(1);
        (line, column)
    }

    fn collect_text_content(document: &Html) -> String {
        let body_selector = Selector::parse("body").expect("body selector should be valid");
        if let Some(body) = document.select(&body_selector).next() {
            body.text().collect::<Vec<_>>().join("\n")
        } else {
            document
                .root_element()
                .text()
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    fn extract_domain(url: &str) -> String {
        Url::parse(url)
            .or_else(|_| Url::parse(&format!("https://{url}")))
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_default()
    }

    fn domain_matches(domain: &str, expected: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        let expected = expected.to_ascii_lowercase();
        domain == expected || domain.ends_with(&format!(".{expected}"))
    }

    fn extract_tables(document: &Html, max_items: usize, include_metadata: bool) -> Vec<Value> {
        if max_items == 0 {
            return Vec::new();
        }

        let table_selector = Selector::parse("table").expect("table selector should be valid");
        let row_selector = Selector::parse("tr").expect("tr selector should be valid");
        let header_selector = Selector::parse("th").expect("th selector should be valid");
        let cell_selector = Selector::parse("th, td").expect("cell selector should be valid");
        let caption_selector =
            Selector::parse("caption").expect("caption selector should be valid");

        let mut tables = Vec::new();
        for (index, table) in document.select(&table_selector).take(max_items).enumerate() {
            let mut headers = Vec::new();
            if let Some(header_row) = table.select(&row_selector).next() {
                headers = header_row
                    .select(&header_selector)
                    .map(Self::element_text)
                    .filter(|value| !value.is_empty())
                    .collect();
            }
            if headers.is_empty() {
                headers = table
                    .select(&header_selector)
                    .map(Self::element_text)
                    .filter(|value| !value.is_empty())
                    .collect();
            }

            let mut rows = Vec::new();
            for row in table.select(&row_selector) {
                let cells: Vec<String> = row
                    .select(&cell_selector)
                    .map(Self::element_text)
                    .filter(|value| !value.is_empty())
                    .collect();
                if !cells.is_empty() {
                    rows.push(cells);
                }
            }

            let mut table_data = json!({
                "index": index,
                "headers": headers,
                "rows": rows,
            });

            if include_metadata {
                let caption = table
                    .select(&caption_selector)
                    .next()
                    .map(Self::element_text);
                table_data["metadata"] = json!({
                    "caption": caption,
                    "row_count": table_data["rows"].as_array().map(|rows| rows.len()).unwrap_or(0),
                    "column_count": table_data["headers"].as_array().map(|headers| headers.len()).unwrap_or(0),
                });
            }

            tables.push(table_data);
        }
        tables
    }

    fn extract_lists(document: &Html, max_items: usize, include_metadata: bool) -> Vec<Value> {
        if max_items == 0 {
            return Vec::new();
        }

        let list_selector = Selector::parse("ul, ol").expect("list selector should be valid");
        let item_selector = Selector::parse("li").expect("li selector should be valid");

        let mut lists = Vec::new();
        for (index, list) in document.select(&list_selector).take(max_items).enumerate() {
            let tag = list.value().name();
            let list_type = if tag == "ol" { "ordered" } else { "unordered" };
            let items: Vec<String> = list
                .select(&item_selector)
                .map(Self::element_text)
                .filter(|value| !value.is_empty())
                .collect();

            let mut list_data = json!({
                "index": index,
                "type": list_type,
                "items": items,
            });

            if include_metadata {
                list_data["metadata"] = json!({
                    "tag": tag,
                    "item_count": list_data["items"].as_array().map(|values| values.len()).unwrap_or(0),
                });
            }

            lists.push(list_data);
        }
        lists
    }

    fn extract_headings(document: &Html, max_items: usize) -> Vec<Value> {
        if max_items == 0 {
            return Vec::new();
        }

        let heading_selector =
            Selector::parse("h1, h2, h3, h4, h5, h6").expect("heading selector should be valid");

        document
            .select(&heading_selector)
            .take(max_items)
            .enumerate()
            .map(|(index, heading)| {
                let tag_name = heading.value().name();
                let level = tag_name
                    .strip_prefix('h')
                    .and_then(|value| value.parse::<u8>().ok())
                    .unwrap_or(0);
                json!({
                    "index": index,
                    "level": level,
                    "text": Self::element_text(heading),
                    "id": heading.value().attr("id"),
                })
            })
            .collect()
    }

    fn extract_title(document: &Html) -> Option<String> {
        let title_selector = Selector::parse("title").ok()?;
        document
            .select(&title_selector)
            .next()
            .map(Self::element_text)
    }

    fn element_text(element: ElementRef<'_>) -> String {
        element
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

struct CompiledPattern {
    regex: Regex,
}

#[derive(Clone, Copy)]
enum SearchField {
    Title,
    Url,
}

impl SearchField {
    fn parse_many(fields: &[String]) -> Result<Vec<Self>> {
        if fields.is_empty() {
            return Err(anyhow!(
                "'fields' cannot be empty. Supported values: title, url"
            ));
        }

        let mut parsed = Vec::new();
        for field in fields {
            let normalized = field.trim().to_ascii_lowercase();
            let search_field = match normalized.as_str() {
                "title" => SearchField::Title,
                "url" => SearchField::Url,
                "content" => {
                    return Err(anyhow!(
                        "'content' is not supported in cached tab search. Use search_page_content with scope='full'."
                    ));
                }
                _ => {
                    return Err(anyhow!(
                        "Unsupported field '{}'. Supported values: title, url",
                        field
                    ));
                }
            };
            parsed.push(search_field);
        }
        Ok(parsed)
    }

    fn value_name_weight<'a>(&self, tab: &'a TabInfo) -> (&'a str, &'static str, f64) {
        match self {
            SearchField::Title => (&tab.title, "title", 2.0),
            SearchField::Url => (&tab.url, "url", 1.0),
        }
    }
}

struct TabInfo {
    client_id: String,
    tab_id: i64,
    window_id: i64,
    title: String,
    url: String,
    domain: String,
    active: bool,
    pinned: bool,
    incognito: bool,
}

impl TabInfo {
    fn from_value(tab: &Value, client_id: &str) -> Option<Self> {
        let tab_id = tab.get("id").and_then(Value::as_i64)?;
        let window_id = tab.get("windowId").and_then(Value::as_i64).unwrap_or(0);
        let title = tab
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let url = tab
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let domain = SearchService::extract_domain(&url);
        let active = tab.get("active").and_then(Value::as_bool).unwrap_or(false);
        let pinned = tab.get("pinned").and_then(Value::as_bool).unwrap_or(false);
        let incognito = tab
            .get("incognito")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Some(Self {
            client_id: client_id.to_owned(),
            tab_id,
            window_id,
            title,
            url,
            domain,
            active,
            pinned,
            incognito,
        })
    }
}

#[cfg(test)]
mod tests {
    use scraper::Html;

    use super::SearchService;

    #[test]
    fn pattern_plain_case_sensitive_and_insensitive() {
        let insensitive = SearchService::build_pattern("login", false, false)
            .expect("case-insensitive plain pattern should compile");
        let insensitive_matches =
            SearchService::find_matches("Login LOGIN", &insensitive, "title", 10, 10);
        assert_eq!(insensitive_matches.len(), 2);

        let sensitive = SearchService::build_pattern("login", false, true)
            .expect("case-sensitive plain pattern should compile");
        let sensitive_matches =
            SearchService::find_matches("Login LOGIN login", &sensitive, "title", 10, 10);
        assert_eq!(sensitive_matches.len(), 1);
        assert_eq!(sensitive_matches[0].matched_text, "login");
    }

    #[test]
    fn pattern_regex_reports_invalid_expression() {
        let result = SearchService::build_pattern("([a-z", true, false);
        assert!(result.is_err(), "invalid regex should produce an error");
    }

    #[test]
    fn domain_matching_supports_exact_and_subdomain() {
        assert!(SearchService::domain_matches("github.com", "github.com"));
        assert!(SearchService::domain_matches(
            "api.github.com",
            "github.com"
        ));
        assert!(!SearchService::domain_matches(
            "evilgithub.com",
            "github.com"
        ));
    }

    #[test]
    fn context_snippet_handles_utf8_boundaries() {
        let text = "前缀-你好世界-后缀";
        let start = text.find("你好").expect("target should exist");
        let end = start + "你好".len();

        let context = SearchService::context_snippet(text, start, end, 3);
        assert!(
            context.contains("**你好**"),
            "snippet should highlight target, got: {context}"
        );
    }

    #[test]
    fn structured_extractors_parse_tables_lists_and_headings() {
        let html = r#"
            <html>
              <head><title>Demo</title></head>
              <body>
                <h1 id="main">Main title</h1>
                <h2>Section A</h2>
                <table>
                  <caption>Users</caption>
                  <tr><th>Name</th><th>Role</th></tr>
                  <tr><td>Alice</td><td>Admin</td></tr>
                </table>
                <ul><li>One</li><li>Two</li></ul>
                <ol><li>First</li></ol>
              </body>
            </html>
        "#;
        let document = Html::parse_document(html);

        let tables = SearchService::extract_tables(&document, 10, true);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0]["headers"][0], "Name");
        assert_eq!(tables[0]["rows"][1][1], "Admin");
        assert_eq!(tables[0]["metadata"]["caption"], "Users");

        let lists = SearchService::extract_lists(&document, 10, true);
        assert_eq!(lists.len(), 2);
        assert_eq!(lists[0]["type"], "unordered");
        assert_eq!(lists[1]["type"], "ordered");

        let headings = SearchService::extract_headings(&document, 10);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0]["level"], 1);
        assert_eq!(headings[0]["id"], "main");
    }
}
