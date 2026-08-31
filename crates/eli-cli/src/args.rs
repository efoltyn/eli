#[derive(Parser, Debug)]
#[command(name = "market-search", version, about = "Market Search: MCP server for finance data + a terminal coding agent")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Command>,

    /// Provider: openrouter | openai | anthropic | ollama | mock
    #[arg(long, global = true)]
    provider: Option<String>,

    /// Model name (provider-specific)
    #[arg(long, global = true)]
    model: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Interactive setup - configure provider, model, and API key
    Setup,

    /// Create a default config file (if missing)
    Init,

    /// Print or set config values
    Config {
        /// Set a config value: provider, model, mem_steps, key, sec_user_agent, compact, compact_trigger, compact_keep, summary_model, parallel_commands, parallel_subagents, scrollback_max_lines
        #[arg(long)]
        set: Option<String>,

        /// Value to set
        #[arg(long)]
        value: Option<String>,
    },

    /// Emit JSON schema for a CLI subcommand (hidden)
    #[command(hide = true)]
    ToolInfo {
        /// Subcommand path (e.g., finance timeseries)
        #[arg(value_name = "PATH", num_args = 0..)]
        path: Vec<String>,
    },

    /// Financial data tools (for raw time-series exploration)
    Finance {
        #[command(subcommand)]
        cmd: FinanceCommand,
    },

    /// Web tools (crawl, search, read)
    Web {
        #[command(subcommand)]
        cmd: WebCommand,
    },

    /// Legal research tools (case law, dockets, citations, regulations, statutes)
    Legal {
        #[command(subcommand)]
        cmd: LegalCommand,
    },

    /// Start MCP (Model Context Protocol) server — exposes market-search tools as native Claude Code tools via JSON-RPC stdio.
    Mcp(McpArgs),

    /// Log research picks for a report to track performance over time.
    Picks {
        #[command(subcommand)]
        cmd: PicksCommand,
    },
}

#[derive(Subcommand, Debug)]
enum PicksCommand {
    /// Record ticker/market picks at current prices for a given report.
    Log(PicksLogArgs),
}

#[derive(clap::Args, Debug)]
struct PicksLogArgs {
    /// Path to the HTML report file (supports ~/).
    #[arg(long)]
    report: String,

    /// Equity ticker(s) to track at current price (comma-separated or repeatable).
    #[arg(long, value_delimiter = ',')]
    ticker: Vec<String>,

    /// Prediction market slug(s) to track at current probability (comma-separated or repeatable).
    #[arg(long, value_delimiter = ',')]
    market: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct McpArgs {
    #[command(subcommand)]
    cmd: Option<McpSubcommand>,

    /// Run as HTTP server instead of stdio (MCP Streamable HTTP transport).
    #[arg(long, default_value_t = false)]
    http: bool,

    /// Port for HTTP mode.
    #[arg(long, default_value = "8484")]
    port: u16,

    /// Which tool families to advertise: all (default) | finance | legal.
    /// The `legal-search` binary defaults to `legal`.
    #[arg(long)]
    profile: Option<String>,
}

#[derive(Subcommand, Debug)]
enum McpSubcommand {
    /// Get a public HTTPS /mcp URL (boots local HTTP MCP + spawns a tunnel).
    Share(ShareArgs),
}

#[derive(clap::Args, Debug)]
struct ShareArgs {
    /// Tunnel provider: cloudflare | ngrok | tunnelmole | self-host
    #[arg(long, default_value = "cloudflare")]
    provider: String,

    /// Local port the MCP HTTP server is bound to.
    #[arg(long, default_value = "8484")]
    port: u16,

    /// For ngrok: reserved subdomain (e.g. mysub.ngrok-free.dev or just mysub).
    #[arg(long)]
    domain: Option<String>,

