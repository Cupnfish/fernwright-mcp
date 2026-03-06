use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchTabsArgs {
    #[serde(default)]
    pub client_id: Option<String>,
    /// Search query (supports regex patterns when use_regex=true).
    pub query: String,
    /// Fields supported in cached tab search: title, url.
    #[serde(default = "default_search_fields")]
    pub fields: Vec<String>,
    /// Use regex pattern matching.
    #[serde(default)]
    pub use_regex: bool,
    /// Case sensitive search.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Maximum number of matched tabs to return.
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchPageContentArgs {
    #[serde(default)]
    pub client_id: Option<String>,
    pub tab_id: i64,
    /// Search query (supports regex when use_regex=true).
    pub query: String,
    /// "cached" searches cached tab metadata; "full" fetches HTML via getPageHtml.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// CSS selector used when scope=full.
    #[serde(default)]
    pub selector: Option<String>,
    /// Use regex pattern matching.
    #[serde(default)]
    pub use_regex: bool,
    /// Case sensitive search.
    #[serde(default)]
    pub case_sensitive: bool,
    /// Characters to include before/after the match in context snippets.
    #[serde(default = "default_context_chars")]
    pub context_chars: usize,
    /// Maximum number of matches to return.
    #[serde(default = "default_max_matches")]
    pub max_matches: usize,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FilterTabsArgs {
    #[serde(default)]
    pub client_id: Option<String>,
    /// Optional URL pattern filter.
    #[serde(default)]
    pub url_pattern: Option<String>,
    /// Optional title pattern filter.
    #[serde(default)]
    pub title_pattern: Option<String>,
    /// Domain filter (matches exact domain and subdomains).
    #[serde(default)]
    pub domain: Option<String>,
    /// Keep only active tabs.
    #[serde(default)]
    pub active_only: bool,
    /// Keep only pinned tabs.
    #[serde(default)]
    pub pinned_only: bool,
    /// Keep only incognito tabs.
    #[serde(default)]
    pub incognito_only: bool,
    /// Use regex for url_pattern/title_pattern.
    #[serde(default)]
    pub use_regex: bool,
    /// Case-sensitive matching for url_pattern/title_pattern.
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtractStructuredDataArgs {
    #[serde(default)]
    pub client_id: Option<String>,
    pub tab_id: i64,
    /// tables | lists | headings | all
    #[serde(default = "default_extract_type")]
    pub extract_type: String,
    /// CSS selector used as extraction root.
    #[serde(default)]
    pub selector: Option<String>,
    /// Maximum number of extracted items.
    #[serde(default = "default_max_items")]
    pub max_items: usize,
    /// Include metadata in each extracted group.
    #[serde(default = "default_true")]
    pub include_metadata: bool,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindInPageArgs {
    #[serde(default)]
    pub client_id: Option<String>,
    pub query: String,
    /// Also fetch tab HTML and search content for cached matches.
    #[serde(default)]
    pub deep_search: bool,
    /// Fields supported in the initial cached search: title, url.
    #[serde(default = "default_search_fields")]
    pub fields: Vec<String>,
    #[serde(default)]
    pub use_regex: bool,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default = "default_context_chars")]
    pub context_chars: usize,
    #[serde(default = "default_max_content_matches")]
    pub max_content_matches: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub client_id: String,
    pub tab_id: i64,
    pub window_id: i64,
    pub title: String,
    pub url: String,
    pub matches: Vec<ContentMatch>,
    pub relevance_score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentMatch {
    pub field: String,
    pub context: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub matched_text: String,
}

#[derive(Debug, Serialize)]
pub struct FilteredTabsResult {
    pub client_id: String,
    pub total_tabs: usize,
    pub filtered_tabs: Vec<FilteredTab>,
    pub applied_filters: Value,
}

#[derive(Debug, Serialize)]
pub struct FilteredTab {
    pub client_id: String,
    pub tab_id: i64,
    pub window_id: i64,
    pub title: String,
    pub url: String,
    pub domain: String,
    pub active: bool,
    pub pinned: bool,
    pub incognito: bool,
    pub match_reason: Vec<String>,
}

fn default_search_fields() -> Vec<String> {
    vec!["title".to_owned(), "url".to_owned()]
}

fn default_max_results() -> usize {
    50
}

fn default_scope() -> String {
    "cached".to_owned()
}

fn default_context_chars() -> usize {
    100
}

fn default_max_matches() -> usize {
    100
}

fn default_extract_type() -> String {
    "all".to_owned()
}

fn default_max_items() -> usize {
    50
}

fn default_max_content_matches() -> usize {
    50
}

fn default_true() -> bool {
    true
}