    /// For ngrok: authtoken if not already configured globally.
    #[arg(long)]
    authtoken: Option<String>,
}

#[derive(Subcommand, Debug)]
enum FinanceCommand {
    /// Fetch OHLCV time-series for one or more tickers.
    Timeseries(FinanceTimeseriesArgs),
    /// Largest day movers by percent move, market cap, dollar volume, or estimated value change.
    Movers(FinanceMoversArgs),
    /// Current snapshot of income/balance/cashflow + 32 trailing ratios + company profile (sector, industry, employees). Single ticker returns object; multi-ticker returns array.
    Fundamentals(FinanceFundamentalsArgs),
    /// Search for ticker symbols or macro series IDs.
    Search(FinanceSearchArgs),
    /// Fetch recent SEC filings (8-K, 10-K, 10-Q) for a ticker.
    Filings(FinanceFilingsArgs),
    /// Alias for filings.
    Sec(FinanceFilingsArgs),
    /// Fetch earnings and macro release schedules (no-auth public endpoints).
    Schedule(FinanceScheduleArgs),
    /// Aggregate implied Fed policy trajectory from local prediction-market cache.
    RatePath(FinanceRatePathArgs),
    /// Prediction market discovery + pricing (Kalshi default; falls back to Polymarket).
    Odds(FinanceOddsArgs),
    /// Listed options chains with IV/skew summaries (Yahoo Finance).
    Options(FinanceOptionsArgs),
    /// Deprecated alias for `finance odds sync` (canonical). Same flags, same backend; kept for cron compatibility.
    Sync(FinanceSyncArgs),
    /// Local paper trading sandbox using live Kalshi/Polymarket prices.
    Paper(FinancePaperArgs),
    /// Interactive Brokers via local TWS / IB Gateway.
    Ibkr(FinanceIbkrArgs),
    /// Recent US Treasury auction results (bid-to-cover, tails, bidder breakdown).
    Auctions(FinanceAuctionsArgs),
    /// CFTC Commitment of Traders positioning (spec vs commercial, weekly).
    Cot(FinanceCotArgs),
    /// Futures term structure (forward curve) for commodities.
    Curve(FinanceCurveArgs),
    /// NY Fed Markets: overnight rates (SOFR/EFFR), reverse repo, SOMA holdings, dealer positions.
    Nyfed(FinanceNyfedArgs),
    /// CBOE volatility indices / term structure: VIX, VVIX, OVX, GVZ, SKEW.
    #[command(name = "volatility", visible_alias = "volsurface")]
    Volsurface(FinanceVolsurfaceArgs),
    /// OFR Financial Stress Index: composite + credit/equity/funding/vol decomposition.
    Stress(FinanceStressArgs),
    /// Treasury fiscal data: national debt, daily statement, average interest rates.
    Fiscal(FinanceFiscalArgs),
    /// ECB Statistical Data Warehouse: EUR/USD, Euro STR, M3, EURIBOR, yield curve, balance sheet.
    Ecb(FinanceEcbArgs),
    /// EIA: US petroleum inventories (crude, gasoline, distillate), natural gas storage.
    Eia(FinanceEiaArgs),
    /// BIS: global central bank policy rates, total assets, credit-to-GDP gaps, property prices.
    Bis(FinanceBisArgs),
    /// BOJ: Bank of Japan monetary base, balance sheet, TANKAN, call rate, money stock.
    Boj(FinanceBojArgs),
    /// BOE: Bank of England Bank Rate, SONIA, gilt yields, M4, GBP FX rates.
    Boe(FinanceBoeArgs),
}

#[derive(Subcommand, Debug)]
enum WebCommand {
    /// Crawl a website and extract content from all discovered pages.
    Crawl(WebCrawlArgs),
    /// Ingestion-focused web search for URL candidates + diagnostics.
    Search(WebSearchArgs),
    /// Read and extract content from one or many URLs.
    Read(WebReadArgs),
    /// Extract key facts from content (URL, file, or text).
    Extract(WebExtractArgs),
}

#[derive(Subcommand, Debug)]
enum AgentCommand {
    /// Generate a market intelligence report (JSON + HTML) using Eli tools and optional model synthesis.
    Report(AgentReportArgs),
    /// Run a single Eli worker from a natural-language task.
    Run(AgentRunArgs),
    /// Run many Eli workers in parallel from a task template and vars file.
    Fanout(AgentFanoutArgs),
    /// Chunk a large input and orchestrate map/reduce/critic swarm synthesis.
    Swarm(AgentSwarmArgs),
    /// Critique a lead thesis/report using worker fanout.
    Critique(AgentModeArgs),
    /// Find additional evidence for/against a thesis via worker fanout.
    Evidence(AgentModeArgs),
    /// Run competitive workers to find the best answer.
    Compete(AgentModeArgs),
    /// Run worker debate and synthesize consensus.
    Debate(AgentModeArgs),
}

#[derive(Subcommand, Debug)]
enum SentinelCommand {
    /// Start the sentinel daemon in the background.
    Start(SentinelStartArgs),
    /// Stop the sentinel daemon.
    Stop(SentinelStopArgs),
    /// Print sentinel daemon status.
    Status(SentinelStatusArgs),
    /// Register a new sentinel trigger subscription.
    Subscribe(SentinelSubscribeArgs),
    /// Remove a sentinel subscription by id or name.
    Unsubscribe(SentinelUnsubscribeArgs),
    /// List configured sentinel subscriptions.
    List(SentinelListArgs),
    /// Emit a synthetic alert packet for wiring tests.
    Test(SentinelTestArgs),
    /// Replay recent packets from queue file.
    Replay(SentinelReplayArgs),
    /// Internal daemon process entrypoint.
    #[command(hide = true)]
    DaemonRun(SentinelDaemonRunArgs),
}

#[derive(Debug, Serialize)]
struct RustFileSummary {
    items_total: usize,
    functions: usize,
    function_names: Vec<String>,
    /// Full signatures: "pub async fn name(param: Type) -> ReturnType"
    /// Lets an AI caller understand the API contract without reading source.
    function_signatures: Vec<String>,
    structs: usize,
    struct_names: Vec<String>,
    /// Per-struct field list: {struct_name: ["field: Type", ...]}
    struct_fields: std::collections::BTreeMap<String, Vec<String>>,
    enums: usize,
    enum_names: Vec<String>,
    impls: usize,
    impl_targets: Vec<String>,
    /// Methods per impl block: {"LlmAdapter for AnthropicAdapter": ["pub async fn chat(...)  -> ..."]}
    impl_methods: std::collections::BTreeMap<String, Vec<String>>,
    traits: usize,
    trait_names: Vec<String>,
    modules: usize,
    module_names: Vec<String>,
    uses: usize,
    use_paths: Vec<String>,
    consts: usize,
    const_names: Vec<String>,
    statics: usize,
    type_aliases: usize,
    type_alias_names: Vec<String>,
    macros: usize,
    others: usize,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum CrawlViewMode {
    Summary,
    Raw,
    Path,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum CrawlSaveMode {
    Auto,
    Off,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum WebSearchModeArg {
    Auto,
    News,
    Finance,
    Research,
    Tech,
    Encyclopedia,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum WebSearchRecencyArg {
    Day,
    Week,
    Month,
    Year,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum FinancePaperCommandArg {
    Trade,
    Positions,
    Trades,
    Mark,
    Reset,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum FinancePaperModeArg {
    Simulated,
    #[value(alias = "live_like")]
    LiveLike,
    #[value(alias = "kalshi_demo")]
    KalshiDemo,
    #[value(alias = "polymarket_demo")]
    PolymarketDemo,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum FinancePaperSideArg {
    Yes,
    No,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum FinancePaperOrderActionArg {
    Buy,
    Sell,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum FinanceIbkrCommandArg {
    Snapshot,
    Timeseries,
    AccountSummary,
    Positions,
    Portfolio,
    OpenOrders,
    PlaceOrder,
    CancelOrder,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum SentinelSeverityArg {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Copy, Clone, Debug, ValueEnum, Eq, PartialEq)]
enum SentinelSpawnTargetArg {
    Default,
    Codex,
    Claude,
    Gemini,
    Both,
}

#[derive(clap::Args, Debug)]
struct SentinelPathArgs {
    /// Sentinel root directory.
    #[arg(long = "sentinel-dir")]
    sentinel_dir: Option<PathBuf>,

    /// Queue JSONL file override.
    #[arg(long = "queue-file")]
    queue_file: Option<PathBuf>,

    /// Intelligence packets JSONL file override.
    #[arg(long = "packets-file")]
    packets_file: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct SentinelStartArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Daemon evaluation interval in seconds.
    #[arg(long = "interval-secs", default_value_t = 15)]
    interval_secs: u64,
}

#[derive(clap::Args, Debug)]
struct SentinelStopArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,
}

#[derive(clap::Args, Debug)]
struct SentinelStatusArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,
}

#[derive(clap::Args, Debug)]
struct SentinelSubscribeArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Human-readable subscription name (internal slug).
    #[arg(long)]
    name: String,

    /// Human-readable prediction statement shown in the UI.
    /// Example: "Gold will reach 5200 before Friday close"
    #[arg(long)]
    title: Option<String>,

    /// Title of the report that authored this prediction.
    #[arg(long = "source-report")]
    source_report_title: Option<String>,

    /// Date of the source report (ISO format, e.g. 2026-03-06).
    #[arg(long = "source-date")]
    source_report_date: Option<String>,

    /// Filename of the source report (relative to reports_dir) for opening in the UI.
    #[arg(long = "source-file")]
    source_report_file: Option<String>,

    /// Key evidence snippet or quote from the source report justifying this prediction.
    #[arg(long = "source-evidence")]
    source_evidence: Option<String>,

    /// Trigger expression, e.g. \"pyth_wti > 80 && poly_hormuz_yes > 0.50\".
    /// Optional when --fire-at is set (defaults to "true" for pure checkpoint daemons).
    #[arg(long)]
    expr: Option<String>,

    /// Optional variable mapping (repeatable): var=provider:query
    #[arg(long = "var")]
    vars: Vec<String>,

    /// Why this alert matters (stored in packet/playbook).
    #[arg(long = "why")]
    why: Option<String>,

    /// Prompt template for the follow-up playbook.
    #[arg(long = "prompt-template")]
    prompt_template: Option<String>,

    /// Alert severity.
    #[arg(long, value_enum, default_value = "medium")]
    severity: SentinelSeverityArg,

    /// Cooldown between repeated triggers (seconds).
    #[arg(long = "cooldown-secs", default_value_t = 300)]
    cooldown_secs: u64,

    /// Start enabled (default true).
    #[arg(long, default_value_t = true)]
    enabled: bool,

    /// Spawn headless AI agent (claude/codex) when this subscription triggers.
    #[arg(long = "spawn-agent", default_value_t = false)]
    spawn_agent: bool,

    /// Which headless writer(s) should fire when this subscription triggers.
    #[arg(long = "spawn-target", value_enum, default_value = "default")]
    spawn_target: SentinelSpawnTargetArg,

    /// Legacy spawn cooldown field retained for compatibility with existing subscriptions.
    /// Spawn routing now uses rolling per-hour budgets instead.
    #[arg(long = "spawn-cooldown-secs", default_value_t = 14400, hide = true)]
    spawn_cooldown_secs: u64,

    /// Human-readable prediction thesis this daemon encodes.
    /// When set, fires on HIT (condition met) OR MISS (deadline elapsed without condition met).
    #[arg(long = "prediction")]
    prediction: Option<String>,

    /// Which variable name in --expr to track as the prediction target (e.g., "pyth_wti").
    #[arg(long = "target-var")]
    target_var: Option<String>,

    /// Predicted numeric target for target-var (e.g., 90.0).
    #[arg(long = "target-value")]
    target_value: Option<f64>,

    /// Prediction deadline in RFC3339 format (e.g., 2026-04-01T00:00:00Z).
    /// Fires MISS if condition not met by this time.
    #[arg(long = "deadline")]
    deadline: Option<String>,

    /// Scheduled fire time in RFC3339 format (e.g., 2026-03-12T16:03:00Z).
    /// Daemon fires ONCE at this exact time. Expr is evaluated at that moment for HIT/MISS.
    /// Omit --expr for a pure checkpoint (always fires, no condition).
    #[arg(long = "fire-at")]
    fire_at: Option<String>,
}

#[derive(clap::Args, Debug)]
struct SentinelUnsubscribeArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Subscription id or exact name.
    #[arg(long = "id")]
    id_or_name: String,
}

#[derive(clap::Args, Debug)]
struct SentinelListArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,
}

#[derive(clap::Args, Debug)]
struct SentinelTestArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Synthetic test scenario.
    #[arg(long, default_value = "generic")]
    scenario: String,
}

#[derive(clap::Args, Debug)]
struct SentinelReplayArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Number of recent queue lines to replay.
    #[arg(long = "max-lines", default_value_t = 50)]
    max_lines: usize,
}

#[derive(clap::Args, Debug)]
struct SentinelDaemonRunArgs {
    #[command(flatten)]
    paths: SentinelPathArgs,

    /// Daemon evaluation interval in seconds.
    #[arg(long = "interval-secs", default_value_t = 15)]
    interval_secs: u64,
}

#[derive(clap::Args, Debug)]
struct AgentReportArgs {
    /// Report objective/prompt. Defaults to Eli three-pillar research framework.
    #[arg(long)]
    prompt: Option<String>,

    /// Comma-separated market tickers for snapshot/timeseries.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "SPY,QQQ,IWM,DIA,^VIX,BTC-USD,ETH-USD,SOL-USD,DX-Y.NYB,GC=F,CL=F"
    )]
    tickers: Vec<String>,

    /// Historical lookback span for timeseries (e.g. 14d, 30d, 3mo).
    #[arg(long, default_value = "14d")]
    lookback: String,

    /// Timeseries granularity (e.g. 15min, 1h, 1d).
    #[arg(long, default_value = "1h")]
    granularity: String,

    /// Lock clock-sensitive tools to N minutes before report start.
    #[arg(long = "lock-minutes", conflicts_with = "as_of")]
    lock_minutes: Option<u64>,

    /// Explicit report anchor time (RFC3339 or YYYY-MM-DD).
    #[arg(long = "as-of", conflicts_with = "lock_minutes")]
    as_of: Option<String>,

    /// Comma-separated odds search queries.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "recession,fed,inflation,iran,oil,china,taiwan"
    )]
    odds_queries: Vec<String>,

    /// Number of top markets per odds query.
    #[arg(long, default_value_t = 8)]
    top: usize,

    /// Comma-separated web queries for narrative context.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "stock market today,treasury yields today,fed policy outlook,oil geopolitical risk"
    )]
    web_queries: Vec<String>,

    /// Max runtime budget per command invocation (milliseconds).
    #[arg(long = "max-ms", default_value_t = 45000)]
    max_ms: u64,

    /// Comma-separated fallback models for synthesis worker.
    #[arg(long = "fallback-models", value_delimiter = ',')]
    fallback_models: Vec<String>,

    /// Optional explicit HTML output path.
    #[arg(long = "html-out")]
    html_out: Option<PathBuf>,

    /// Output JSON path for the report envelope.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct AgentRunArgs {
    /// Natural-language task for the worker.
    #[arg(long)]
    task: String,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Comma-separated fallback models (used on worker failure).
    #[arg(long = "fallback-models", value_delimiter = ',')]
    fallback_models: Vec<String>,

    /// Max runtime budget per worker (milliseconds).
    #[arg(long = "max-ms", default_value_t = 45000)]
    max_ms: u64,

    /// Max total attempts per worker across primary + fallbacks.
    #[arg(long = "max-attempts", default_value_t = 4)]
    max_attempts: usize,

    /// Require the final report to cite these path prefixes (comma-separated).
    #[arg(long = "must-cite", value_delimiter = ',')]
    must_cite: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct AgentFanoutArgs {
    /// Task template. Use placeholders like {{ticker}} or {{stance}}.
    #[arg(long = "task-template")]
    task_template: String,

    /// JSON file containing an array of objects for template vars.
    #[arg(long)]
    vars: PathBuf,

    /// Optional shared artifact manifest path all workers should read first.
    #[arg(long = "shared-manifest")]
    shared_manifest: Option<PathBuf>,

    /// Max workers to run at once.
    #[arg(long, default_value = "4")]
    max_parallel: usize,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Comma-separated fallback models (used on worker failure).
    #[arg(long = "fallback-models", value_delimiter = ',')]
    fallback_models: Vec<String>,

    /// Max runtime budget per worker (milliseconds).
    #[arg(long = "max-ms", default_value_t = 45000)]
    max_ms: u64,

    /// Max total attempts per worker across primary + fallbacks.
    #[arg(long = "max-attempts", default_value_t = 4)]
    max_attempts: usize,

    /// Require each successful worker report to cite these path prefixes (comma-separated).
    #[arg(long = "must-cite", value_delimiter = ',')]
    must_cite: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct AgentSwarmArgs {
    /// High-level goal for the swarm.
    #[arg(long)]
    task: String,

    /// Input file to process (txt/md/json/csv/ndjson/pdf).
    #[arg(long)]
    input: PathBuf,

    /// Optional explicit number of chunk workers (X swarms).
    #[arg(long)]
    chunks: Option<usize>,

    /// Approximate characters per chunk when --chunks is not provided.
    #[arg(long = "chunk-chars", default_value_t = 20_000)]
    chunk_chars: usize,

    /// Character overlap between chunks to reduce boundary loss.
    #[arg(long = "overlap-chars", default_value_t = 500)]
    overlap_chars: usize,

    /// Hard cap on produced chunks.
    #[arg(long = "max-chunks", default_value_t = 64)]
    max_chunks: usize,

    /// Max workers to run at once for map stage.
    #[arg(long, default_value = "4")]
    max_parallel: usize,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Comma-separated fallback models (used on worker failure).
    #[arg(long = "fallback-models", value_delimiter = ',')]
    fallback_models: Vec<String>,

    /// Max runtime budget per worker (milliseconds).
    #[arg(long = "max-ms", default_value_t = 120_000)]
    max_ms: u64,

    /// Max total attempts per worker across primary + fallbacks.
    #[arg(long = "max-attempts", default_value_t = 4)]
    max_attempts: usize,

    /// Require successful stage reports to cite these path prefixes (comma-separated).
    #[arg(long = "must-cite", value_delimiter = ',')]
    must_cite: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct AgentModeArgs {
    /// User objective for this report mode.
    #[arg(long)]
    prompt: String,

    /// Optional lead report or thesis file path.
    #[arg(long)]
    lead: Option<PathBuf>,

    /// JSON file containing an array of worker objects (name/model/role/etc).
    #[arg(long)]
    vars: PathBuf,

    /// Optional shared artifact manifest path all workers should read first.
    #[arg(long = "shared-manifest")]
    shared_manifest: Option<PathBuf>,

    /// Allow workers to reference peer output in compete/debate modes.
    #[arg(long, default_value_t = false)]
    allow_cheat: bool,

    /// Max workers to run at once.
    #[arg(long, default_value = "4")]
    max_parallel: usize,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Comma-separated fallback models (used on worker failure).
    #[arg(long = "fallback-models", value_delimiter = ',')]
    fallback_models: Vec<String>,

    /// Max runtime budget per worker (milliseconds).
    #[arg(long = "max-ms", default_value_t = 120_000)]
    max_ms: u64,

    /// Max total attempts per worker across primary + fallbacks.
    #[arg(long = "max-attempts", default_value_t = 2)]
    max_attempts: usize,

    /// Require each successful worker report to cite these path prefixes (comma-separated).
    #[arg(long = "must-cite", value_delimiter = ',')]
    must_cite: Vec<String>,
}

#[derive(clap::Args, Debug)]
struct CodeArgs {
    /// Path to Rust source file or directory to analyze.
    path: PathBuf,

    /// Also generate code (e.g., getter methods for structs).
    #[arg(long, default_value_t = false)]
    generate: bool,

    /// Minimum line count filter (directory mode only).
    #[arg(long, default_value_t = 0)]
    min_loc: usize,

    /// Maximum number of files to analyze after sorting by path (directory mode only).
    #[arg(long)]
    max_files: Option<usize>,

    /// Parallel worker count for Rust parsing (directory mode only).
    #[arg(long)]
    workers: Option<usize>,

    /// Number of rows to include for each hotspot ranking (directory mode only).
    #[arg(long, default_value_t = 20)]
    top: usize,

    /// Include per-file metrics in response (directory mode only).
    #[arg(long, default_value_t = false)]
    include_files: bool,

    /// Search for symbol usages across all .rs files in path (comma-separated).
    /// Uses multi-pattern matching. Returns every line containing any of the symbols
    /// with file path and line number. Works on files and directories.
    #[arg(long, value_delimiter = ',')]
    find: Vec<String>,

    /// Emit the complete public API surface for a directory: every pub fn (with full
    /// signature), pub struct (with field types), pub enum (with variants), pub trait
    /// (with method signatures), grouped by file. Ideal for understanding a module's
    /// contract before writing new code.
    #[arg(long, default_value_t = false)]
    pub_api: bool,

    /// Optional output file for JSON response.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct WebCrawlArgs {
    /// URL to start crawling from.
    #[arg(long)]
    url: String,

    /// Maximum number of pages to crawl (default: 50).
    #[arg(long, default_value = "50")]
    max_pages: usize,

    /// Respect robots.txt (default: true).
    #[arg(long, default_value = "true")]
    respect_robots: bool,

    /// Include subdomains in crawl (default: false).
    #[arg(long, default_value = "false")]
    subdomains: bool,

    /// Crawl via sitemap discovery mode.
    #[arg(long, default_value = "false")]
    sitemap: bool,

    /// Smart crawl mode: HTTP first, render JS only when needed.
    #[arg(long, default_value = "false", conflicts_with = "sitemap")]
    smart: bool,

    /// Terminal output view.
    #[arg(long, value_enum, default_value_t = CrawlViewMode::Summary)]
    view: CrawlViewMode,

    /// Save policy when --out is not provided.
    #[arg(long, value_enum, default_value_t = CrawlSaveMode::Auto)]
    save: CrawlSaveMode,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct WebSearchArgs {
    /// Search query.
    #[arg(long)]
    query: String,

    /// Search mode tuned for different ingestion workflows.
    #[arg(long, value_enum, default_value_t = WebSearchModeArg::Auto)]
    mode: WebSearchModeArg,

    /// Include only these domains (comma-separated).
    #[arg(long, value_delimiter = ',')]
    domains: Vec<String>,

    /// Exclude these domains (comma-separated).
    #[arg(long = "exclude-domains", value_delimiter = ',')]
    exclude_domains: Vec<String>,

    /// Recency hint (day, week, month, year).
    #[arg(long, value_enum)]
    recency: Option<WebSearchRecencyArg>,

    /// Earliest publication date (YYYY-MM-DD).
    #[arg(long)]
    since: Option<String>,

    /// Latest publication date (YYYY-MM-DD).
    #[arg(long)]
    until: Option<String>,

    /// Maximum items to return.
    #[arg(long, default_value_t = 15)]
    top: usize,

    /// Number of top results to probe with web read diagnostics.
    #[arg(long = "probe-top", default_value_t = 4)]
    probe_top: usize,

    /// Maximum parallel network operations.
    #[arg(long = "max-parallel", default_value_t = 6)]
    max_parallel: usize,

    /// Optional run-tracking key for delta comparisons.
    #[arg(long = "track-key")]
    track_key: Option<String>,

    /// Emit full verbose payload (snippets + detailed score components).
    #[arg(long, default_value_t = false)]
    full: bool,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct WebReadArgs {
    /// URL(s) to read content from (repeatable or comma-separated).
    #[arg(long = "url", value_delimiter = ',')]
    url: Vec<String>,

    /// Optional file containing URLs (one per line, '#' comments allowed).
    #[arg(long = "urls-file")]
    urls_file: Option<PathBuf>,

    /// Maximum parallel URL fetches.
    #[arg(long = "max-parallel", default_value_t = 6)]
    max_parallel: usize,

    /// Max chars to keep per article text in default compact mode.
    #[arg(long = "max-chars", default_value_t = 2400)]
    max_chars: usize,

    /// Emit full verbose payload (full text + attempt details).
    #[arg(long, default_value_t = false)]
    full: bool,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct WebExtractArgs {
    /// URL to fetch and extract from.
    #[arg(long)]
    url: Option<String>,

    /// File path to extract from.
    #[arg(long)]
    file: Option<PathBuf>,

    /// Inline text to extract from (use heredoc for large content).
    #[arg(long)]
    text: Option<String>,

    /// Number of bullet points to extract (default: 10).
    #[arg(long, default_value = "10")]
    bullets: usize,

    /// Focus extraction on specific topic.
    #[arg(long)]
    focus: Option<String>,

    /// Output file path (JSON).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceScheduleArgs {
    /// Schedule kind: earnings | macro | all.
    #[arg(long, default_value = "all")]
    pub kind: String,
    /// Single date (YYYY-MM-DD). If set, overrides --from/--to.
    #[arg(long)]
    pub date: Option<String>,
    /// Start date (YYYY-MM-DD).
    #[arg(long = "from")]
    pub from: Option<String>,
    /// End date (YYYY-MM-DD).
    #[arg(long = "to")]
    pub to: Option<String>,
    /// Optional ticker filter for earnings rows (repeatable or comma-separated).
    #[arg(long, visible_alias = "tickers", value_delimiter = ',')]
    pub ticker: Vec<String>,
    /// Macro-only: keep only major US releases (CPI, PCE, GDP, jobs, FOMC, claims).
    #[arg(long, default_value_t = false)]
    pub major: bool,
    /// Minimum market cap for earnings (e.g. 10B, 500M, 1T).
    #[arg(long = "min-cap")]
    pub min_cap: Option<String>,
    /// Filter earnings by report time: pre-market | after-hours.
    #[arg(long)]
    pub time: Option<String>,
    /// Macro filtering profile: broad | market | major.
    #[arg(long = "macro-profile", default_value = "market")]
    pub macro_profile: String,
    /// Output format (json only).
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceRatePathArgs {
    /// Optional cache directory for prediction-market CSVs.
    #[arg(long)]
    pub cache_dir: Option<PathBuf>,
    /// DEPRECATED: accepted for backwards compatibility but ignored. The live
    /// API path always dominates; the CSV cache is only used when the live
    /// fetch fails. The auto/meeting/fallback distinction was a no-op.
    #[arg(long, default_value = "auto", hide = true)]
    pub source_mode: String,
    /// Output format (json only).
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Output file path.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceAuctionsArgs {
    /// Filter by security type: bill, note, bond, tips, frn, or all.
    #[arg(long, default_value = "all")]
    pub security_type: String,
    /// Number of recent auctions to return.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,
    /// Output format (json only).
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Output file path.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceCotArgs {
    /// Search query to filter by contract name (e.g. "gold", "crude oil", "10y note").
    #[arg(long)]
    pub query: Option<String>,
    /// Number of weeks of data to fetch. Default 12.
    #[arg(long, default_value_t = 12)]
    pub weeks: usize,
    /// Report type: auto (detect from query), disaggregated (commodities), or financial (rates/FX/equity).
    #[arg(long, default_value = "auto")]
    pub report: String,
    /// Max number of distinct contracts to return (default 15).
    #[arg(long)]
    pub limit: Option<usize>,
    /// Output format (json only).
    #[arg(long, default_value = "json")]
    pub format: String,
    /// Output file path.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceCurveArgs {
    /// Commodity to chart (e.g. oil, gold, natgas, silver, copper). Use --list to see all.
    #[arg(long)]
    pub commodity: Option<String>,
    /// Number of forward months to include (default 12, max 24).
    #[arg(long, default_value_t = 12)]
    pub months: usize,
    /// List supported commodities.
    #[arg(long, default_value_t = false)]
    pub list: bool,
    /// Output file path.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceNyfedArgs {
    /// Endpoint: rates | rrp | soma | dealers
    #[arg(long, default_value = "rates")]
    pub kind: String,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceVolsurfaceArgs {
    /// Comma-separated CBOE index symbols (default: all 9). Options: VIX,VIX9D,VIX3M,VIX6M,VIX1Y,VVIX,OVX,GVZ,SKEW
    #[arg(long)]
    pub symbols: Option<String>,
    /// Number of historical trading days (default: latest only)
    #[arg(long)]
    pub history: Option<usize>,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceStressArgs {
    /// Days of history (default: 30)
    #[arg(long, default_value_t = 30)]
    pub range: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceFiscalArgs {
    /// Endpoint: debt | statement | interest
    #[arg(long, default_value = "debt")]
    pub kind: String,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceEcbArgs {
    /// Preset: eurusd | fx_majors | estr | m3 | euribor | yield_curve | balance_sheet
    #[arg(long)]
    pub preset: Option<String>,
    /// SDMX dataset (e.g. EXR, BSI, FM, EST, YC). Use with --key.
    #[arg(long)]
    pub dataset: Option<String>,
    /// SDMX dimension key (e.g. D.USD.EUR.SP00.A). Use with --dataset.
    #[arg(long)]
    pub key: Option<String>,
    /// Start period (YYYY-MM-DD or YYYY-MM or YYYY).
    #[arg(long, default_value = "2025-01-01")]
    pub start: String,
    /// End period.
    #[arg(long)]
    pub end: Option<String>,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceEiaArgs {
    /// Preset: crude | gasoline | distillate | all | nat_gas
    #[arg(long)]
    pub preset: Option<String>,
    /// Custom API route (e.g. petroleum/stoc/wstk/data/).
    #[arg(long)]
    pub route: Option<String>,
    /// Start date (YYYY-MM-DD).
    #[arg(long)]
    pub start: Option<String>,
    /// Max observations to return (default 52 = ~1 year weekly).
    #[arg(long, default_value = "52")]
    pub length: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceBisArgs {
    /// Preset: policy_rates | assets | credit_gap | property | eer
    #[arg(long)]
    pub preset: Option<String>,
    /// SDMX dataset (e.g. WS_CBPOL). Use with --key.
    #[arg(long)]
    pub dataset: Option<String>,
    /// SDMX key (e.g. M.US+XM+JP+GB). Use with --dataset.
    #[arg(long)]
    pub key: Option<String>,
    /// Country codes (comma-separated, e.g. US,XM,JP,GB).
    #[arg(long)]
    pub countries: Option<String>,
    /// Start period (YYYY-MM).
    #[arg(long, default_value = "2020-01")]
    pub start: String,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceBojArgs {
    /// Preset: policy_rate | call_rate | monetary_base | balance_sheet | money_stock | tankan | fx
    #[arg(long)]
    pub preset: Option<String>,
    /// BOJ database name (e.g. IR01, FM01, BS01, CO). Use with --codes.
    #[arg(long)]
    pub db: Option<String>,
    /// BOJ series codes (comma-separated).
    #[arg(long)]
    pub codes: Option<String>,
    /// Start date (YYYYMM format for BOJ).
    #[arg(long, default_value = "202401")]
    pub start: String,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct FinanceBoeArgs {
    /// Preset: bank_rate | sonia | gilts | m4 | fx | all
    #[arg(long)]
    pub preset: Option<String>,
    /// Series codes (comma-separated, e.g. IUDBEDR,IUDSOIA).
    #[arg(long)]
    pub codes: Option<String>,
    /// Start date (DD/Mon/YYYY format, e.g. 01/Jan/2025).
    #[arg(long, default_value = "01/Jan/2025")]
    pub start: String,
    /// End date (DD/Mon/YYYY or "now").
    #[arg(long, default_value = "now")]
    pub end: String,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceFundamentalsArgs {
    /// Tickers to fetch (repeatable or comma-separated).
    #[arg(long, visible_alias = "ticker", value_delimiter = ',')]
    tickers: Vec<String>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceMoversArgs {
    /// Candidate universe: day_movers | day_gainers | day_losers | most_actives |
    /// small_cap_gainers | aggressive_small_caps | top_etfs | most_shorted | tickers.
    #[arg(long, default_value = "day_movers")]
    universe: String,

    /// Explicit tickers for universe=tickers, or to override screener discovery.
    #[arg(long, visible_alias = "ticker", value_delimiter = ',')]
    tickers: Vec<String>,

    /// Direction filter: gainers | losers | both.
    #[arg(long, default_value = "both")]
    direction: String,

    /// Sort by: percent | abs_percent | market_cap | value_change | dollar_volume | volume.
    #[arg(long, default_value = "value_change")]
    sort_by: String,

    /// Data provider for price/change: auto | yahoo | ibkr. Auto uses IBKR when configured, Yahoo otherwise.
    #[arg(long, default_value = "auto")]
    provider: String,

    /// Minimum market cap filter, e.g. 500M, 2B, 1.5T.
    #[arg(long)]
    min_market_cap: Option<String>,

    /// Maximum market cap filter, e.g. 2B for small/mid-cap screens.
    #[arg(long)]
    max_market_cap: Option<String>,

    /// Minimum absolute percent move.
    #[arg(long, default_value_t = 0.0)]
    min_change_pct: f64,

    /// Minimum last price.
    #[arg(long, default_value_t = 5.0)]
    min_price: f64,

    /// Minimum reported day volume.
    #[arg(long)]
    min_volume: Option<u64>,

    /// Optional sector filter (case-insensitive substring match against Yahoo screener `sector`).
    #[arg(long)]
    sector: Option<String>,

    /// Optional industry filter (case-insensitive substring match against Yahoo screener `industry`).
    #[arg(long)]
    industry: Option<String>,

    /// Number of movers to return.
    #[arg(long, default_value_t = 25)]
    limit: usize,

    /// Candidate count per Yahoo screener leg before local filtering/sorting.
    #[arg(long, default_value_t = 100)]
    scan_limit: usize,

    /// Optional IBKR account code. Used when --provider ibkr or auto detects IBKR setup.
    #[arg(long)]
    ibkr_account: Option<String>,

    /// Optional IBKR host override.
    #[arg(long)]
    ibkr_host: Option<String>,

    /// Optional IBKR port override.
    #[arg(long)]
    ibkr_port: Option<u16>,

    /// Optional IBKR client id override.
    #[arg(long)]
    ibkr_client_id: Option<i32>,

    /// Optional IBKR market data type: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    #[arg(long)]
    ibkr_market_data_type: Option<i32>,

    /// Optional IBKR timeout in seconds.
    #[arg(long)]
    ibkr_timeout_secs: Option<u64>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Also fetch and surface pre-market / after-hours move alongside regular-session change.
    #[arg(long, default_value_t = false)]
    include_extended_hours: bool,
}

#[derive(clap::Args, Debug)]
struct FinanceSearchArgs {
    /// Search query (e.g. "Apple" or "Inflation").
    #[arg(long, required = false)]
    query: Option<String>,

    /// Search query (positional alternative to --query).
    #[arg(index = 1, required = false)]
    query_positional: Option<String>,

    /// Data provider (auto | yahoo | ibkr). Auto uses the normal Yahoo+FRED search path.
    #[arg(long, default_value = "auto")]
    provider: String,

    /// Optional IBKR account code (e.g. U1234567). Used when --provider ibkr.
    #[arg(long)]
    ibkr_account: Option<String>,

    /// Optional IBKR host override.
    #[arg(long)]
    ibkr_host: Option<String>,

    /// Optional IBKR port override.
    #[arg(long)]
    ibkr_port: Option<u16>,

    /// Optional IBKR client id override.
    #[arg(long)]
    ibkr_client_id: Option<i32>,

    /// Optional IBKR market data type: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    #[arg(long)]
    ibkr_market_data_type: Option<i32>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Optional policy file override.
    #[arg(long = "policy-file")]
    policy_file: Option<PathBuf>,

    /// Policy mode: observe | assist | enforce.
    #[arg(long = "policy-mode", default_value = "observe")]
    policy_mode: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceOddsArgs {
    #[command(subcommand)]
    action: Option<FinanceOddsAction>,

    /// Data source: kalshi (default), polymarket, or auto (kalshi then polymarket).
    #[arg(long)]
    provider: Option<String>,
    /// Kalshi series ticker.
    #[arg(long)]
    series: Option<String>,

    /// Event ticker.
    #[arg(long)]
    event: Option<String>,

    /// Market ticker.
    #[arg(long)]
    market: Option<String>,

    /// Filter by status (e.g. open).
    #[arg(long)]
    status: Option<String>,

    /// Page size limit.
    #[arg(long)]
    limit: Option<usize>,

    /// Pagination cursor.
    #[arg(long)]
    cursor: Option<String>,

    /// Max pages to fetch (Kalshi list endpoints).
    #[arg(long)]
    max_pages: Option<usize>,

    /// List series (Kalshi only).
    #[arg(long)]
    list_series: bool,

    /// List events.
    #[arg(long)]
    list_events: bool,

    /// List markets.
    #[arg(long)]
    list_markets: bool,

    /// List tags (Polymarket only).
    #[arg(long)]
    list_tags: bool,

    /// Category filter (Kalshi list endpoints, and local CSV search when --search is used).
    #[arg(long)]
    category: Option<String>,

    /// Case-insensitive literal substring match (titles/tickers/slugs).
    #[arg(long, alias = "query")]
    search: Option<String>,

    /// Optional country filter for local CSV search (v1: US only).
    #[arg(long)]
    country: Option<String>,

    /// Minimum market volume in USD (local CSV search).
    #[arg(long = "min-volume", alias = "min_volume")]
    min_volume: Option<f64>,

    /// Return top N markets after ranking (local CSV search).
    #[arg(long)]
    top: Option<usize>,

    /// Ranking for local CSV search output.
    /// Options: relevance, volume, delta_prob, delta_yes_price, delta_volume.
    #[arg(long = "sort-by", alias = "sort_by", default_value = "relevance")]
    sort_by: String,
    /// Query profile: auto | macro | broad.
    #[arg(long, default_value = "auto")]
    profile: String,

    /// Optional policy file override.
    #[arg(long = "policy-file")]
    policy_file: Option<PathBuf>,

    /// Policy mode: observe | assist | enforce.
    #[arg(long = "policy-mode", default_value = "observe")]
    policy_mode: String,

    /// Only include markets that changed since the last sync (requires sync delta index).
    #[arg(long, default_value_t = false)]
    deltas_only: bool,

    /// Minimum absolute probability move (percentage points) since last sync.
    /// Requires sync delta index.
    #[arg(long = "min-delta-pp")]
    min_delta_pp: Option<f64>,

    /// Include compact ranking explanations in local CSV search output.
    #[arg(long, default_value_t = false)]
    explain: bool,

    /// Upgrade cache search results to live API prices (fresh bid/ask/volume). Default for --search.
    #[arg(long, default_value_t = false)]
    live: bool,

    /// Use only the local FTS cache for --search; faster but can be stale and skips live fallback.
    #[arg(long, conflicts_with = "live")]
    local: bool,

    /// Include mention/speech-prediction markets (filtered by default).
    #[arg(long, default_value_t = false)]
    include_mentions: bool,

    /// Include orderbook depth (heavier call; Polymarket orderbook supported).
    #[arg(long)]
    orderbook: bool,

    /// Orderbook depth (levels).
    #[arg(long)]
    depth: Option<usize>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum FinanceOddsAction {
    /// Sync prediction markets (Kalshi + Polymarket) to a local CSV cache.
    Sync(FinanceSyncArgs),

    /// Print local cache paths for odds CSVs.
    Where(FinanceOddsWhereArgs),
}

#[derive(clap::Args, Debug)]
struct FinanceOddsWhereArgs {
    /// Override cache directory (defaults to the same cache used by `eli finance sync`).
    #[arg(long)]
    cache_dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceOptionsArgs {
    /// Underlying ticker (e.g. INTC).
    #[arg(long, visible_alias = "tickers")]
    ticker: String,

    /// Expiration date (YYYY-MM-DD). If omitted, summary mode uses the first usable future expiry.
    #[arg(long)]
    expiry: Option<String>,

    /// Target days to expiry. If set and --expiry is omitted, picks the closest listed expiry.
    #[arg(long = "target-dte")]
    target_dte: Option<i64>,

    /// As-of timestamp. Options providers currently support current live/delayed snapshots only.
    #[arg(long = "as-of")]
    as_of: Option<String>,

    /// Filter: calls | puts | both (default: both).
    #[arg(long = "type", value_name = "calls|puts|both")]
    option_type: Option<String>,

    /// Only return strikes within this percentage of the underlying (e.g. 10 = +/-10%).
    #[arg(long = "near-money")]
    near_money: Option<f64>,

    /// Return summary metrics only (no full chain).
    #[arg(long)]
    summary: bool,

    /// List available expirations only.
    #[arg(long)]
    expirations: bool,

    /// Fetch ALL expirations and compute cross-expiry analytics.
    /// Dumps full chain to --out file, prints term structure summary to stdout.
    #[arg(long)]
    all: bool,

    /// Data provider (auto | yahoo | ibkr). Auto uses Yahoo baseline plus IBKR overlay when reachable.
    #[arg(long, default_value = "auto")]
    provider: String,

    /// Optional IBKR account code (e.g. U1234567). Used when --provider ibkr.
    #[arg(long)]
    ibkr_account: Option<String>,

    /// Optional IBKR host override.
    #[arg(long)]
    ibkr_host: Option<String>,

    /// Optional IBKR port override.
    #[arg(long)]
    ibkr_port: Option<u16>,

    /// Optional IBKR client id override.
    #[arg(long)]
    ibkr_client_id: Option<i32>,

    /// Optional IBKR market data type: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    #[arg(long)]
    ibkr_market_data_type: Option<i32>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceSyncArgs {
    /// Sources to sync: kalshi, polymarket (comma-separated). Default: both.
    #[arg(long, value_delimiter = ',')]
    sources: Vec<String>,

    /// Debug only: frontier-sample page cap per source. Hides coverage and is not a normal sync control.
    #[arg(long)]
    max_pages: Option<usize>,

    /// Fail if pagination/coverage checks indicate incomplete source exhaustion.
    #[arg(long)]
    strict: bool,

    /// Include sports markets/events in sync output (default: false).
    #[arg(long)]
    include_sports: bool,

    /// Include Kalshi historical markets (archived/settled tier). Default: false.
    #[arg(long, hide = true)]
    include_historical: bool,

    /// Fast refresh from Kalshi websocket ticker stream using cached baseline (no full re-pagination).
    #[arg(long)]
    stream_refresh: bool,

    /// Breadth heartbeat in hours for stream refresh mode. If cached baseline is older, force strict REST anchor sync (default: 6).
    #[arg(long, hide = true)]
    refresh_heartbeat_hours: Option<u64>,

    /// WebSocket listen window in seconds for stream refresh mode (default: 300).
    #[arg(long, hide = true)]
    stream_refresh_timeout_secs: Option<u64>,

    /// Cache directory for CSV files (defaults to platform cache dir).
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json", hide = true)]
    format: String,

    /// Emit full verbose payload on stdout (default is compact/token-efficient).
    #[arg(long, hide = true)]
    full: bool,

    /// Optional policy file override.
    #[arg(long = "policy-file", hide = true)]
    policy_file: Option<PathBuf>,

    /// Policy mode: observe | assist | enforce.
    #[arg(long = "policy-mode", default_value = "observe", hide = true)]
    policy_mode: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinancePaperArgs {
    /// Command: trade | positions | trades | mark | reset
    #[arg(long, value_enum, default_value = "trade")]
    command: FinancePaperCommandArg,

    /// Execution mode (v1: simulated local fills)
    #[arg(long, value_enum, default_value = "simulated")]
    mode: FinancePaperModeArg,

    /// Paper account name.
    #[arg(long, default_value = "default")]
    account: String,

    /// Provider for trade/mark pricing (kalshi|polymarket).
    #[arg(long)]
    provider: Option<String>,

    /// Market ticker or market id (required for --command trade).
    #[arg(long)]
    market: Option<String>,

    /// Side (yes|no) for --command trade.
    #[arg(long, value_enum)]
    side: Option<FinancePaperSideArg>,

    /// Order action (buy|sell) for --command trade.
    #[arg(long, value_enum)]
    action: Option<FinancePaperOrderActionArg>,

    /// Quantity/contracts for --command trade.
    #[arg(long)]
    qty: Option<f64>,

    /// Optional manual fill price in probability units [0,1]. If omitted, uses live midpoint.
    #[arg(long)]
    price: Option<f64>,

    /// Starting paper cash for account init/reset (default: 10000).
    #[arg(long)]
    starting_cash: Option<f64>,

    /// Trade history limit for --command trades (default: 50).
    #[arg(long)]
    limit: Option<usize>,

    /// Optional custom cache dir or full state file path.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Output format (json only).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceFilingsArgs {
    /// Ticker to fetch filings for.
    #[arg(long, visible_alias = "tickers")]
    ticker: String,

    /// Form types to include (comma-separated), e.g. 8-K,10-K,10-Q. Defaults to 8-K,10-K,10-Q.
    #[arg(long, value_delimiter = ',')]
    forms: Vec<String>,

    /// Max number of filings to return.
    #[arg(long, default_value_t = 5)]
    limit: usize,

    /// Download exactly one file: the primary document for the most recent matching filing.
    #[arg(
        long,
        conflicts_with_all = ["no_download", "download_all", "primary_only", "include_text"]
    )]
    single_file: bool,

    /// Do not download filing files; return SEC metadata and URLs only.
    #[arg(long)]
    no_download: bool,

    /// Download every document listed in each filing index, not just the primary document.
    #[arg(long)]
    download_all: bool,

    /// Only download the filing index JSON and primary document; skip filtered exhibits.
    #[arg(long)]
    primary_only: bool,

    /// Deprecated alias for the old text mode. Now downloads raw filing files without parsing.
    #[arg(long)]
    include_text: bool,

    /// Include the primary filing document's raw decoded text inline. Use --max-chars to cap it.
    #[arg(long)]
    raw_text: bool,

    /// For earnings 8-Ks (item 2.02), inline the press-release exhibit (EX-99.1) as parsed
    /// plain text in `press_release_text`. The XBRL cover (primary doc) is bypassed.
    #[arg(long)]
    press_release_text: bool,

    /// Cap inline raw filing text when --raw-text is set.
    #[arg(long)]
    max_chars: Option<usize>,

    /// SEC User-Agent override with contact email, e.g. "eli-cli (mailto:me@example.com)".
    #[arg(long)]
    user_agent: Option<String>,

    /// Override cache directory (defaults to Eli's cache dir).
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceTimeseriesArgs {
    /// Preset ticker group. 17 presets: macro, yield_curve, liquidity, crypto, forex_majors,
    /// credit (ICE BofA OAS), financial_conditions (NFCI/STLFSI4/VIX), recession (Sahm/spreads/claims),
    /// fed_balance_sheet (QE/QT/TGA/RRP), housing (Case-Shiller/starts/mortgage), labor (NFP/JOLTS/wages),
    /// inflation (CPI/PCE/sticky/median/breakevens), real_rates (TIPS yields), consumer_credit (delinquency/SLOOS),
    /// energy (WTI/Brent/NG/RBOB/HO/Gasoil via IBKR), commodities (CL/GC/SI/HG/grains/softs via IBKR),
    /// treasuries (ZT/ZF/ZN/ZB + VIX futures via IBKR).
    /// Expands to predefined tickers with default range/granularity. User flags override defaults.
    #[arg(long)]
    preset: Option<String>,

    /// Tickers to fetch (repeatable or comma-separated).
    #[arg(long, visible_alias = "ticker", value_delimiter = ',')]
    tickers: Vec<String>,

    /// Optional file with tickers (one per line).
    #[arg(long)]
    tickers_file: Option<PathBuf>,

    /// Lookback range (e.g. 1d, 12mo, 5y).
    #[arg(long, default_value = "1y")]
    range: String,

    /// Candle size / sampling granularity (e.g. 10m, 1h, 1d, 1w, 1mo).
    #[arg(long, default_value = "1d")]
    granularity: String,

    /// Explicit window start (RFC3339 or YYYY-MM-DD). Must be used with --end.
    #[arg(long)]
    start: Option<String>,

    /// Explicit window end (RFC3339 or YYYY-MM-DD). Must be used with --start.
    #[arg(long)]
    end: Option<String>,

    /// End timestamp for the window (RFC3339). If you pass YYYY-MM-DD, it's treated as end-of-day UTC. Defaults to now (UTC).
    #[arg(long)]
    as_of: Option<String>,

    /// Data provider (auto | yahoo | fred | ibkr | pyth | binance). "auto" routes by ticker prefix: PYTH:/CLEV:/IBKR:/BN:/FRED: → matching provider; numeric (Polymarket) and KX*-prefix (Kalshi) auto-detected; bare names → Yahoo, with FRED fallback for macro-style IDs.
    #[arg(long, default_value = "auto")]
    provider: String,

    /// Optional IBKR account code (e.g. U1234567). Used when --provider ibkr.
    #[arg(long)]
    ibkr_account: Option<String>,

    /// Optional IBKR host override.
    #[arg(long)]
    ibkr_host: Option<String>,

    /// Optional IBKR port override.
    #[arg(long)]
    ibkr_port: Option<u16>,

    /// Optional IBKR client id override.
    #[arg(long)]
    ibkr_client_id: Option<i32>,

    /// Optional IBKR market data type: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    #[arg(long)]
    ibkr_market_data_type: Option<i32>,

    /// Optional explicit prediction market provider to include as a timeseries series (kalshi | polymarket).
    #[arg(long)]
    odds_provider: Option<String>,

    /// Optional prediction market identifier. Kalshi: market ticker. Polymarket: market ID or slug.
    #[arg(long)]
    odds_market: Option<String>,

    /// Prediction market side to include for --odds-market (yes | no). Defaults to yes.
    #[arg(long, default_value = "yes")]
    odds_side: String,

    /// Safety cap for points per ticker.
    #[arg(long)]
    max_points_per_ticker: Option<usize>,

    /// Override cache directory (defaults to Eli's cache dir).
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Output format (currently: json).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
struct FinanceIbkrArgs {
    /// IBKR command surface.
    #[arg(long, value_enum)]
    command: FinanceIbkrCommandArg,

    /// Optional IBKR account code (e.g. U1234567).
    #[arg(long)]
    account: Option<String>,

    /// Optional IBKR host override.
    #[arg(long)]
    host: Option<String>,

    /// Optional IBKR port override.
    #[arg(long)]
    port: Option<u16>,

    /// Optional IBKR client id override.
    #[arg(long)]
    client_id: Option<i32>,

    /// Optional IBKR market data type: 1 live, 2 frozen, 3 delayed, 4 delayed-frozen.
    #[arg(long)]
    market_data_type: Option<i32>,

    /// Optional timeout in seconds for the bridge request.
    #[arg(long)]
    timeout_secs: Option<u64>,

    /// Tickers for snapshot / timeseries commands (repeatable or comma-separated).
    #[arg(long, value_delimiter = ',')]
    tickers: Vec<String>,

    /// Range for timeseries requests (e.g. 1d, 1mo, 1y).
    #[arg(long, default_value = "1mo")]
    range: String,

    /// Granularity for timeseries requests (e.g. 1min, 5min, 1h, 1d).
    #[arg(long, default_value = "1day")]
    granularity: String,

    /// Contract symbol for order placement.
    #[arg(long)]
    symbol: Option<String>,

    /// Contract security type for orders (default: STK).
    #[arg(long)]
    sec_type: Option<String>,

    /// Contract exchange (default: SMART).
    #[arg(long)]
    exchange: Option<String>,

    /// Contract primary exchange.
    #[arg(long)]
    primary_exchange: Option<String>,

    /// Contract currency (default: USD).
    #[arg(long)]
    currency: Option<String>,

    /// Contract expiry / contract month (e.g. 20260320).
    #[arg(long)]
    expiry: Option<String>,

    /// Contract strike price.
    #[arg(long)]
    strike: Option<f64>,

    /// Contract right (C/P).
    #[arg(long)]
    right: Option<String>,

    /// Contract multiplier.
    #[arg(long)]
    multiplier: Option<String>,

    /// Trading class.
    #[arg(long)]
    trading_class: Option<String>,

    /// Order side for place-order (BUY or SELL).
    #[arg(long)]
    side: Option<String>,

    /// Order type for place-order (MKT, LMT, STP, STP LMT).
    #[arg(long)]
    order_type: Option<String>,

    /// Quantity for place-order.
    #[arg(long)]
    quantity: Option<f64>,

    /// Limit price for limit orders.
    #[arg(long)]
    limit_price: Option<f64>,

    /// Stop price for stop orders.
    #[arg(long)]
    stop_price: Option<f64>,

    /// Time in force (default: DAY).
    #[arg(long)]
    tif: Option<String>,

    /// Order id for cancel-order.
    #[arg(long)]
    order_id: Option<i32>,

    /// Optional account summary tag filter.
    #[arg(long)]
    tags: Option<String>,

    /// Output format (json only).
    #[arg(long, default_value = "json")]
    format: String,

    /// Write full JSON output to a file instead of stdout.
    #[arg(long)]
    out: Option<PathBuf>,
}
