// MCP (Model Context Protocol) stdio server.
// Exposes eli finance and web tools as native Claude Code tools via JSON-RPC 2.0.
// Usage: eli mcp   ← Claude Code connects automatically via .mcp.json

async fn cmd_mcp() -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    while let Some(request) = mcp_read_request(&mut reader).await? {
        // Notifications have no id and require no response.
        let method = match request.get("method").and_then(|m| m.as_str()) {
            Some(m) => m.to_string(),
            None => continue,
        };
        if method.starts_with("notifications/") {
            continue;
        }

        let id = request
            .get("id")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let response = match method.as_str() {
            "initialize" => mcp_initialize(id, &request),
            "tools/list" => mcp_tools_list(id),
            "tools/call" => mcp_tools_call(id, &request).await,
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            }),
        };

        mcp_write_response(&mut out, &response)?;
    }

    Ok(())
}

async fn mcp_read_request<R>(reader: &mut R) -> Result<Option<serde_json::Value>>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt as _;

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request = serde_json::from_str(trimmed).context("parse mcp request json")?;
        return Ok(Some(request));
    }
}

fn mcp_write_response<W: std::io::Write>(out: &mut W, response: &serde_json::Value) -> Result<()> {
    let body = serde_json::to_string(response).context("serialize response")?;
    writeln!(out, "{body}")?;
    out.flush()?;
    Ok(())
}

/// The eli-terminal wordmark, embedded so the binary is self-contained. Served at
/// /favicon.ico + /favicon.png (so a connector UI fetching the domain favicon gets the
/// eli mark instead of falling back to the ngrok-free.dev favicon) and also handed to the
/// client via serverInfo.icons below (MCP SEP-973, the spec-correct path).
const FAVICON_PNG: &[u8] = include_bytes!("favicon.png");

fn mcp_initialize(id: serde_json::Value, request: &serde_json::Value) -> serde_json::Value {
    // Version negotiation (MCP spec): when the client sends a protocolVersion, echo it
    // back rather than forcing our own — a strict client that requested an older version
    // can reject a newer one it doesn't recognize. Fall back to our latest when absent.
    // Our surface is tools/* only (declared in capabilities), stable across these versions.
    let protocol_version = request
        .get("params")
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("2025-11-25")
        .to_string();

    let mut server_info = json!({
        "name": "market-search",
        "title": "eli terminal",
        "version": env!("CARGO_PKG_VERSION"),
    });

    // SEP-973 serverInfo.icons/websiteUrl: claude.ai's web/mobile connector currently
    // REJECTS an initialize result that carries serverInfo.icons (claude-ai-mcp #474:
    // "your account was authorized, but … returned an error when connecting / ofid_")
    // and does not render it anyway (#152) — so strip the branding block for the
    // claude.ai client (it still gets the logo from /favicon.ico). Other clients
    // (Claude Code/Desktop) keep it, with the spec-correct `sizes` ARRAY shape.
    // claude.ai sends clientInfo.name = "Anthropic/ClaudeAI".
    let client_name = request
        .get("params")
        .and_then(|p| p.get("clientInfo"))
        .and_then(|c| c.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if client_name != "Anthropic/ClaudeAI" {
        use base64::Engine as _;
        let icon_data_uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(FAVICON_PNG)
        );
        server_info["websiteUrl"] = json!("https://eliterminal.com");
        server_info["icons"] =
            json!([{ "src": icon_data_uri, "mimeType": "image/png", "sizes": ["256x256"] }]);
    }

    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol_version,
            "capabilities": { "tools": {} },
            "serverInfo": server_info
        }
    })
}

/// Which tool families this server advertises.
///
/// One binary, two products: `market-search` ships the finance catalog,
/// `legal-search` ships the legal one, and `--profile all` (or setting
/// `ELI_MCP_PROFILE=all`) exposes both to a client that wants everything.
/// Filtering happens at `tools/list` only — `tools/call` still dispatches any
/// known tool, so a client that cached an older catalog keeps working.
fn mcp_profile() -> String {
    std::env::var("ELI_MCP_PROFILE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "all".to_string())
}

fn mcp_catalog() -> Vec<serde_json::Value> {
    let finance: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("mcp_tools.json")).expect("valid MCP tools catalog");
    let legal: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("legal_tools.json")).expect("valid legal tools catalog");

    match mcp_profile().as_str() {
        "finance" | "market" | "markets" => finance,
        "legal" | "law" => legal,
        _ => finance.into_iter().chain(legal).collect(),
    }
}

fn mcp_tools_list(id: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": { "tools": mcp_catalog() }
    })
}

/// When true, return full raw JSON inline (for HTTP/web clients that can't read files).
/// When false, use the summary + file system (for Claude Code stdio).
async fn mcp_tools_call(id: serde_json::Value, request: &serde_json::Value) -> serde_json::Value {
    mcp_tools_call_inner(id, request, false).await
}

async fn mcp_tools_call_full(id: serde_json::Value, request: &serde_json::Value) -> serde_json::Value {
    mcp_tools_call_inner(id, request, true).await
}

async fn mcp_tools_call_inner(id: serde_json::Value, request: &serde_json::Value, full_output: bool) -> serde_json::Value {
    let params = match request.get("params") {
        Some(p) => p,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "Missing params" }
            })
        }
    };

    let tool_name = match params.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_string(),
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "Missing tool name" }
            })
        }
    };

    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    // ── data_query: jq on cached /tmp/eli_* files (no subprocess needed) ──
    if tool_name == "data_query" {
        return mcp_data_query(id, &args).await;
    }

    // claude.ai / web (full_output, no file access) can't read the local files finance_filings
    // downloads — its default response is local paths + URLs with no inline content, which sends
    // a no-file-access client bouncing to WebSearch+WebFetch. So in HTTP mode, when the caller
    // didn't already ask for text, auto-return the cleaned filing TEXT inline (and skip the
    // pointless server-side download). Stdio (Claude Code, file access) is untouched: it keeps
    // the download + local paths to explore the raw file directly.
    if full_output && tool_name == "finance_filings" {
        if let Some(obj) = args.as_object_mut() {
            let has_text = obj.get("raw_text").and_then(|v| v.as_bool()).unwrap_or(false)
                || obj
                    .get("press_release_text")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            if !has_text {
                obj.insert("raw_text".to_string(), serde_json::Value::Bool(true));
                obj.entry("max_chars").or_insert(serde_json::json!(50_000));
                obj.entry("download").or_insert(serde_json::Value::Bool(false));
            }
        }
    }

    let cli_args = match mcp_build_cli_args(&tool_name, &args) {
        Ok(a) => a,
        Err(e) => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": format!("Invalid arguments: {e}") }
            })
        }
    };

    match mcp_run_subprocess(cli_args).await {
        Ok(output) => {
            // HTTP mode (web/phone clients): return full raw JSON inline.
            // These clients can't read files — they need the data in the response.
            //
            // Stdio mode (Claude Code): summary + file. The AI reads the file
            // only when it needs drill-down detail.
            const INLINE_THRESHOLD: usize = 2_000;

            let response_text = if full_output {
                // HTTP mode (claude.ai / web): return everything inline. For a large
                // timeseries, also inject a precomputed `_summary` (start/end/high/low +
                // freshness `last_t` + the turning-point `path`) into the JSON. These
                // clients can't run code over the raw candle array, so without this they
                // can't see the shape of a move — only the wall of numbers. Additive and
                // fallback-safe: on any parse hiccup we return the raw output unchanged.
                if tool_name == "finance_timeseries" && output.len() > INLINE_THRESHOLD {
                    match (
                        serde_json::from_str::<serde_json::Value>(&output),
                        mcp_timeseries_summary(&output),
                    ) {
                        (Ok(mut v), Some(sum)) => {
                            match (v.as_object_mut(), serde_json::from_str::<serde_json::Value>(&sum))
                            {
                                (Some(obj), Ok(sum_v)) => {
                                    obj.insert("_summary".to_string(), sum_v);
                                    serde_json::to_string(&v).unwrap_or(output)
                                }
                                _ => output,
                            }
                        }
                        _ => output,
                    }
                } else if tool_name == "finance_filings" {
                    // No-file-access client: drop the local-only path fields (download_dir,
                    // primary_doc_path, etc.) — they're unreachable over MCP and just crowd out
                    // the inline `raw_text` we auto-returned above. Keep the SEC `url`/index URL.
                    match serde_json::from_str::<serde_json::Value>(&output) {
                        Ok(mut v) => {
                            if let Some(arr) = v.get_mut("filings").and_then(|f| f.as_array_mut()) {
                                for f in arr.iter_mut() {
                                    if let Some(o) = f.as_object_mut() {
                                        for k in [
                                            "download_dir",
                                            "primary_doc_path",
                                            "index_json_path",
                                            "downloaded_files",
                                            "text_path",
                                        ] {
                                            o.remove(k);
                                        }
                                    }
                                }
                                serde_json::to_string(&v).unwrap_or(output)
                            } else {
                                output
                            }
                        }
                        Err(_) => output,
                    }
                } else {
                    output
                }
            } else if output.len() <= INLINE_THRESHOLD {
                // Small outputs: return inline (yield_curve, rate_path, auctions, etc.)
                output
            } else {
                // Strip repeated metadata from individual items before saving.
                // Hoists one copy to top-level _meta, strips from array items.
                let cleaned = mcp_strip_metadata(&output);
                let save_data = cleaned.as_deref().unwrap_or(&output);

                // Save (cleaned) output to file
                let ts = chrono::Utc::now().timestamp_millis();
                let path = format!("/tmp/eli_{tool_name}_{ts}.json");
                let saved = std::fs::write(&path, save_data).is_ok();

                // Build per-tool compact summary
                let summary = mcp_build_summary(&tool_name, &output);

                if saved {
                    format!(
                        "{{\
                        \"_file\":\"{path}\",\
                        \"_chars\":{chars},\
                        {summary}\
                        }}",
                        path = path,
                        chars = save_data.len(),
                        summary = summary,
                    )
                } else {
                    // File save failed — return truncated inline
                    let truncated = &output[..4000.min(output.len())];
                    truncated.to_string()
                }
            };
            // Prepend wall-clock timestamp to every tool response so the AI
            // always knows the current date/time without a separate call.
            let now_local = chrono::Local::now();
            let response_text = format!(
                "[current_time: {} | {}]\n{}",
                now_local.format("%A %B %-d, %Y %l:%M %p %Z"),
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
                response_text
            );
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": response_text }]
                }
            })
        }
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": format!("Tool execution failed: {e}") }
        }),
    }
}

/// Handle data_query: run jq on a cached /tmp/eli_* file.
/// This is the "filing cabinet drawer pull" — lets the AI extract specific
/// slices from large cached outputs without loading the whole file into context.
async fn mcp_data_query(id: serde_json::Value, args: &serde_json::Value) -> serde_json::Value {
    let file = match args.get("file").and_then(|f| f.as_str()) {
        Some(f) => f,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "file parameter required" }
            });
        }
    };

    // Security: only allow /tmp/eli_* files
    if !file.starts_with("/tmp/eli_") {
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": "file must be a /tmp/eli_* path from a previous tool call" }
        });
    }

    if !std::path::Path::new(file).exists() {
        return json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": format!("File not found: {file}") }
        });
    }

    let jq_expr = match args.get("jq").and_then(|j| j.as_str()) {
        Some(j) => j,
        None => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32602, "message": "jq expression required" }
            });
        }
    };

    // Run jq on the file
    let result = TokioCommand::new("jq")
        .arg("-c")
        .arg(jq_expr)
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;

    match result {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": format!("jq error: {}", stderr.trim()) }
                });
            }
            // Apply same inline threshold — if query result is still huge, save it
            const QUERY_THRESHOLD: usize = 4_000;
            let response_text = if stdout.len() <= QUERY_THRESHOLD {
                stdout
            } else {
                let ts = chrono::Utc::now().timestamp_millis();
                let out_path = format!("/tmp/eli_query_{ts}.json");
                let _ = std::fs::write(&out_path, &stdout);
                format!(
                    "{{\"_file\":\"{}\",\"_chars\":{}}}",
                    out_path, stdout.len()
                )
            };
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [{ "type": "text", "text": response_text }]
                }
            })
        }
        Err(_) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32603, "message": "jq not found — install with: brew install jq" }
        }),
    }
}

/// Strip repeated per-item metadata keys before saving to file.
/// Hoists first-seen freshness/run_meta to top-level _meta, removes from items.
/// Returns None if parsing fails or no stripping needed.
fn mcp_strip_metadata(output: &str) -> Option<String> {
    let mut root: serde_json::Value = serde_json::from_str(output).ok()?;

    // Keys to strip from individual array items
    const STRIP_KEYS: &[&str] = &[
        "freshness",
        "delta_context",
        "delta_since_last_sync",
        "run_meta",
        "collected_at",
        "transport_received",
        "transport_origin",
    ];

    // Find arrays in the JSON (markets, snapshots, indicators, positions, series, etc.)
    let array_keys: Vec<String> = root
        .as_object()?
        .iter()
        .filter(|(_, v)| v.is_array())
        .map(|(k, _)| k.clone())
        .collect();

    let mut stripped_any = false;
    let mut hoisted_meta = serde_json::Map::new();

    for key in &array_keys {
        if let Some(arr) = root.get_mut(key).and_then(|v| v.as_array_mut()) {
            for item in arr.iter_mut() {
                if let Some(obj) = item.as_object_mut() {
                    for &strip_key in STRIP_KEYS {
                        if let Some(removed) = obj.remove(strip_key) {
                            stripped_any = true;
                            // Hoist first-seen value to _meta
                            if !hoisted_meta.contains_key(strip_key) {
                                hoisted_meta.insert(strip_key.to_string(), removed);
                            }
                        }
                    }
                }
            }
        }
    }

    if !stripped_any {
        return None;
    }

    // Add hoisted metadata at top level
    if !hoisted_meta.is_empty() {
        if let Some(obj) = root.as_object_mut() {
            obj.insert(
                "_meta".to_string(),
                serde_json::Value::Object(hoisted_meta),
            );
        }
    }

    serde_json::to_string(&root).ok()
}

/// Detect turning points (swings) in a close series via a dual-extreme ZigZag.
/// `reversal` is the minimum move (price units) away from a running extreme that
/// confirms a pivot. Returns pivot indices, always including the first and last point.
/// This is what powers the compact "76>80>42>90" path in the timeseries summary:
/// start/end/high/low alone hide the ORDER of moves; the pivot path shows the shape.
fn zigzag_pivots(values: &[f64], reversal: f64) -> Vec<usize> {
    let n = values.len();
    if n <= 2 {
        return (0..n).collect();
    }
    let mut pivots: Vec<usize> = vec![0];
    let mut last_pivot = 0usize;
    let mut max_i = 0usize;
    let mut min_i = 0usize;
    for i in 1..n {
        if values[i] > values[max_i] {
            max_i = i;
        }
        if values[i] < values[min_i] {
            min_i = i;
        }
        // A top is confirmed when price falls `reversal` below the running max that
        // formed after the last pivot; symmetrically for a bottom.
        if max_i > last_pivot && values[max_i] - values[i] >= reversal {
            pivots.push(max_i);
            last_pivot = max_i;
            min_i = i;
            max_i = i;
        } else if min_i > last_pivot && values[i] - values[min_i] >= reversal {
            pivots.push(min_i);
            last_pivot = min_i;
            max_i = i;
            min_i = i;
        }
    }
    if *pivots.last().unwrap() != n - 1 {
        pivots.push(n - 1);
    }
    pivots
}

/// Compute per-ticker summary stats from a timeseries JSON response.
/// Returns a JSON string like `{"SPY":{"start":550.0,"end":530.0,...},...}` or None on parse failure.
fn mcp_timeseries_summary(output: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(output).ok()?;
    let series = v.get("series").and_then(|s| s.as_array())?;

    let mut map = serde_json::Map::new();
    for entry in series {
        let ticker = entry.get("ticker").and_then(|t| t.as_str())?;
        let candles = entry.get("candles").and_then(|c| c.as_array())?;
        if candles.is_empty() {
            continue;
        }
        let get_close = |c: &serde_json::Value| c.get("c").and_then(|x| x.as_f64());
        let start = get_close(candles.first()?)?;
        let end = get_close(candles.last()?)?;
        let return_pct = if start != 0.0 {
            (end / start - 1.0) * 100.0
        } else {
            0.0
        };
        let get_ts = |c: &serde_json::Value| c.get("t").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let (high, low, high_date, low_date) = candles
            .iter()
            .fold(
                (f64::NEG_INFINITY, f64::INFINITY, String::new(), String::new()),
                |(h, l, hd, ld), c| {
                    let hi = c.get("h").and_then(|x| x.as_f64()).unwrap_or(h);
                    let lo = c.get("l").and_then(|x| x.as_f64()).unwrap_or(l);
                    let ts = get_ts(c);
                    let new_hd = if hi > h { ts.clone() } else { hd };
                    let new_ld = if lo < l { ts } else { ld };
                    (h.max(hi), l.min(lo), new_hd, new_ld)
                },
            );
        let range = high - low;
        let position_pct = if range > 0.0 {
            ((end - low) / range * 1000.0).round() / 10.0
        } else {
            50.0
        };
        // Annualised volatility — detect candle period from timestamps to use correct √N
        // √252 is only correct for daily candles; hourly needs √(252*6.5), etc.
        let ann_factor = if candles.len() >= 2 {
            let t0 = candles[0].get("t").and_then(|t| t.as_f64());
            let t1 = candles[1].get("t").and_then(|t| t.as_f64());
            if let (Some(t0), Some(t1)) = (t0, t1) {
                let diff = (t1 - t0).abs();
                // If timestamps look like milliseconds (>1e10), convert to seconds
                let diff_secs = if diff > 1e9 { diff / 1000.0 } else { diff };
                if diff_secs < 120.0        { 252.0 * 390.0 }  // 1-min (390 min/day)
                else if diff_secs < 600.0   { 252.0 * 78.0  }  // 5-min
                else if diff_secs < 1800.0  { 252.0 * 26.0  }  // 15-min
                else if diff_secs < 7200.0  { 252.0 * 6.5   }  // 1-hour
                else if diff_secs < 28800.0 { 252.0 * 2.0   }  // 4-hour
                else                        { 252.0          }  // daily or longer
            } else { 252.0 }
        } else { 252.0f64 };
        let closes: Vec<f64> = candles.iter().filter_map(get_close).collect();
        let vol_ann = if closes.len() >= 2 {
            let log_rets: Vec<f64> = closes
                .windows(2)
                .map(|w| (w[1] / w[0]).ln())
                .filter(|r| r.is_finite())
                .collect();
            if log_rets.len() >= 2 {
                let mean = log_rets.iter().sum::<f64>() / log_rets.len() as f64;
                let var = log_rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>()
                    / (log_rets.len() - 1) as f64;
                (var.sqrt() * ann_factor.sqrt() * 100.0 * 10.0).round() / 10.0
            } else {
                0.0
            }
        } else {
            0.0
        };
        // Truncate timestamps to date-only for readability (first 10 chars = YYYY-MM-DD)
        let high_date_short = if high_date.len() >= 10 { &high_date[..10] } else { &high_date };
        let low_date_short = if low_date.len() >= 10 { &low_date[..10] } else { &low_date };

        // Freshness stamp + turning-point path. start/end/high/low cannot convey the
        // ORDER of moves (V-bottom vs late spike); the path makes the shape explicit
        // for a consumer that can't recompute it from the raw file (e.g. claude.ai).
        let pairs: Vec<(String, f64)> = candles
            .iter()
            .filter_map(|c| Some((get_ts(c), get_close(c)?)))
            .collect();
        let first_t = pairs
            .first()
            .map(|(t, _)| if t.len() >= 16 { t[..16].to_string() } else { t.clone() })
            .unwrap_or_default();
        let last_t = pairs
            .last()
            .map(|(t, _)| if t.len() >= 16 { t[..16].to_string() } else { t.clone() })
            .unwrap_or_default();
        let path = {
            let vals: Vec<f64> = pairs.iter().map(|(_, c)| *c).collect();
            if vals.len() >= 2 {
                let rng = high - low;
                // Start the reversal threshold at ~10% of the high-low range and widen
                // until the path is compact (<=8 pivots), so it stays readable.
                let mut rev = if rng > 0.0 { rng * 0.10 } else { f64::INFINITY };
                let mut piv = zigzag_pivots(&vals, rev);
                let mut guard = 0;
                while piv.len() > 8 && guard < 6 {
                    rev *= 1.6;
                    piv = zigzag_pivots(&vals, rev);
                    guard += 1;
                }
                piv.iter()
                    .map(|&i| {
                        let d = &pairs[i].0;
                        let d_short = if d.len() >= 10 { &d[..10] } else { d.as_str() };
                        format!("{}({})", (vals[i] * 100.0).round() / 100.0, d_short)
                    })
                    .collect::<Vec<_>>()
                    .join(">")
            } else {
                String::new()
            }
        };
        map.insert(
            ticker.to_string(),
            serde_json::json!({
                "start": (start * 100.0).round() / 100.0,
                "end":   (end   * 100.0).round() / 100.0,
                "return_pct": (return_pct * 10.0).round() / 10.0,
                "high": (high * 100.0).round() / 100.0,
                "high_date": high_date_short,
                "low":  (low  * 100.0).round() / 100.0,
                "low_date": low_date_short,
                "position_pct": position_pct,
                "vol_ann_pct": vol_ann,
                "n_candles": candles.len(),
                "first_t": first_t,
                "last_t": last_t,
                "path": path,
            }),
        );
    }
    if map.is_empty() {
        None
    } else {
        serde_json::to_string(&serde_json::Value::Object(map)).ok()
    }
}

/// Build a compact summary string for the MCP response.
/// Each tool type extracts the 5-10 key numbers/facts the AI needs to reason.
/// The full data is always available at the file path.
/// Returns a JSON fragment (without outer braces) to be embedded in the response object.
fn mcp_build_summary(tool: &str, output: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(output) {
        Ok(v) => v,
        Err(_) => return format!("\"_summary\":null"),
    };

    match tool {
        "finance_odds" => {
            // Preserve odds search ranking. The search layer already combines provider
            // rank, live hydration, relevance, freshness, and source diversity; MCP
            // should not re-rank by volume or literal title substring.
            let mut lines = Vec::new();
            let mut total_vol_usd: f64 = 0.0;
            let mut kalshi_count: usize = 0;
            let mut poly_count: usize = 0;
            if let Some(markets) = v.get("markets").and_then(|m| m.as_array()) {
                // Stats
                for mkt in markets.iter() {
                    total_vol_usd += mkt.get("volume_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    match mkt.get("source").and_then(|s| s.as_str()) {
                        Some("kalshi") => kalshi_count += 1,
                        Some("polymarket") => poly_count += 1,
                        _ => {}
                    }
                }
                let display: Vec<&serde_json::Value> = markets.iter().collect();
                for mkt in display.iter().take(10) {
                    let title = mkt.get("title").and_then(|t| t.as_str()).unwrap_or("?");
                    let title_short: String = title.chars().take(120).collect();
                    let prob = mkt.get("probability_yes").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    let vol = mkt.get("volume_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let src = mkt.get("source").and_then(|s| s.as_str()).unwrap_or("?");
                    // market_id is required for citation hygiene rule #2 — every prediction
                    // market quote in a synthesis must be re-pullable. The underlying data
                    // calls this field `ticker` (numeric for Polymarket, alphanumeric for
                    // Kalshi); we surface it as `id:` for re-pull lookups.
                    let market_id = mkt.get("ticker")
                        .or_else(|| mkt.get("market_id"))
                        .and_then(|m| m.as_str())
                        .map(|s| {
                            let trimmed: String = s.chars().take(40).collect();
                            format!(" id:{}", trimmed)
                        })
                        .unwrap_or_default();
                    let delta_str = mkt.get("change_since")
                        .and_then(|d| {
                            d.get("prob_delta_pp")
                                .or_else(|| d.get("probability_delta_pct_points"))
                        })
                        .and_then(|d| d.as_f64())
                        .map(|d| format!(" d:{:+.1}pp", d))
                        .unwrap_or_default();
                    lines.push(format!(
                        "{}|{:.0}%|${:.0}K_USD|{}{}{}",
                        title_short, prob * 100.0, vol / 1000.0, src, market_id, delta_str
                    ));
                }
            }
            let query = v.get("query").and_then(|q| q.as_str()).unwrap_or("?");
            let total = v
                .get("total_markets")
                .or_else(|| v.get("total_matches"))
                .and_then(|t| t.as_u64())
                .unwrap_or(0);
            // Anchor the per-market delta to the absolute baseline timestamp (the prior
            // snapshot it is measured against), not the current catalog time.
            let sync_age = v.get("delta_context")
                .and_then(|dc| dc.get("baseline_as_of"))
                .and_then(|s| s.as_str())
                .map(|s| format!(",\"change_baseline_as_of\":\"{}\"", &s[..16.min(s.len())]))
                .unwrap_or_default();
            format!(
                "\"query\":\"{}\",\"total_markets\":{},\"total_vol_usd\":{:.0},\"sources\":\"kalshi:{} poly:{}\"{},\"_schema\":\".markets[].{{title,probability_yes,volume_usd,source,ticker,event_ticker}}\",\"top\":[{}]",
                query, total, total_vol_usd, kalshi_count, poly_count, sync_age,
                lines.iter()
                    .map(|l| format!("\"{}\"", l.replace('"', "'")))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        "finance_options" => {
            // Extract: ticker, price, P/C ratio, max pain, IV, and multi-expiry
            // aggregates from the current nested schema. Keep old top-level keys
            // as fallback for compatibility with older saved artifacts.
            let ticker = v.get("ticker").and_then(|t| t.as_str()).unwrap_or("?");
            let price = v.get("underlying_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
            let metrics = v.get("metrics");
            let multi = v.get("multi_expiry_summary");

            let pc_vol = metrics
                .and_then(|m| m.get("put_call_ratio_volume"))
                .and_then(|p| p.as_f64())
                .or_else(|| {
                    multi
                        .and_then(|m| m.get("weighted_put_call_ratio"))
                        .and_then(|p| p.as_f64())
                })
                .or_else(|| v.get("put_call_ratio_volume").and_then(|p| p.as_f64()));
            let pc_oi = metrics
                .and_then(|m| m.get("put_call_ratio_oi"))
                .and_then(|p| p.as_f64())
                .or_else(|| v.get("put_call_ratio_oi").and_then(|p| p.as_f64()));
            let max_pain = metrics
                .and_then(|m| m.get("max_pain"))
                .and_then(|p| p.as_f64())
                .or_else(|| v.get("max_pain").and_then(|p| p.as_f64()));
            let iv = v
                .get("atm_iv")
                .and_then(|i| i.as_f64())
                .or_else(|| metrics.and_then(|m| m.get("atm_iv")).and_then(|i| i.as_f64()))
                .or_else(|| v.get("implied_volatility").and_then(|i| i.as_f64()))
                .or_else(|| v.get("iv").and_then(|i| i.as_f64()));
            let total_oi = metrics
                .and_then(|m| {
                    Some(
                        m.get("total_call_oi")?.as_u64()?
                            + m.get("total_put_oi")?.as_u64()?,
                    )
                })
                .or_else(|| multi.and_then(|m| m.get("aggregate_oi")).and_then(|o| o.as_u64()))
                .or_else(|| v.get("total_open_interest").and_then(|o| o.as_u64()));
            let total_vol = metrics
                .and_then(|m| {
                    Some(
                        m.get("total_call_volume")?.as_u64()?
                            + m.get("total_put_volume")?.as_u64()?,
                    )
                })
                .or_else(|| {
                    multi
                        .and_then(|m| m.get("aggregate_volume"))
                        .and_then(|o| o.as_u64())
                })
                .or_else(|| v.get("total_volume").and_then(|o| o.as_u64()));

            // Expiration dates available
            let exp_count = v.get("expirations").and_then(|e| e.as_array()).map(|a| a.len())
                .or_else(|| {
                    multi
                        .and_then(|m| m.get("snapshots"))
                        .and_then(|s| s.as_array())
                        .map(|a| a.len())
                })
                .or_else(|| v.get("chains").and_then(|c| c.as_array()).map(|a| a.len()))
                .unwrap_or(0);

            let selected_exp = v.get("selected_expiry").and_then(|e| e.as_str());
            let selected_dte = v.get("selected_days_to_expiry").and_then(|e| e.as_i64());
            let sel_reason = v.get("selection_reason").and_then(|e| e.as_str());

            let mut parts = vec![format!("\"ticker\":\"{}\",\"price\":{:.2}", ticker, price)];
            if let Some(exp) = selected_exp { parts.push(format!("\"selected_expiry\":\"{}\"", exp)); }
            if let Some(dte) = selected_dte { parts.push(format!("\"selected_dte\":{}", dte)); }
            if let Some(reason) = sel_reason { parts.push(format!("\"selection_reason\":\"{}\"", reason)); }
            if let Some(r) = pc_vol { parts.push(format!("\"pc_vol_ratio\":{:.2}", r)); }
            if let Some(r) = pc_oi { parts.push(format!("\"pc_oi_ratio\":{:.2}", r)); }
            if let Some(mp) = max_pain {
                parts.push(format!("\"max_pain\":{:.2}", mp));
                parts.push(format!("\"max_pain_gap\":{:+.2}", mp - price));
            }
            if let Some(i) = iv { parts.push(format!("\"iv_pct\":{:.1}", i * 100.0)); }
            if let Some(oi) = total_oi { parts.push(format!("\"total_oi\":{}", oi)); }
            if let Some(vol) = total_vol { parts.push(format!("\"total_vol\":{}", vol)); }
            if let Some(snap_count) = multi
                .and_then(|m| m.get("snapshots"))
                .and_then(|s| s.as_array())
                .map(|a| a.len())
            {
                parts.push(format!("\"multi_expiry_snapshots\":{}", snap_count));
            }
            if let Some(nearest_monthly_pain) = multi
                .and_then(|m| m.get("max_pain_range"))
                .and_then(|r| r.get("nearest_monthly_pain"))
                .and_then(|p| p.as_f64())
            {
                parts.push(format!("\"nearest_monthly_pain\":{:.2}", nearest_monthly_pain));
            }
            if exp_count > 0 { parts.push(format!("\"exp_dates\":{}", exp_count)); }
            parts.push("\"_schema\":\".{metrics,calls[],puts[],multi_expiry_summary.snapshots[]}\"".to_string());
            parts.join(",")
        }
        "finance_timeseries" => {
            match mcp_timeseries_summary(output) {
                Some(summary) => format!(
                    "\"_schema\":\".series[].{{ticker,candles[].{{t,o,h,l,c,v}}}}\",\"_summary\":{}",
                    summary
                ),
                None => format!("\"_summary\":null"),
            }
        }
        "finance_movers" => {
            let mut lines = Vec::new();
            if let Some(movers) = v.get("movers").and_then(|m| m.as_array()) {
                for mover in movers.iter().take(15) {
                    let ticker = mover.get("ticker").and_then(|v| v.as_str()).unwrap_or("?");
                    let pct = mover.get("change_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let price = mover.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let cap = mover.get("market_cap").and_then(|v| v.as_u64()).unwrap_or(0);
                    let value_change = mover
                        .get("estimated_value_change")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let src = mover.get("source").and_then(|v| v.as_str()).unwrap_or("?");
                    let cap_str = if cap >= 1_000_000_000_000 {
                        format!("${:.1}T", cap as f64 / 1e12)
                    } else if cap >= 1_000_000_000 {
                        format!("${:.0}B", cap as f64 / 1e9)
                    } else if cap >= 1_000_000 {
                        format!("${:.0}M", cap as f64 / 1e6)
                    } else {
                        "?".to_string()
                    };
                    let value_str = if value_change.abs() >= 1e12 {
                        format!("{:+.1}T", value_change / 1e12)
                    } else if value_change.abs() >= 1e9 {
                        format!("{:+.1}B", value_change / 1e9)
                    } else if value_change.abs() >= 1e6 {
                        format!("{:+.0}M", value_change / 1e6)
                    } else {
                        "?".to_string()
                    };
                    lines.push(format!(
                        "{}|{:+.1}%|${:.2}|cap:{}|value:{}|{}",
                        ticker, pct, price, cap_str, value_str, src
                    ));
                }
            }
            let provider = v.get("provider").and_then(|p| p.as_str()).unwrap_or("?");
            let universe = v.get("universe").and_then(|p| p.as_str()).unwrap_or("?");
            let direction = v.get("direction").and_then(|p| p.as_str()).unwrap_or("?");
            let sort_by = v.get("sort_by").and_then(|p| p.as_str()).unwrap_or("?");
            let returned = v.get("returned").and_then(|n| n.as_u64()).unwrap_or(lines.len() as u64);
            let candidate_count = v.get("candidate_count").and_then(|n| n.as_u64()).unwrap_or(0);
            format!(
                "\"provider\":\"{}\",\"universe\":\"{}\",\"direction\":\"{}\",\"sort_by\":\"{}\",\"candidate_count\":{},\"returned\":{},\"_schema\":\".movers[].{{ticker,name,price,change_pct,market_cap,volume,dollar_volume,estimated_value_change,source}}\",\"top\":[{}]",
                provider,
                universe,
                direction,
                sort_by,
                candidate_count,
                returned,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_cot" => {
            // Group all weeks by contract, compute raw historical percentile + data age (no labels)
            let mut contract_weeks: std::collections::HashMap<String, Vec<(String, i64, i64, f64)>> =
                std::collections::HashMap::new();
            if let Some(positions) = v.get("positions").and_then(|p| p.as_array()) {
                for pos in positions {
                    let contract = pos.get("contract_name").and_then(|c| c.as_str()).unwrap_or("?").to_string();
                    let date = pos.get("report_date").and_then(|d| d.as_str()).unwrap_or("").to_string();
                    let net = pos.get("spec_net").and_then(|s| s.as_i64()).unwrap_or(0);
                    let chg = pos.get("spec_net_change").and_then(|c| c.as_i64()).unwrap_or(0);
                    let pct = pos.get("spec_net_pct_oi").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    contract_weeks.entry(contract).or_default().push((date, net, chg, pct));
                }
            }

            let gen_date = v.get("generated_at").and_then(|g| g.as_str())
                .and_then(|g| if g.len() >= 10 { Some(g[..10].to_string()) } else { None });

            let mut lines = Vec::new();
            let mut contract_list: Vec<String> = contract_weeks.keys().cloned().collect();
            contract_list.sort();

            for contract in &contract_list {
                let weeks = contract_weeks.get(contract).unwrap();
                let mut sorted = weeks.clone();
                sorted.sort_by(|a, b| b.0.cmp(&a.0)); // desc by date → latest first

                let (latest_date, latest_net, latest_chg, latest_pct) = &sorted[0];
                let all_nets: Vec<i64> = sorted.iter().map(|w| w.1).collect();
                let n_weeks = all_nets.len();

                // Historical percentile: where does current net sit across all returned weeks.
                // Raw rank only — no categorical labels (NET_LONG/NET_SHORT/COVERING/etc removed).
                let rank = all_nets.iter().filter(|&&n| n <= *latest_net).count();
                let pctile = if n_weeks > 1 { (rank * 100) / n_weeks } else { 50usize };

                // Data age
                let age_str = if latest_date.len() >= 10 {
                    if let Some(ref gd) = gen_date {
                        use chrono::NaiveDate;
                        if let (Ok(rd), Ok(gd)) = (
                            NaiveDate::parse_from_str(&latest_date[..10], "%Y-%m-%d"),
                            NaiveDate::parse_from_str(gd, "%Y-%m-%d"),
                        ) {
                            let days = (gd - rd).num_days();
                            if days > 0 { format!(" as_of:{} ({}d)", &latest_date[..10], days) } else { format!(" as_of:{}", &latest_date[..10]) }
                        } else { format!(" as_of:{}", &latest_date[..10]) }
                    } else { format!(" as_of:{}", &latest_date[..10]) }
                } else { String::new() };

                let name_short: String = contract.chars().take(35).collect();
                lines.push(format!(
                    "{}|net:{:+}|chg:{:+}|pctile:{}/{}w|{:.1}%OI{}",
                    name_short, latest_net, latest_chg, pctile, n_weeks, latest_pct, age_str
                ));
            }

            let report_type = v.get("report_type").and_then(|r| r.as_str()).unwrap_or("?");
            // Surface the staleness fields that already exist in the full COT output but
            // were missing from the summary — COT is released weekly (Fri for prior Tue),
            // so a summary with no freshness signal can be read as current when it is 10d+ old.
            let data_as_of = v.get("data_as_of").and_then(|d| d.as_str()).unwrap_or("");
            let staleness_days = v.get("staleness_days").and_then(|s| s.as_i64());
            let next_release = v.get("next_release").and_then(|n| n.as_str()).unwrap_or("");
            format!(
                "\"report\":\"{}\",\"data_as_of\":\"{}\",\"staleness_days\":{},\"next_release\":\"{}\",\"contracts\":{},\"_schema\":\".positions[].{{contract_name,report_date,spec_net,spec_net_change,spec_net_pct_oi,comm_net}}\",\"latest\":[{}]",
                report_type,
                data_as_of.replace('"', "'"),
                staleness_days.map(|n| n.to_string()).unwrap_or_else(|| "null".to_string()),
                next_release.replace('"', "'"),
                contract_list.len(),
                lines.iter()
                    .map(|l| format!("\"{}\"", l.replace('"', "'")))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        "finance_rate_path" => {
            let rate = v.get("current_rate").and_then(|r| r.as_f64()).unwrap_or(0.0);
            let rate_basis = v
                .get("current_rate_basis")
                .and_then(|r| r.as_str())
                .unwrap_or("?");
            let target_lower = v
                .get("current_rates")
                .and_then(|r| r.get("target_lower_bound"))
                .and_then(|r| r.as_f64());
            let target_upper = v
                .get("current_rates")
                .and_then(|r| r.get("target_upper_bound"))
                .and_then(|r| r.as_f64());
            let effective_rate = v
                .get("current_rates")
                .and_then(|r| r.get("effective_rate"))
                .and_then(|r| r.as_f64());
            let mut lines = Vec::new();
            let mut first_50pct_cut: Option<String> = None;
            if let Some(meetings) = v.get("meetings").and_then(|m| m.as_array()) {
                for mtg in meetings {
                    let date = mtg.get("date").and_then(|d| d.as_str()).unwrap_or("?");
                    // Use Option to distinguish missing data from 0% probability
                    let hold = mtg.get("hold_prob").and_then(|p| p.as_f64());
                    let cut  = mtg.get("cut_prob").and_then(|p| p.as_f64());
                    let hike = mtg.get("hike_prob").and_then(|p| p.as_f64());
                    let fmt_prob = |p: Option<f64>| p.map(|v| format!("{:.0}%", v * 100.0)).unwrap_or_else(|| "?".to_string());
                    if date.starts_with("2026") && cut.map_or(false, |c| c > 0.50) && first_50pct_cut.is_none() {
                        first_50pct_cut = Some(date[..7.min(date.len())].to_string());
                    }
                    let label = &date[..7.min(date.len())];
                    lines.push(format!("{}:H{}/C{}/K{}", label, fmt_prob(hold), fmt_prob(cut), fmt_prob(hike)));
                }
            }
            let first_cut_str = first_50pct_cut
                .map(|m| format!(",\"first_50pct_cut_month\":\"{}\"", m))
                .unwrap_or_default();
            let target_range_str = match (target_lower, target_upper) {
                (Some(lower), Some(upper)) => {
                    format!(",\"target_range\":\"{lower:.2}-{upper:.2}\"")
                }
                _ => String::new(),
            };
            let effective_rate_str = effective_rate
                .map(|value| format!(",\"effective_rate\":{value:.2}"))
                .unwrap_or_default();
            // Compact year_view summary: top cut count + top EOY rate.
            let year_view_str = v
                .get("year_view")
                .map(|yv| {
                    let top_cuts = yv.get("cuts_distribution")
                        .and_then(|d| d.as_object())
                        .and_then(|o| o.iter().max_by(|a, b| {
                            let pa = a.1.as_f64().unwrap_or(0.0);
                            let pb = b.1.as_f64().unwrap_or(0.0);
                            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                        }))
                        .map(|(k, v)| format!("{}={:.0}%", k, v.as_f64().unwrap_or(0.0) * 100.0))
                        .unwrap_or_default();
                    let top_eoy = yv.get("eoy_rate_distribution")
                        .and_then(|d| d.as_object())
                        .and_then(|o| o.iter().max_by(|a, b| {
                            let pa = a.1.as_f64().unwrap_or(0.0);
                            let pb = b.1.as_f64().unwrap_or(0.0);
                            pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
                        }))
                        .map(|(k, v)| format!("{}={:.0}%", k, v.as_f64().unwrap_or(0.0) * 100.0))
                        .unwrap_or_default();
                    let year = yv.get("year").and_then(|y| y.as_i64()).unwrap_or(0);
                    format!(",\"year_view\":\"y={} top_cuts:{} top_eoy:{}\"", year, top_cuts, top_eoy)
                })
                .unwrap_or_default();
            let compound_str = v
                .get("compound_paths")
                .and_then(|cp| cp.as_array())
                .filter(|arr| !arr.is_empty())
                .map(|arr| format!(",\"n_compound_paths\":{}", arr.len()))
                .unwrap_or_default();
            format!(
                "\"current_rate\":{:.2},\"current_rate_basis\":\"{}\"{}{}{}{}{},\"_note\":\"H/C/K are independently priced prediction markets — may not sum to 100%\",\"_schema\":\".meetings[].{{date,hold_prob,cut_prob,hike_prob,cut_25bp_prob,cut_50bp_plus_prob,volume,n_markets}};.year_view;.compound_paths[]\",\"meetings\":[{}]",
                rate, rate_basis, target_range_str, effective_rate_str, first_cut_str, year_view_str, compound_str,
                lines.iter().map(|l| format!("\"{}\"", l)).collect::<Vec<_>>().join(",")
            )
        }
        "finance_schedule" => {
            let mut earnings_lines = Vec::new();
            let mut macro_lines = Vec::new();
            let total_earnings;
            if let Some(earnings) = v.get("earnings").and_then(|e| e.as_array()) {
                total_earnings = earnings.len();
                // market_cap is now a typed u64 in the JSON. Fall back to string
                // parsing only as belt-and-suspenders for any cached payloads
                // produced before the schema cleanup.
                let read_mcap = |val: &serde_json::Value| -> f64 {
                    match val.get("market_cap") {
                        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
                        Some(serde_json::Value::String(s)) => s
                            .chars()
                            .filter(|c| c.is_ascii_digit())
                            .collect::<String>()
                            .parse::<f64>()
                            .unwrap_or(0.0),
                        _ => 0.0,
                    }
                };
                // Sort by market cap descending, show top 30
                let mut sorted: Vec<&serde_json::Value> = earnings.iter().collect();
                sorted.sort_by(|a, b| {
                    let ma = read_mcap(a);
                    let mb = read_mcap(b);
                    mb.partial_cmp(&ma).unwrap_or(std::cmp::Ordering::Equal)
                });
                for e in sorted.iter().take(30) {
                    let sym = e.get("symbol").and_then(|s| s.as_str()).unwrap_or("?");
                    let date = e.get("date").and_then(|d| d.as_str()).unwrap_or("?");
                    let time = e.get("time").and_then(|t| t.as_str()).unwrap_or("?");
                    // eps_forecast is now a typed f64. Fall back to string only for
                    // legacy/cached payloads.
                    let eps = match e.get("eps_forecast") {
                        Some(serde_json::Value::Number(n)) => n
                            .as_f64()
                            .map(|v| format!("{v:.2}"))
                            .unwrap_or_else(|| "?".to_string()),
                        Some(serde_json::Value::String(s)) => s.clone(),
                        _ => "?".to_string(),
                    };
                    let mcap_val = read_mcap(e);
                    let mcap_str = if mcap_val >= 1e12 { format!("${:.1}T", mcap_val / 1e12) }
                        else if mcap_val >= 1e9 { format!("${:.0}B", mcap_val / 1e9) }
                        else { String::new() };
                    earnings_lines.push(format!("{}|{}|{}|eps:{} {}", sym, date, time, eps, mcap_str));
                }
            } else {
                total_earnings = 0;
            }
            if let Some(macro_evts) = v.get("macro").and_then(|m| m.as_array()) {
                for evt in macro_evts.iter().take(10) {
                    let name = evt.get("title").and_then(|n| n.as_str()).unwrap_or("?");
                    let date = evt.get("date").and_then(|d| d.as_str()).unwrap_or("?");
                    let name_short: String = name.chars().take(40).collect();
                    macro_lines.push(format!("{}|{}", name_short, date));
                }
            }
            // Stats: pre/after-hours breakdown
            let mut pre_count = 0usize;
            let mut after_count = 0usize;
            let mut by_date: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
            if let Some(earnings) = v.get("earnings").and_then(|e| e.as_array()) {
                for e in earnings {
                    let time = e.get("time").and_then(|t| t.as_str()).unwrap_or("");
                    if time.contains("pre") { pre_count += 1; }
                    else if time.contains("after") { after_count += 1; }
                    let date = e.get("date").and_then(|d| d.as_str()).unwrap_or("?");
                    *by_date.entry(date[..10.min(date.len())].to_string()).or_insert(0) += 1;
                }
            }
            let date_dist: Vec<String> = by_date.iter().map(|(d, n)| format!("{}:{}", d, n)).collect();
            format!(
                "\"earnings_total\":{},\"showing_top\":30,\"pre_market\":{},\"after_hours\":{},\"by_date\":\"{}\",\"macro_count\":{},\"_schema\":\".earnings[].{{symbol,date,time,market_cap,eps_forecast,last_year_eps,fiscal_quarter_ending}}\",\"earnings\":[{}],\"macro\":[{}]",
                total_earnings, pre_count, after_count, date_dist.join(" "),
                macro_lines.len(),
                earnings_lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(","),
                macro_lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(","),
            )
        }
        "finance_auctions" => {
            let mut lines = Vec::new();
            if let Some(auctions) = v.get("auctions").and_then(|a| a.as_array()) {
                for auction in auctions.iter().take(15) {
                    let sec_type = auction.get("security_type").and_then(|t| t.as_str()).unwrap_or("?");
                    let term = auction.get("security_term").and_then(|t| t.as_str()).unwrap_or("?");
                    let btc = auction.get("bid_to_cover_ratio").and_then(|b| b.as_f64()).unwrap_or(0.0);
                    let accepted = auction.get("total_accepted").and_then(|a| a.as_f64()).unwrap_or(0.0);
                    let indirect = auction.get("indirect_bidder_pct").and_then(|i| i.as_f64()).unwrap_or(0.0);
                    let date = auction.get("auction_date").and_then(|d| d.as_str()).unwrap_or("?");
                    let accepted_b = accepted / 1e9;
                    // high_yield: present for notes/bonds, null for bills (discount securities)
                    let yield_str = auction.get("high_yield").and_then(|y| y.as_f64())
                        .map(|y| format!("|y:{:.3}%", y))
                        .unwrap_or_default();
                    lines.push(format!("{} {}|BTC:{:.2}|${:.0}B|ind:{:.0}%{}|{}", sec_type, term, btc, accepted_b, indirect, yield_str, date));
                }
            }
            let total = v.get("count").and_then(|c| c.as_u64()).unwrap_or(lines.len() as u64);
            format!(
                "\"count\":{},\"_schema\":\".auctions[].{{security_type,security_term,auction_date,bid_to_cover_ratio,high_yield,total_accepted,indirect_bidder_pct,direct_bidder_pct}}\",\"auctions\":[{}]",
                total,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_ecb" => {
            // ECB: series[].{label, observations[].{period, value}}
            // Show latest value per series, compact
            let preset = v.get("preset").and_then(|p| p.as_str()).unwrap_or("custom");
            let mut lines = Vec::new();
            if let Some(series) = v.get("series").and_then(|s| s.as_array()) {
                for s in series {
                    let label = s.get("label").and_then(|l| l.as_str()).unwrap_or("?");
                    let label_short: String = label.chars().take(30).collect();
                    let unit = s.get("unit").and_then(|u| u.as_str());
                    if let Some(obs) = s.get("observations").and_then(|o| o.as_array()).and_then(|a| a.last()) {
                        let period = obs.get("period").and_then(|p| p.as_str()).unwrap_or("?");
                        let val = obs.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let val_str = match unit {
                            Some(u) if u.contains("percent") || u.contains("pct") => format!("{:.2}%", val),
                            Some(u) if u.contains("EUR") && val > 1e9 => format!("{:.1}B EUR", val / 1e9),
                            _ if val > 1e12 => format!("{:.1}T", val / 1e12),
                            _ if val > 1e9 => format!("{:.1}B", val / 1e9),
                            _ => format!("{:.4}", val),
                        };
                        lines.push(format!("{}:{}({})", label_short, val_str, period));
                    }
                }
            }
            let n = v.get("series").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
            format!(
                "\"preset\":\"{}\",\"n_series\":{},\"_schema\":\".series[].{{label,key,dataset,unit,observations[].{{period,value}}}}\",\"latest\":[{}]",
                preset, n,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_curve" => {
            // Futures forward curve: contracts[].{contract, price, change_from_front_pct}
            let commodity = v.get("commodity").and_then(|c| c.as_str()).unwrap_or("?");
            let front = v.get("front_month_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
            let back = v.get("back_month_price").and_then(|p| p.as_f64()).unwrap_or(0.0);
            let spread_pct = v.get("spread_pct").and_then(|s| s.as_f64()).unwrap_or(0.0);
            let unit = v.get("unit").and_then(|u| u.as_str()).unwrap_or("");
            let mut contracts_str = Vec::new();
            if let Some(contracts) = v.get("contracts").and_then(|c| c.as_array()) {
                for c in contracts {
                    let month = c.get("contract").and_then(|m| m.as_str()).unwrap_or("?");
                    let price = c.get("price").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    let chg = c.get("change_from_front_pct").and_then(|p| p.as_f64()).unwrap_or(0.0);
                    contracts_str.push(format!("{}:{:.2}({:+.1}%)", month, price, chg));
                }
            }
            format!(
                "\"commodity\":\"{}\",\"unit\":\"{}\",\"front\":{:.2},\"back\":{:.2},\"spread_pct\":{:.1},\"contracts\":[{}]",
                commodity, unit, front, back, spread_pct,
                contracts_str.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(",")
            )
        }
        "finance_boe" => {
            // BOE: series[].{code, label, observations[].{period, value}}
            let preset = v.get("preset").and_then(|p| p.as_str()).unwrap_or("custom");
            let mut lines = Vec::new();
            if let Some(series) = v.get("series").and_then(|s| s.as_array()) {
                for s in series {
                    let label = s.get("label").and_then(|l| l.as_str()).unwrap_or("?");
                    let label_short: String = label.chars().take(25).collect();
                    if let Some(obs) = s.get("observations").and_then(|o| o.as_array()).and_then(|a| a.last()) {
                        let date = obs.get("period").and_then(|d| d.as_str()).unwrap_or("?");
                        let val = obs.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        // BOE series: rates in %, FX as levels, M4 in millions
                        let val_str = if label.contains("Rate") || label.contains("Yield") || label.contains("SONIA") {
                            format!("{:.2}%", val)
                        } else if label.contains("GBP/") {
                            format!("{:.4}", val)
                        } else if val > 1e6 {
                            format!("{:.0}M", val / 1e6)
                        } else {
                            format!("{:.2}", val)
                        };
                        lines.push(format!("{}:{}({})", label_short, val_str, date));
                    }
                }
            }
            let n = v.get("series").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
            format!(
                "\"preset\":\"{}\",\"n_series\":{},\"_schema\":\".series[].{{code,label,observations[].{{date,value}}}}\",\"latest\":[{}]",
                preset, n,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_boj" => {
            // BOJ: series[].{code, name, unit, observations[].{period, value}}
            let preset = v.get("preset").and_then(|p| p.as_str()).unwrap_or("custom");
            let mut lines = Vec::new();
            if let Some(series) = v.get("series").and_then(|s| s.as_array()) {
                for s in series {
                    let name = s.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    let name_short: String = name.chars().take(30).collect();
                    let unit = s.get("unit").and_then(|u| u.as_str()).unwrap_or("");
                    if let Some(obs) = s.get("observations").and_then(|o| o.as_array()).and_then(|a| a.last()) {
                        let period = obs.get("period").and_then(|p| p.as_str()).unwrap_or("?");
                        let val = obs.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let val_str = if unit.contains("%") || unit.to_lowercase().contains("percent") {
                            format!("{:.2}%", val)
                        } else if unit.contains("100mil") || unit.contains("億") {
                            format!("{:.0}(100M¥)", val)
                        } else if val.abs() > 1e6 {
                            format!("{:.1}M", val / 1e6)
                        } else {
                            format!("{:.2}", val)
                        };
                        lines.push(format!("{}:{}({})", name_short, val_str, period));
                    }
                }
            }
            let n = v.get("series").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
            format!(
                "\"preset\":\"{}\",\"n_series\":{},\"_schema\":\".series[].{{code,name,unit,frequency,observations[].{{period,value}}}}\",\"latest\":[{}]",
                preset, n,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_bis" => {
            // BIS: series[].{label, ref_area, unit, observations[].{period, value}}
            let dataset = v.get("dataset").and_then(|d| d.as_str()).unwrap_or("?");
            let mut lines = Vec::new();
            if let Some(series) = v.get("series").and_then(|s| s.as_array()) {
                for s in series {
                    let label = s.get("label").and_then(|l| l.as_str()).unwrap_or("?");
                    let label_short: String = label.chars().take(30).collect();
                    let ref_area = s.get("ref_area").and_then(|r| r.as_str()).unwrap_or("?");
                    let unit = s.get("unit").and_then(|u| u.as_str());
                    if let Some(obs) = s.get("observations").and_then(|o| o.as_array()).and_then(|a| a.last()) {
                        let period = obs.get("period").and_then(|p| p.as_str()).unwrap_or("?");
                        let val = obs.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let val_str = match unit {
                            Some(u) if u.contains("percent") || u.contains("pct") => format!("{:.2}%", val),
                            _ if val > 1e9 => format!("{:.1}B", val / 1e9),
                            _ => format!("{:.2}", val),
                        };
                        lines.push(format!("{}[{}]:{}({})", label_short, ref_area, val_str, period));
                    }
                }
            }
            let n = v.get("series").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
            format!(
                "\"dataset\":\"{}\",\"n_series\":{},\"_schema\":\".series[].{{label,key,ref_area,unit,frequency,observations[].{{period,value}}}}\",\"latest\":[{}]",
                dataset, n,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_eia" => {
            // EIA: series[].{label, observations[].{period, value, units, product_name}}
            let preset = v.get("preset").and_then(|p| p.as_str()).unwrap_or("custom");
            let mut lines = Vec::new();
            if let Some(series) = v.get("series").and_then(|s| s.as_array()) {
                for s in series {
                    let label = s.get("label").and_then(|l| l.as_str()).unwrap_or("?");
                    let label_short: String = label.chars().take(35).collect();
                    if let Some(obs_arr) = s.get("observations").and_then(|o| o.as_array()) {
                        if let Some(latest) = obs_arr.last() {
                            let period = latest.get("period").and_then(|p| p.as_str()).unwrap_or("?");
                            let val = latest.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            let units = latest.get("units").and_then(|u| u.as_str()).unwrap_or("");
                            // WoW change if at least 2 observations
                            let wow_str = if obs_arr.len() >= 2 {
                                let prev = obs_arr[obs_arr.len() - 2].get("value").and_then(|v| v.as_f64()).unwrap_or(val);
                                let chg = val - prev;
                                if chg.abs() > 0.01 { format!(" WoW:{:+.1}", chg) } else { String::new() }
                            } else { String::new() };
                            let val_str = if units.contains("bbl") && val > 1e6 {
                                format!("{:.1}M bbl", val / 1e6)
                            } else if units.contains("Bcf") || units.contains("bcf") {
                                format!("{:.0} Bcf", val)
                            } else if val > 1e6 {
                                format!("{:.1}M", val / 1e6)
                            } else {
                                format!("{:.1} {}", val, &units[..units.len().min(10)])
                            };
                            lines.push(format!("{}:{}{}({})", label_short, val_str, wow_str, period));
                        }
                    }
                }
            }
            let n = v.get("series").and_then(|s| s.as_array()).map(|a| a.len()).unwrap_or(0);
            format!(
                "\"preset\":\"{}\",\"n_series\":{},\"_schema\":\".series[].{{label,observations[].{{period,value,units,product_name,area_name}}}}\",\"latest\":[{}]",
                preset, n,
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "finance_fiscal" => {
            let kind = v.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
            let mut lines = Vec::new();
            match kind {
                "debt" => {
                    if let Some(items) = v.get("debt").and_then(|d| d.as_array()) {
                        for item in items.iter().take(3) {
                            let date = item.get("record_date").and_then(|d| d.as_str()).unwrap_or("?");
                            let total = item.get("total_debt_billions").and_then(|t| t.as_f64()).unwrap_or(0.0);
                            let public = item.get("public_debt_billions").and_then(|p| p.as_f64());
                            let pub_str = public.map(|p| format!(" pub:${:.1}T", p / 1e3)).unwrap_or_default();
                            lines.push(format!("{}|total:${:.2}T{}", date, total / 1e3, pub_str));
                        }
                    }
                }
                "statement" => {
                    if let Some(items) = v.get("statement").and_then(|s| s.as_array()) {
                        // Group by date, show most recent
                        for item in items.iter().take(5) {
                            let date = item.get("record_date").and_then(|d| d.as_str()).unwrap_or("?");
                            let acct = item.get("account").and_then(|a| a.as_str()).unwrap_or("?");
                            let acct_short: String = acct.chars().take(30).collect();
                            let close = item.get("close_today_bal").and_then(|c| c.as_f64());
                            let close_str = close.map(|c| format!("${:.0}M", c)).unwrap_or("?".to_string());
                            lines.push(format!("{}|{}|{}", date, acct_short, close_str));
                        }
                    }
                }
                "interest" => {
                    if let Some(items) = v.get("interest").and_then(|i| i.as_array()) {
                        let mut seen_date = String::new();
                        for item in items.iter().take(10) {
                            let date = item.get("record_date").and_then(|d| d.as_str()).unwrap_or("?");
                            let desc = item.get("security_desc").and_then(|s| s.as_str()).unwrap_or("?");
                            let desc_short: String = desc.chars().take(25).collect();
                            let rate = item.get("avg_interest_rate_pct").and_then(|r| r.as_f64()).unwrap_or(0.0);
                            if seen_date.is_empty() { seen_date = date.to_string(); }
                            if date == seen_date {
                                lines.push(format!("{}:{:.3}%", desc_short, rate));
                            }
                        }
                        if !seen_date.is_empty() {
                            lines.insert(0, format!("as_of:{}", seen_date));
                        }
                    }
                }
                _ => {}
            }
            let cadence = v.get("release_cadence").and_then(|c| c.as_str()).unwrap_or("");
            let data_as_of = v.get("data_as_of").and_then(|d| d.as_str()).unwrap_or("");
            let staleness = v.get("staleness_days").and_then(|s| s.as_i64());
            format!(
                "\"kind\":\"{}\",\"release_cadence\":\"{}\",\"data_as_of\":\"{}\",\"staleness_days\":{},\"_schema\":\".{{debt[],statement[],interest[]}}\",\"data\":[{}]",
                kind, cadence, data_as_of,
                staleness.map(|n| n.to_string()).unwrap_or_else(|| "null".to_string()),
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        // ── legal ──────────────────────────────────────────────────────────
        "legal_cite" => {
            // The verdict line matters more than the payload here: a caller
            // that only reads the summary must still see which cites failed.
            let checked = v.get("checked").and_then(|x| x.as_u64()).unwrap_or(0);
            let verified = v.get("verified").and_then(|x| x.as_u64()).unwrap_or(0);
            let unverified = v.get("unverified").and_then(|x| x.as_u64()).unwrap_or(0);
            let mut lines = Vec::new();
            if let Some(arr) = v.get("citations").and_then(|x| x.as_array()) {
                for c in arr.iter().take(25) {
                    let cite = c.get("citation").and_then(|x| x.as_str()).unwrap_or("?");
                    let exists = c.get("exists").and_then(|x| x.as_bool()).unwrap_or(false);
                    let name = c.get("case_name").and_then(|x| x.as_str()).unwrap_or("");
                    lines.push(if exists {
                        format!("OK {cite} = {name}")
                    } else {
                        format!("NOT FOUND {cite}")
                    });
                }
            }
            let cited_by_total = v
                .get("cited_by_total")
                .and_then(|x| x.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".to_string());
            format!(
                "\"checked\":{checked},\"verified\":{verified},\"unverified\":{unverified},\"cited_by_total\":{cited_by_total},\"_schema\":\".citations[].{{citation,exists,case_name,court,date_filed,url,note}}\",\"verdicts\":[{}]",
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "legal_search" => {
            let total = v
                .get("total_available")
                .and_then(|x| x.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".to_string());
            let mut lines = Vec::new();
            if let Some(arr) = v.get("results").and_then(|x| x.as_array()) {
                for r in arr.iter().take(20) {
                    let name: String = r
                        .get("case_name")
                        .and_then(|x| x.as_str())
                        .unwrap_or("?")
                        .chars()
                        .take(48)
                        .collect();
                    let court = r.get("court_id").and_then(|x| x.as_str()).unwrap_or("?");
                    let date = r.get("date_filed").and_then(|x| x.as_str()).unwrap_or("?");
                    let cites = r.get("cite_count").and_then(|x| x.as_u64()).unwrap_or(0);
                    lines.push(format!("{name}|{court}|{date}|cited:{cites}"));
                }
            }
            format!(
                "\"total_available\":{total},\"returned\":{},\"_schema\":\".results[].{{case_name,citation,court,court_id,date_filed,judge,cite_count,snippet,opinion_id,url}}\",\"hits\":[{}]",
                v.get("results").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0),
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "legal_docket" => {
            let name = v.get("case_name").and_then(|x| x.as_str()).unwrap_or("?");
            let court = v.get("court").and_then(|x| x.as_str()).unwrap_or("?");
            let number = v.get("docket_number").and_then(|x| x.as_str()).unwrap_or("?");
            let total = v.get("entry_count").and_then(|x| x.as_u64()).unwrap_or(0);
            let returned = v.get("entries").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
            let mut lines = Vec::new();
            if let Some(arr) = v.get("entries").and_then(|x| x.as_array()) {
                for e in arr.iter().take(20) {
                    let n = e.get("entry_number").and_then(|x| x.as_u64()).unwrap_or(0);
                    let date = e.get("date_filed").and_then(|x| x.as_str()).unwrap_or("?");
                    let desc: String = e
                        .get("description")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    lines.push(format!("#{n}|{date}|{desc}"));
                }
            }
            format!(
                "\"case_name\":\"{}\",\"court\":\"{court}\",\"docket_number\":\"{number}\",\"entry_count\":{total},\"entries_returned\":{returned},\"_schema\":\".entries[].{{entry_number,date_filed,description,documents[]}}\",\"docket\":[{}]",
                name.replace('"', "'"),
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "legal_fedreg" => {
            let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("search");
            let total = v
                .get("total_available")
                .and_then(|x| x.as_u64())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "null".to_string());
            let mut lines = Vec::new();
            if mode == "facet" {
                if let Some(arr) = v.get("facets").and_then(|x| x.as_array()) {
                    for f in arr.iter().take(40) {
                        let key = f.get("key").and_then(|x| x.as_str()).unwrap_or("?");
                        let count = f.get("count").and_then(|x| x.as_u64()).unwrap_or(0);
                        lines.push(format!("{key}:{count}"));
                    }
                }
            } else if let Some(arr) = v.get("documents").and_then(|x| x.as_array()) {
                for d in arr.iter().take(20) {
                    let num = d.get("document_number").and_then(|x| x.as_str()).unwrap_or("?");
                    let kind = d.get("doc_type").and_then(|x| x.as_str()).unwrap_or("?");
                    let date = d.get("publication_date").and_then(|x| x.as_str()).unwrap_or("?");
                    let close = d.get("comments_close_on").and_then(|x| x.as_str()).unwrap_or("-");
                    let title: String = d
                        .get("title")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(52)
                        .collect();
                    lines.push(format!("{num}|{kind}|{date}|comments_close:{close}|{title}"));
                }
            }
            format!(
                "\"mode\":\"{mode}\",\"total_available\":{total},\"_schema\":\".documents[].{{document_number,title,doc_type,agencies,publication_date,effective_on,comments_close_on,docket_ids,cfr_references,text}}\",\"items\":[{}]",
                lines.iter().map(|l| format!("\"{}\"", l.replace('"', "'"))).collect::<Vec<_>>().join(",")
            )
        }
        "legal_cfr" => {
            let mode = v.get("mode").and_then(|x| x.as_str()).unwrap_or("text");
            let citation = v.get("citation").and_then(|x| x.as_str()).unwrap_or("?");
            let as_of = v.get("as_of").and_then(|x| x.as_str()).unwrap_or("?");
            let extra = match mode {
                "history" => {
                    let n = v.get("amendments").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
                    // One date, many amended sections — the summary wants the
                    // distinct dates, not one line per section.
                    let mut dates: Vec<String> = v
                        .get("amendments")
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter(|x| x.get("substantive").and_then(|s| s.as_bool()).unwrap_or(true))
                                .filter_map(|x| x.get("date").and_then(|d| d.as_str()).map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    dates.dedup();
                    dates.truncate(12);
                    format!(
                        "\"amendments\":{n},\"recent_substantive_dates\":[{}]",
                        dates.iter().map(|d| format!("\"{d}\"")).collect::<Vec<_>>().join(",")
                    )
                }
                "diff" => {
                    let d = v.get("diff");
                    format!(
                        "\"changed\":{},\"lines_added\":{},\"lines_removed\":{}",
                        d.and_then(|x| x.get("changed")).and_then(|x| x.as_bool()).unwrap_or(false),
                        d.and_then(|x| x.get("lines_added")).and_then(|x| x.as_u64()).unwrap_or(0),
                        d.and_then(|x| x.get("lines_removed")).and_then(|x| x.as_u64()).unwrap_or(0)
                    )
                }
                _ => format!(
                    "\"chars\":{}",
                    v.get("chars").and_then(|x| x.as_u64()).unwrap_or(0)
                ),
            };
            format!(
                "\"mode\":\"{mode}\",\"citation\":\"{citation}\",\"as_of\":\"{as_of}\",{extra},\"_schema\":\".{{text,amendments[],diff,results[]}}\""
            )
        }
        _ => {
            // No custom summary for this tool — data is in the file, read it.
            format!("\"_note\":\"no compact summary for {tool}; the full structured result is in this response\"")
        }
    }
}

fn mcp_build_cli_args(tool: &str, args: &serde_json::Value) -> anyhow::Result<Vec<String>> {
    let s = |v: &str| v.to_string();
    match tool {
        "finance_timeseries" => {
            let mut v = vec![s("finance"), s("timeseries")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(tickers) = args
                .get("tickers")
                .or_else(|| args.get("ticker"))
                .and_then(|t| t.as_str())
            {
                v.extend([s("--tickers"), s(tickers)]);
            }
            if v.len() <= 2 {
                anyhow::bail!("either preset or tickers required");
            }
            if let Some(range) = args.get("range").and_then(|r| r.as_str()) {
                v.extend([s("--range"), s(range)]);
            }
            if let Some(granularity) = args.get("granularity").and_then(|g| g.as_str()) {
                v.extend([s("--granularity"), s(granularity)]);
            }
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(as_of) = args.get("as_of").and_then(|a| a.as_str()) {
                v.extend([s("--as-of"), s(as_of)]);
            }
            if let Some(start) = args.get("start").and_then(|a| a.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            if let Some(end) = args.get("end").and_then(|a| a.as_str()) {
                v.extend([s("--end"), s(end)]);
            }
            if let Some(max_pts) = args.get("max_points_per_ticker").and_then(|n| n.as_u64()) {
                v.extend([s("--max-points-per-ticker"), max_pts.to_string()]);
            }
            if let Some(account) = args.get("ibkr_account").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-account"), s(account)]);
            }
            if let Some(odds_market) = args.get("odds_market").and_then(|m| m.as_str()) {
                v.extend([s("--odds-market"), s(odds_market)]);
            }
            if let Some(odds_provider) = args.get("odds_provider").and_then(|p| p.as_str()) {
                v.extend([s("--odds-provider"), s(odds_provider)]);
            }
            if let Some(odds_side) = args.get("odds_side").and_then(|s_| s_.as_str()) {
                v.extend([s("--odds-side"), s(odds_side)]);
            }
            Ok(v)
        }
        "finance_movers" => {
            let mut v = vec![s("finance"), s("movers")];
            if let Some(universe) = args.get("universe").and_then(|u| u.as_str()) {
                v.extend([s("--universe"), s(universe)]);
            }
            if let Some(tickers) = args
                .get("tickers")
                .or_else(|| args.get("ticker"))
                .and_then(|t| t.as_str())
            {
                v.extend([s("--tickers"), s(tickers)]);
            }
            if let Some(direction) = args.get("direction").and_then(|d| d.as_str()) {
                v.extend([s("--direction"), s(direction)]);
            }
            if let Some(sort_by) = args.get("sort_by").and_then(|d| d.as_str()) {
                v.extend([s("--sort-by"), s(sort_by)]);
            }
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(min_cap) = args.get("min_market_cap").and_then(|m| m.as_str()) {
                v.extend([s("--min-market-cap"), s(min_cap)]);
            }
            if let Some(max_cap) = args.get("max_market_cap").and_then(|m| m.as_str()) {
                v.extend([s("--max-market-cap"), s(max_cap)]);
            }
            if let Some(min_change) = args.get("min_change_pct").and_then(|n| n.as_f64()) {
                v.extend([s("--min-change-pct"), min_change.to_string()]);
            }
            if let Some(min_price) = args.get("min_price").and_then(|n| n.as_f64()) {
                v.extend([s("--min-price"), min_price.to_string()]);
            }
            if let Some(min_volume) = args.get("min_volume").and_then(|n| n.as_u64()) {
                v.extend([s("--min-volume"), min_volume.to_string()]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            if let Some(scan_limit) = args.get("scan_limit").and_then(|n| n.as_u64()) {
                v.extend([s("--scan-limit"), scan_limit.to_string()]);
            }
            if let Some(account) = args.get("ibkr_account").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-account"), s(account)]);
            }
            if let Some(host) = args.get("ibkr_host").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-host"), s(host)]);
            }
            if let Some(port) = args.get("ibkr_port").and_then(|n| n.as_u64()) {
                v.extend([s("--ibkr-port"), port.to_string()]);
            }
            if let Some(client_id) = args.get("ibkr_client_id").and_then(|n| n.as_i64()) {
                v.extend([s("--ibkr-client-id"), client_id.to_string()]);
            }
            if let Some(data_type) = args.get("ibkr_market_data_type").and_then(|n| n.as_i64()) {
                v.extend([s("--ibkr-market-data-type"), data_type.to_string()]);
            }
            if let Some(timeout_secs) = args.get("ibkr_timeout_secs").and_then(|n| n.as_u64()) {
                v.extend([s("--ibkr-timeout-secs"), timeout_secs.to_string()]);
            }
            if let Some(b) = args.get("include_extended_hours").and_then(|v| v.as_bool()) {
                if b { v.push(s("--include-extended-hours")); }
            }
            Ok(v)
        }
        "finance_rate_path" => {
            // source_mode argument is accepted for backwards compatibility but
            // ignored (no-op flag). Don't propagate it to the CLI.
            Ok(vec![s("finance"), s("rate-path")])
        }
        "finance_odds" => {
            let mut v = vec![s("finance"), s("odds")];
            // List flags accept the call without requiring --search; otherwise --search
            // is required to avoid returning the entire Kalshi catalog.
            let list_series = args.get("list_series").and_then(|b| b.as_bool()).unwrap_or(false);
            let list_events = args.get("list_events").and_then(|b| b.as_bool()).unwrap_or(false);
            let list_markets = args.get("list_markets").and_then(|b| b.as_bool()).unwrap_or(false);
            let list_tags = args.get("list_tags").and_then(|b| b.as_bool()).unwrap_or(false);
            let any_list = list_series || list_events || list_markets || list_tags;
            let search = args.get("search").and_then(|s| s.as_str());
            if !any_list && search.is_none() {
                anyhow::bail!("search required (or set list_series/list_events/list_markets/list_tags)");
            }
            if let Some(q) = search {
                v.extend([s("--search"), s(q)]);
            }
            // Default --live=true only when searching; list endpoints don't need it.
            if !any_list {
                let live = args.get("live").and_then(|l| l.as_bool()).unwrap_or(true);
                if live {
                    v.push(s("--live"));
                } else {
                    v.push(s("--local"));
                }
            } else if args.get("live").and_then(|l| l.as_bool()).unwrap_or(false) {
                v.push(s("--live"));
            }
            if list_series { v.push(s("--list-series")); }
            if list_events { v.push(s("--list-events")); }
            if list_markets { v.push(s("--list-markets")); }
            if list_tags { v.push(s("--list-tags")); }
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(series) = args.get("series").and_then(|p| p.as_str()) {
                v.extend([s("--series"), s(series)]);
            }
            if let Some(event) = args.get("event").and_then(|p| p.as_str()) {
                v.extend([s("--event"), s(event)]);
            }
            if let Some(market) = args.get("market").and_then(|p| p.as_str()) {
                v.extend([s("--market"), s(market)]);
            }
            if let Some(min_vol) = args.get("min_volume").and_then(|n| n.as_u64()) {
                v.extend([s("--min-volume"), min_vol.to_string()]);
            }
            if let Some(top) = args.get("top").and_then(|n| n.as_u64()) {
                v.extend([s("--top"), top.to_string()]);
            }
            if let Some(sort_by) = args.get("sort_by").and_then(|p| p.as_str()) {
                v.extend([s("--sort-by"), s(sort_by)]);
            }
            if let Some(category) = args.get("category").and_then(|p| p.as_str()) {
                v.extend([s("--category"), s(category)]);
            }
            if let Some(profile) = args.get("profile").and_then(|p| p.as_str()) {
                v.extend([s("--profile"), s(profile)]);
            }
            if let Some(country) = args.get("country").and_then(|p| p.as_str()) {
                v.extend([s("--country"), s(country)]);
            }
            if let Some(cursor) = args.get("cursor").and_then(|p| p.as_str()) {
                v.extend([s("--cursor"), s(cursor)]);
            }
            if args
                .get("deltas_only")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--deltas-only"));
            }
            if args
                .get("explain")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--explain"));
            }
            if args
                .get("include_mentions")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--include-mentions"));
            }
            if let Some(max_pages) = args.get("max_pages").and_then(|n| n.as_u64()) {
                v.extend([s("--max-pages"), max_pages.to_string()]);
            }
            if let Some(min_delta_pp) = args.get("min_delta_pp").and_then(|n| n.as_f64()) {
                v.extend([s("--min-delta-pp"), min_delta_pp.to_string()]);
            }
            if let Some(status) = args.get("status").and_then(|p| p.as_str()) {
                v.extend([s("--status"), s(status)]);
            }
            // Polymarket orderbook depth pass-through.
            if args
                .get("orderbook")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--orderbook"));
            }
            if let Some(d) = args.get("depth").and_then(|n| n.as_u64()) {
                v.extend([s("--depth"), d.to_string()]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            Ok(v)
        }
        "finance_options" => {
            let ticker = args
                .get("ticker")
                .and_then(|t| t.as_str())
                .ok_or_else(|| anyhow::anyhow!("ticker required"))?;
            let mut v = vec![s("finance"), s("options"), s("--ticker"), s(ticker)];
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(account) = args.get("ibkr_account").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-account"), s(account)]);
            }
            if let Some(host) = args.get("ibkr_host").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-host"), s(host)]);
            }
            if let Some(port) = args.get("ibkr_port").and_then(|a| a.as_u64()) {
                v.extend([s("--ibkr-port"), port.to_string()]);
            }
            if let Some(client_id) = args.get("ibkr_client_id").and_then(|a| a.as_i64()) {
                v.extend([s("--ibkr-client-id"), client_id.to_string()]);
            }
            if let Some(market_data_type) =
                args.get("ibkr_market_data_type").and_then(|a| a.as_i64())
            {
                v.extend([s("--ibkr-market-data-type"), market_data_type.to_string()]);
            }
            if let Some(as_of) = args.get("as_of").and_then(|a| a.as_str()) {
                v.extend([s("--as-of"), s(as_of)]);
            }
            // --expirations and --summary are mutually exclusive in the CLI.
            // Skip --summary when caller is asking just for expiration dates.
            let want_expirations = args.get("expirations").and_then(|b| b.as_bool()).unwrap_or(false);
            let want_all = args.get("all").and_then(|b| b.as_bool()).unwrap_or(false);
            let summary = args
                .get("summary")
                .and_then(|b| b.as_bool())
                .unwrap_or(true);
            if summary && !want_expirations && !want_all {
                v.push(s("--summary"));
            }
            if !want_expirations {
                if let Some(nm) = args.get("near_money").and_then(|n| n.as_f64()) {
                    v.extend([s("--near-money"), nm.to_string()]);
                } else {
                    // Default near-money to 10% to prevent oversized chain output
                    v.extend([s("--near-money"), s("10")]);
                }
            }
            // Target days-to-expiry pass-through. CLI picks nearest listed expiry
            // when set without an explicit --expiry.
            if let Some(dte) = args.get("target_dte").and_then(|n| n.as_i64()) {
                v.extend([s("--target-dte"), dte.to_string()]);
            }
            if let Some(expiry) = args.get("expiry").and_then(|e| e.as_str()) {
                v.extend([s("--expiry"), s(expiry)]);
            }
            if let Some(opt_type) = args.get("type").and_then(|t| t.as_str()) {
                v.extend([s("--type"), s(opt_type)]);
            }
            if want_expirations {
                v.push(s("--expirations"));
            }
            if want_all {
                v.push(s("--all"));
            }
            Ok(v)
        }
        "finance_fundamentals" => {
            let tickers = args
                .get("tickers")
                .or_else(|| args.get("ticker"))
                .and_then(|t| t.as_str())
                .ok_or_else(|| anyhow::anyhow!("tickers required"))?;
            Ok(vec![
                s("finance"),
                s("fundamentals"),
                s("--tickers"),
                s(tickers),
            ])
        }
        "finance_sync" => {
            let mut v = vec![s("finance"), s("sync")];
            if let Some(sources) = args.get("sources").and_then(|s| s.as_str()) {
                v.extend([s("--sources"), s(sources)]);
            }
            if let Some(max_pages) = args.get("max_pages").and_then(|n| n.as_u64()) {
                v.extend([s("--max-pages"), max_pages.to_string()]);
            }
            if args
                .get("strict")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--strict"));
            }
            if args
                .get("include_sports")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--include-sports"));
            }
            if args
                .get("include_historical")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--include-historical"));
            }
            if args
                .get("stream_refresh")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--stream-refresh"));
            }
            if let Some(hours) = args.get("refresh_heartbeat_hours").and_then(|n| n.as_u64()) {
                v.extend([s("--refresh-heartbeat-hours"), hours.to_string()]);
            }
            if let Some(secs) = args
                .get("stream_refresh_timeout_secs")
                .and_then(|n| n.as_u64())
            {
                v.extend([s("--stream-refresh-timeout-secs"), secs.to_string()]);
            }
            if args.get("full").and_then(|b| b.as_bool()).unwrap_or(false) {
                v.push(s("--full"));
            }
            Ok(v)
        }
        "finance_paper" => {
            let mut v = vec![s("finance"), s("paper")];
            if let Some(command) = args.get("command").and_then(|c| c.as_str()) {
                v.extend([s("--command"), s(command)]);
            }
            if let Some(mode) = args.get("mode").and_then(|m| m.as_str()) {
                v.extend([s("--mode"), s(mode)]);
            }
            if let Some(account) = args.get("account").and_then(|a| a.as_str()) {
                v.extend([s("--account"), s(account)]);
            }
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(market) = args.get("market").and_then(|m| m.as_str()) {
                v.extend([s("--market"), s(market)]);
            }
            if let Some(side) = args.get("side").and_then(|s| s.as_str()) {
                v.extend([s("--side"), s(side)]);
            }
            if let Some(action) = args.get("action").and_then(|a| a.as_str()) {
                v.extend([s("--action"), s(action)]);
            }
            if let Some(qty) = args.get("qty").and_then(|q| q.as_f64()) {
                v.extend([s("--qty"), qty.to_string()]);
            }
            if let Some(price) = args.get("price").and_then(|p| p.as_f64()) {
                v.extend([s("--price"), price.to_string()]);
            }
            if let Some(starting_cash) = args.get("starting_cash").and_then(|c| c.as_f64()) {
                v.extend([s("--starting-cash"), starting_cash.to_string()]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            Ok(v)
        }
        "finance_ibkr_account_summary" => {
            let mut v = vec![
                s("finance"),
                s("ibkr"),
                s("--command"),
                s("account-summary"),
            ];
            if let Some(account) = args.get("account").and_then(|a| a.as_str()) {
                v.extend([s("--account"), s(account)]);
            }
            if let Some(tags) = args.get("tags").and_then(|t| t.as_str()) {
                v.extend([s("--tags"), s(tags)]);
            }
            Ok(v)
        }
        "finance_ibkr_positions" => {
            let mut v = vec![s("finance"), s("ibkr"), s("--command"), s("positions")];
            if let Some(account) = args.get("account").and_then(|a| a.as_str()) {
                v.extend([s("--account"), s(account)]);
            }
            Ok(v)
        }
        "finance_ibkr_portfolio" => {
            let mut v = vec![s("finance"), s("ibkr"), s("--command"), s("portfolio")];
            if let Some(account) = args.get("account").and_then(|a| a.as_str()) {
                v.extend([s("--account"), s(account)]);
            }
            Ok(v)
        }
        "finance_ibkr_open_orders" => Ok(vec![
            s("finance"),
            s("ibkr"),
            s("--command"),
            s("open-orders"),
        ]),
        "finance_ibkr_place_order" => {
            let symbol = args
                .get("symbol")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("symbol required"))?;
            let side = args
                .get("side")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("side required"))?;
            let quantity = args
                .get("quantity")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| anyhow::anyhow!("quantity required"))?;
            let mut v = vec![
                s("finance"),
                s("ibkr"),
                s("--command"),
                s("place-order"),
                s("--symbol"),
                s(symbol),
                s("--side"),
                s(side),
                s("--quantity"),
                quantity.to_string(),
            ];
            if let Some(account) = args.get("account").and_then(|a| a.as_str()) {
                v.extend([s("--account"), s(account)]);
            }
            if let Some(order_type) = args.get("order_type").and_then(|a| a.as_str()) {
                v.extend([s("--order-type"), s(order_type)]);
            }
            if let Some(limit_price) = args.get("limit_price").and_then(|a| a.as_f64()) {
                v.extend([s("--limit-price"), limit_price.to_string()]);
            }
            if let Some(stop_price) = args.get("stop_price").and_then(|a| a.as_f64()) {
                v.extend([s("--stop-price"), stop_price.to_string()]);
            }
            if let Some(tif) = args.get("tif").and_then(|a| a.as_str()) {
                v.extend([s("--tif"), s(tif)]);
            }
            if let Some(sec_type) = args.get("sec_type").and_then(|a| a.as_str()) {
                v.extend([s("--sec-type"), s(sec_type)]);
            }
            if let Some(exchange) = args.get("exchange").and_then(|a| a.as_str()) {
                v.extend([s("--exchange"), s(exchange)]);
            }
            if let Some(primary_exchange) = args.get("primary_exchange").and_then(|a| a.as_str()) {
                v.extend([s("--primary-exchange"), s(primary_exchange)]);
            }
            if let Some(currency) = args.get("currency").and_then(|a| a.as_str()) {
                v.extend([s("--currency"), s(currency)]);
            }
            if let Some(expiry) = args.get("expiry").and_then(|a| a.as_str()) {
                v.extend([s("--expiry"), s(expiry)]);
            }
            if let Some(strike) = args.get("strike").and_then(|a| a.as_f64()) {
                v.extend([s("--strike"), strike.to_string()]);
            }
            if let Some(right) = args.get("right").and_then(|a| a.as_str()) {
                v.extend([s("--right"), s(right)]);
            }
            if let Some(multiplier) = args.get("multiplier").and_then(|a| a.as_str()) {
                v.extend([s("--multiplier"), s(multiplier)]);
            }
            if let Some(trading_class) = args.get("trading_class").and_then(|a| a.as_str()) {
                v.extend([s("--trading-class"), s(trading_class)]);
            }
            Ok(v)
        }
        "finance_ibkr_cancel_order" => {
            let order_id = args
                .get("order_id")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| anyhow::anyhow!("order_id required"))?;
            Ok(vec![
                s("finance"),
                s("ibkr"),
                s("--command"),
                s("cancel-order"),
                s("--order-id"),
                order_id.to_string(),
            ])
        }
        "web_search" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow::anyhow!("query required"))?;
            let mut v = vec![s("web"), s("search"), s("--query"), s(query)];
            if let Some(mode) = args.get("mode").and_then(|m| m.as_str()) {
                v.extend([s("--mode"), s(mode)]);
            }
            if let Some(domains) = args.get("domains").and_then(|d| d.as_str()) {
                v.extend([s("--domains"), s(domains)]);
            }
            if let Some(exclude) = args.get("exclude_domains").and_then(|d| d.as_str()) {
                v.extend([s("--exclude-domains"), s(exclude)]);
            }
            if let Some(recency) = args.get("recency").and_then(|r| r.as_str()) {
                v.extend([s("--recency"), s(recency)]);
            }
            if let Some(since) = args.get("since").and_then(|d| d.as_str()) {
                v.extend([s("--since"), s(since)]);
            }
            if let Some(until) = args.get("until").and_then(|d| d.as_str()) {
                v.extend([s("--until"), s(until)]);
            }
            if let Some(top) = args.get("top").and_then(|n| n.as_u64()) {
                v.extend([s("--top"), top.to_string()]);
            }
            if let Some(probe_top) = args.get("probe_top").and_then(|n| n.as_u64()) {
                v.extend([s("--probe-top"), probe_top.to_string()]);
            }
            if let Some(max_parallel) = args.get("max_parallel").and_then(|n| n.as_u64()) {
                v.extend([s("--max-parallel"), max_parallel.to_string()]);
            }
            if let Some(track_key) = args.get("track_key").and_then(|k| k.as_str()) {
                v.extend([s("--track-key"), s(track_key)]);
            }
            if args.get("full").and_then(|b| b.as_bool()).unwrap_or(false) {
                v.push(s("--full"));
            }
            Ok(v)
        }
        "web_read" => {
            let mut v = vec![s("web"), s("read")];
            if let Some(url) = args.get("url").and_then(|u| u.as_str()) {
                v.extend([s("--url"), s(url)]);
            }
            if let Some(urls) = args.get("urls").and_then(|u| u.as_array()) {
                for url in urls {
                    if let Some(url_str) = url.as_str() {
                        v.extend([s("--url"), s(url_str)]);
                    }
                }
            }
            if v.len() == 2 {
                return Err(anyhow::anyhow!("url or urls required"));
            }
            if let Some(max_parallel) = args.get("max_parallel").and_then(|n| n.as_u64()) {
                v.extend([s("--max-parallel"), max_parallel.to_string()]);
            }
            if let Some(max_chars) = args.get("max_chars").and_then(|n| n.as_u64()) {
                v.extend([s("--max-chars"), max_chars.to_string()]);
            }
            if args.get("full").and_then(|b| b.as_bool()).unwrap_or(false) {
                v.push(s("--full"));
            }
            Ok(v)
        }
        "web_crawl" => {
            let url = args
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or_else(|| anyhow::anyhow!("url required"))?;
            let mut v = vec![s("web"), s("crawl"), s("--url"), s(url)];
            if let Some(mp) = args.get("max_pages").and_then(|n| n.as_u64()) {
                v.extend([s("--max-pages"), mp.to_string()]);
            }
            if args.get("smart").and_then(|b| b.as_bool()).unwrap_or(false) {
                v.push(s("--smart"));
            }
            if args
                .get("sitemap")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--sitemap"));
            }
            Ok(v)
        }
        "web_extract" => {
            let mut v = vec![s("web"), s("extract")];
            if let Some(url) = args.get("url").and_then(|u| u.as_str()) {
                v.extend([s("--url"), s(url)]);
            } else if let Some(text) = args.get("text").and_then(|t| t.as_str()) {
                v.extend([s("--text"), s(text)]);
            } else {
                return Err(anyhow::anyhow!("url or text required"));
            }
            if let Some(b) = args.get("bullets").and_then(|n| n.as_u64()) {
                v.extend([s("--bullets"), b.to_string()]);
            }
            if let Some(focus) = args.get("focus").and_then(|f| f.as_str()) {
                v.extend([s("--focus"), s(focus)]);
            }
            Ok(v)
        }
        "finance_search" => {
            let query = args
                .get("query")
                .and_then(|q| q.as_str())
                .ok_or_else(|| anyhow::anyhow!("query required"))?;
            let mut v = vec![s("finance"), s("search"), s("--query"), s(query)];
            if let Some(provider) = args.get("provider").and_then(|p| p.as_str()) {
                v.extend([s("--provider"), s(provider)]);
            }
            if let Some(account) = args.get("ibkr_account").and_then(|a| a.as_str()) {
                v.extend([s("--ibkr-account"), s(account)]);
            }
            if let Some(host) = args.get("ibkr_host").and_then(|h| h.as_str()) {
                v.extend([s("--ibkr-host"), s(host)]);
            }
            if let Some(port) = args.get("ibkr_port").and_then(|p| p.as_u64()) {
                v.extend([s("--ibkr-port"), port.to_string()]);
            }
            if let Some(client_id) = args.get("ibkr_client_id").and_then(|c| c.as_i64()) {
                v.extend([s("--ibkr-client-id"), client_id.to_string()]);
            }
            if let Some(mdt) = args.get("ibkr_market_data_type").and_then(|m| m.as_i64()) {
                v.extend([s("--ibkr-market-data-type"), mdt.to_string()]);
            }
            Ok(v)
        }
        "finance_filings" => {
            let ticker = args
                .get("ticker")
                .and_then(|t| t.as_str())
                .ok_or_else(|| anyhow::anyhow!("ticker required"))?;
            let mut v = vec![s("finance"), s("filings"), s("--ticker"), s(ticker)];
            if let Some(forms) = args.get("forms").and_then(|f| f.as_str()) {
                v.extend([s("--forms"), s(forms)]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            let single_file = args
                .get("single_file")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            if !single_file
                && args
                .get("download")
                .and_then(|b| b.as_bool())
                .is_some_and(|download| !download)
            {
                v.push(s("--no-download"));
            }
            if !single_file
                && args
                .get("download_all")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--download-all"));
            }
            if single_file {
                v.push(s("--single-file"));
            }
            if !single_file
                && args
                .get("important_exhibits")
                .and_then(|b| b.as_bool())
                .is_some_and(|enabled| !enabled)
            {
                v.push(s("--primary-only"));
            }
            let include_text = args
                .get("include_text")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            if include_text {
                v.push(s("--include-text"));
            }
            let raw_text = args
                .get("raw_text")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            if raw_text {
                v.push(s("--raw-text"));
            }
            if include_text || raw_text {
                if let Some(mc) = args.get("max_chars").and_then(|n| n.as_u64()) {
                    v.extend([s("--max-chars"), mc.to_string()]);
                }
            }
            if let Some(user_agent) = args.get("user_agent").and_then(|ua| ua.as_str()) {
                v.extend([s("--user-agent"), s(user_agent)]);
            }
            Ok(v)
        }
        "finance_schedule" => {
            let mut v = vec![s("finance"), s("schedule")];
            // Default kind=macro to avoid returning 1000+ earnings rows
            let kind = args.get("kind").and_then(|k| k.as_str()).unwrap_or("macro");
            v.extend([s("--kind"), s(kind)]);
            let profile_arg = args.get("macro_profile").and_then(|p| p.as_str());
            let mut macro_profile = profile_arg.unwrap_or("market").to_string();
            if let Some(date) = args.get("date").and_then(|d| d.as_str()) {
                v.extend([s("--date"), s(date)]);
            } else {
                let from = args
                    .get("from")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| chrono::Local::now().format("%Y-%m-%d").to_string());
                let to = args
                    .get("to")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        (chrono::Local::now() + chrono::Duration::days(30))
                            .format("%Y-%m-%d")
                            .to_string()
                    });
                v.extend([s("--from"), from, s("--to"), to]);
            }
            // Default major=true when kind is macro to keep output focused
            let major = args
                .get("major")
                .and_then(|b| b.as_bool())
                .unwrap_or(kind == "macro" || kind == "all");
            if major {
                v.push(s("--major"));
                if profile_arg.is_none() {
                    macro_profile = "major".to_string();
                }
            }
            v.extend([s("--macro-profile"), macro_profile]);
            if let Some(tickers) = args.get("ticker").and_then(|s| s.as_str()) {
                v.extend([s("--ticker"), s(tickers)]);
            }
            if let Some(min_cap) = args.get("min_cap").and_then(|s| s.as_str()) {
                v.extend([s("--min-cap"), s(min_cap)]);
            }
            if let Some(time) = args.get("time").and_then(|s| s.as_str()) {
                v.extend([s("--time"), s(time)]);
            }
            Ok(v)
        }
        "finance_auctions" => {
            let mut v = vec![s("finance"), s("auctions")];
            if let Some(st) = args.get("security_type").and_then(|s| s.as_str()) {
                v.extend([s("--security-type"), s(st)]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            Ok(v)
        }
        "finance_cot" => {
            let mut v = vec![s("finance"), s("cot")];
            if let Some(query) = args.get("query").and_then(|q| q.as_str()) {
                v.extend([s("--query"), s(query)]);
            }
            if let Some(weeks) = args.get("weeks").and_then(|n| n.as_u64()) {
                v.extend([s("--weeks"), weeks.to_string()]);
            }
            if let Some(report) = args.get("report").and_then(|r| r.as_str()) {
                v.extend([s("--report"), s(report)]);
            }
            if let Some(limit) = args.get("limit").and_then(|n| n.as_u64()) {
                v.extend([s("--limit"), limit.to_string()]);
            }
            Ok(v)
        }
        "finance_nyfed" => {
            let mut v = vec![s("finance"), s("nyfed")];
            if let Some(kind) = args.get("kind").and_then(|k| k.as_str()) {
                v.extend([s("--kind"), s(kind)]);
            }
            Ok(v)
        }
        "finance_volsurface" => {
            let mut v = vec![s("finance"), s("volsurface")];
            if let Some(symbols) = args.get("symbols").and_then(|s| s.as_str()) {
                v.extend([s("--symbols"), s(symbols)]);
            }
            if let Some(history) = args.get("history").and_then(|n| n.as_u64()) {
                v.extend([s("--history"), history.to_string()]);
            }
            Ok(v)
        }
        "finance_stress" => {
            let mut v = vec![s("finance"), s("stress")];
            if let Some(range) = args.get("range").and_then(|n| n.as_u64()) {
                v.extend([s("--range"), range.to_string()]);
            }
            Ok(v)
        }
        "finance_fiscal" => {
            let mut v = vec![s("finance"), s("fiscal")];
            if let Some(kind) = args.get("kind").and_then(|k| k.as_str()) {
                v.extend([s("--kind"), s(kind)]);
            }
            Ok(v)
        }
        "finance_ecb" => {
            let mut v = vec![s("finance"), s("ecb")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(dataset) = args.get("dataset").and_then(|d| d.as_str()) {
                v.extend([s("--dataset"), s(dataset)]);
            }
            if let Some(key) = args.get("key").and_then(|k| k.as_str()) {
                v.extend([s("--key"), s(key)]);
            }
            if let Some(start) = args.get("start").and_then(|s| s.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            if let Some(end) = args.get("end").and_then(|s| s.as_str()) {
                v.extend([s("--end"), s(end)]);
            }
            Ok(v)
        }
        "finance_eia" => {
            let mut v = vec![s("finance"), s("eia")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(route) = args.get("route").and_then(|r| r.as_str()) {
                v.extend([s("--route"), s(route)]);
            }
            if let Some(start) = args.get("start").and_then(|s| s.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            if let Some(length) = args.get("length").and_then(|n| n.as_u64()) {
                v.extend([s("--length"), length.to_string()]);
            }
            Ok(v)
        }
        "finance_bis" => {
            let mut v = vec![s("finance"), s("bis")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(dataset) = args.get("dataset").and_then(|d| d.as_str()) {
                v.extend([s("--dataset"), s(dataset)]);
            }
            if let Some(key) = args.get("key").and_then(|k| k.as_str()) {
                v.extend([s("--key"), s(key)]);
            }
            if let Some(countries) = args.get("countries").and_then(|c| c.as_str()) {
                v.extend([s("--countries"), s(countries)]);
            }
            if let Some(start) = args.get("start").and_then(|s| s.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            Ok(v)
        }
        "finance_boj" => {
            let mut v = vec![s("finance"), s("boj")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(db) = args.get("db").and_then(|d| d.as_str()) {
                v.extend([s("--db"), s(db)]);
            }
            if let Some(codes) = args.get("codes").and_then(|c| c.as_str()) {
                v.extend([s("--codes"), s(codes)]);
            }
            if let Some(start) = args.get("start").and_then(|s| s.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            Ok(v)
        }
        "finance_boe" => {
            let mut v = vec![s("finance"), s("boe")];
            if let Some(preset) = args.get("preset").and_then(|p| p.as_str()) {
                v.extend([s("--preset"), s(preset)]);
            }
            if let Some(codes) = args.get("codes").and_then(|c| c.as_str()) {
                v.extend([s("--codes"), s(codes)]);
            }
            if let Some(start) = args.get("start").and_then(|s| s.as_str()) {
                v.extend([s("--start"), s(start)]);
            }
            if let Some(end) = args.get("end").and_then(|e| e.as_str()) {
                v.extend([s("--end"), s(end)]);
            }
            Ok(v)
        }
        "finance_curve" => {
            let mut v = vec![s("finance"), s("curve")];
            if let Some(commodity) = args.get("commodity").and_then(|c| c.as_str()) {
                v.extend([s("--commodity"), s(commodity)]);
            }
            if let Some(months) = args.get("months").and_then(|n| n.as_u64()) {
                v.extend([s("--months"), months.to_string()]);
            }
            if args.get("list").and_then(|b| b.as_bool()).unwrap_or(false) {
                v.push(s("--list"));
            }
            Ok(v)
        }
        "code_analyze" => {
            let path = args
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| anyhow::anyhow!("path required"))?;
            let mut v = vec![s("code"), s(path)];
            if args
                .get("pub_api")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--pub-api"));
            }
            if let Some(find) = args.get("find").and_then(|f| f.as_str()) {
                v.extend([s("--find"), s(find)]);
            }
            if args
                .get("include_files")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                v.push(s("--include-files"));
            }
            if let Some(top) = args.get("top").and_then(|n| n.as_u64()) {
                v.extend([s("--top"), top.to_string()]);
            }
            Ok(v)
        }
        "agent_run" => {
            let task = args
                .get("task")
                .and_then(|t| t.as_str())
                .ok_or_else(|| anyhow::anyhow!("task required"))?;
            let mut v = vec![s("agent"), s("run"), s("--task"), s(task)];
            if let Some(ms) = args.get("max_ms").and_then(|n| n.as_u64()) {
                v.extend([s("--max-ms"), ms.to_string()]);
            }
            Ok(v)
        }
        // ── legal ──────────────────────────────────────────────────────────
        "legal_search" => {
            let mut v = vec![s("legal"), s("search")];
            let q = args
                .get("q")
                .or_else(|| args.get("query"))
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("q required"))?;
            v.extend([s("--q"), s(q)]);
            for (json_key, flag) in [
                ("kind", "--kind"),
                ("court", "--court"),
                ("after", "--after"),
                ("before", "--before"),
                ("judge", "--judge"),
                ("status", "--status"),
                ("order", "--order"),
            ] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("cited_gt").and_then(|x| x.as_u64()) {
                v.extend([s("--cited-gt"), n.to_string()]);
            }
            if let Some(n) = args.get("limit").and_then(|x| x.as_u64()) {
                v.extend([s("--limit"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_opinion" => {
            let mut v = vec![s("legal"), s("opinion")];
            if let Some(n) = args.get("id").and_then(|x| x.as_u64()) {
                v.extend([s("--id"), n.to_string()]);
            }
            if let Some(c) = args.get("cite").and_then(|x| x.as_str()) {
                v.extend([s("--cite"), s(c)]);
            }
            if v.len() <= 2 {
                anyhow::bail!("either id or cite required");
            }
            if let Some(n) = args.get("max_chars").and_then(|x| x.as_u64()) {
                v.extend([s("--max-chars"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_docket" => {
            let mut v = vec![s("legal"), s("docket")];
            for (json_key, flag) in [("court", "--court"), ("number", "--number"), ("q", "--q")] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("id").and_then(|x| x.as_u64()) {
                v.extend([s("--id"), n.to_string()]);
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_docket needs id, or number (with court), or q");
            }
            for (json_key, flag) in [("limit", "--limit"), ("offset", "--offset")] {
                if let Some(n) = args.get(json_key).and_then(|x| x.as_u64()) {
                    v.extend([s(flag), n.to_string()]);
                }
            }
            if args.get("entries").and_then(|x| x.as_bool()) == Some(false) {
                v.extend([s("--entries"), s("false")]);
            }
            Ok(v)
        }
        "legal_cite" => {
            let mut v = vec![s("legal"), s("cite")];
            for (json_key, flag) in [("text", "--text"), ("cite", "--cite")] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("id").and_then(|x| x.as_u64()) {
                v.extend([s("--id"), n.to_string()]);
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_cite needs text, cite, or id");
            }
            if args.get("cited_by").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--cited-by"));
            }
            if args.get("authorities").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--authorities"));
            }
            if let Some(n) = args.get("limit").and_then(|x| x.as_u64()) {
                v.extend([s("--limit"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_cfr" => {
            let mut v = vec![s("legal"), s("cfr")];
            if let Some(n) = args.get("title").and_then(|x| x.as_u64()) {
                v.extend([s("--title"), n.to_string()]);
            }
            for (json_key, flag) in [
                ("part", "--part"),
                ("section", "--section"),
                ("subpart", "--subpart"),
                ("date", "--date"),
                ("q", "--q"),
                ("diff_date", "--diff-date"),
            ] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_cfr needs title (with part/section) or q");
            }
            if args.get("history").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--history"));
            }
            if args.get("structure").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--structure"));
            }
            if let Some(n) = args.get("max_chars").and_then(|x| x.as_u64()) {
                v.extend([s("--max-chars"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_fedreg" => {
            let mut v = vec![s("legal"), s("fedreg")];
            for (json_key, flag) in [
                ("q", "--q"),
                ("kind", "--kind"),
                ("agency", "--agency"),
                ("after", "--after"),
                ("before", "--before"),
                ("docket", "--docket"),
                ("cfr_part", "--cfr-part"),
                ("doc", "--doc"),
                ("facet", "--facet"),
            ] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("cfr_title").and_then(|x| x.as_u64()) {
                v.extend([s("--cfr-title"), n.to_string()]);
            }
            if args.get("text").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--text"));
            }
            if args.get("inspection").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--inspection"));
            }
            for (json_key, flag) in [("limit", "--limit"), ("max_chars", "--max-chars")] {
                if let Some(n) = args.get(json_key).and_then(|x| x.as_u64()) {
                    v.extend([s(flag), n.to_string()]);
                }
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_fedreg needs at least one of q, agency, doc, docket, or inspection");
            }
            Ok(v)
        }
        "legal_comments" => {
            let mut v = vec![s("legal"), s("comments")];
            for (json_key, flag) in [
                ("docket", "--docket"),
                ("q", "--q"),
                ("agency", "--agency"),
                ("after", "--after"),
                ("id", "--id"),
            ] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_comments needs docket, q, or id");
            }
            if args.get("text").and_then(|x| x.as_bool()) == Some(true) {
                v.push(s("--text"));
            }
            if let Some(n) = args.get("limit").and_then(|x| x.as_u64()) {
                v.extend([s("--limit"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_enforcement" => {
            let mut v = vec![s("legal"), s("enforcement")];
            for (json_key, flag) in [("source", "--source"), ("q", "--q"), ("after", "--after")] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("limit").and_then(|x| x.as_u64()) {
                v.extend([s("--limit"), n.to_string()]);
            }
            Ok(v)
        }
        "legal_statute" => {
            let mut v = vec![s("legal"), s("statute")];
            if let Some(n) = args.get("title").and_then(|x| x.as_u64()) {
                v.extend([s("--title"), n.to_string()]);
            }
            for (json_key, flag) in [("section", "--section"), ("bill", "--bill")] {
                if let Some(val) = args.get(json_key).and_then(|x| x.as_str()) {
                    v.extend([s(flag), s(val)]);
                }
            }
            if let Some(n) = args.get("congress").and_then(|x| x.as_u64()) {
                v.extend([s("--congress"), n.to_string()]);
            }
            if v.len() <= 2 {
                anyhow::bail!("legal_statute needs title (with section) or bill");
            }
            if let Some(n) = args.get("max_chars").and_then(|x| x.as_u64()) {
                v.extend([s("--max-chars"), n.to_string()]);
            }
            Ok(v)
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {tool}")),
    }
}

async fn mcp_run_subprocess(args: Vec<String>) -> anyhow::Result<String> {
    let exe = std::env::current_exe().context("get current exe path")?;
    let output = TokioCommand::new(&exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("spawn eli subprocess")?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    if !output.status.success() && stdout.trim().is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("exit {}: {}", output.status, stderr.trim()));
    }

    Ok(stdout)
}

// ── MCP Streamable HTTP transport ────────────────────────────────────────────
// POST /mcp  → JSON-RPC request/response (same handlers as stdio mode)
// GET  /     → health check

async fn cmd_mcp_http(port: u16) -> Result<()> {
    use axum::{
        extract::Json as AxumJson,
        http::{header, HeaderName, Method, StatusCode},
        response::IntoResponse,
        routing::{get, post},
        Router,
    };
    use tower_http::cors::{Any, CorsLayer};

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS, Method::DELETE])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::AUTHORIZATION,
            HeaderName::from_static("mcp-session-id"),
            HeaderName::from_static("mcp-protocol-version"),
        ])
        // The claude.ai web client runs in-browser; CORS hides response headers from
        // JS unless they are explicitly exposed, so the connector cannot read the
        // Mcp-Session-Id off the initialize response without this. Expose the session
        // + protocol headers (and WWW-Authenticate for the OAuth challenge path).
        .expose_headers([
            HeaderName::from_static("mcp-session-id"),
            HeaderName::from_static("mcp-protocol-version"),
            header::WWW_AUTHENTICATE,
        ]);

    let app = Router::new()
        .route("/", get(mcp_http_health))
        // MCP Streamable HTTP: GET /mcp must open an SSE stream or return 405. We push no
        // server-initiated messages, so registering POST only makes axum auto-return 405
        // (with Allow: POST) on GET — the spec-correct "no stream here, POST only" answer.
        // The old GET→health handler returned 200 JSON, which made claude.ai's connector
        // probe reject the endpoint as "not a valid MCP server." Health stays on GET /.
        .route("/mcp", post(mcp_http_handle))
        .route("/favicon.ico", get(mcp_favicon))
        .route("/favicon.png", get(mcp_favicon))
        // ── OAuth-discovery stub (added 2026-06-25) ──────────────────────────────
        // claude.ai's WEB connector regressed in the June-2026 connector/auth wave:
        // "Add connector" now runs a MANDATORY OAuth-discovery preflight (RFC 9728
        // PRM → RFC 8414 AS metadata → RFC 7591 DCR) and treats 404s on these as a
        // HARD validation failure — a flat-200 authless server that 404s here is
        // rejected as "not a valid MCP server" (GitHub claude-ai-mcp #402/#172/#143).
        // Claude Code + Desktop still tolerate authless, which is why only the web/
        // phone connector broke. We rubber-stamp the whole flow so the preflight
        // completes while the server stays effectively open: any/no bearer is
        // accepted on /mcp, and /authorize auto-approves. POST /mcp stays 200 (NO
        // 401) so the existing authless runtime path is unbroken (and we dodge the
        // HTTP/2 WWW-Authenticate case-sensitivity bug #219). Discovery URLs are
        // derived per-request from X-Forwarded-Host/Proto, so this is tunnel-agnostic.
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server),
        )
        .route(
            "/.well-known/oauth-authorization-server/mcp",
            get(oauth_authorization_server),
        )
        .route("/register", post(oauth_register))
        .route("/authorize", get(oauth_authorize))
        .route("/token", post(oauth_token))
        .layer(cors);

    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));
    eprintln!("eli mcp http → http://0.0.0.0:{port}/mcp");
    eprintln!("Waiting for connections...");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind port {port}"))?;

    axum::serve(listener, app).await.context("mcp http serve")?;
    Ok(())
}

/// Serve the embedded eli wordmark at /favicon.ico and /favicon.png. A connector UI that
/// fetches the server domain's favicon now gets this instead of falling through to ngrok's.
async fn mcp_favicon() -> impl axum::response::IntoResponse {
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (axum::http::header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON_PNG,
    )
}

async fn mcp_http_health() -> impl axum::response::IntoResponse {
    axum::Json(serde_json::json!({
        "status": "ok",
        "server": "eli-mcp",
        "transport": "streamable-http"
    }))
}

// ── OAuth-discovery stub handlers ────────────────────────────────────────────
// See the route table in cmd_mcp_http for why these exist. They make claude.ai's
// post-June-2026 OAuth preflight complete against an effectively-authless server.
// Nothing here grants real security — every token is accepted and /authorize
// auto-approves. The point is purely to satisfy the connector-add discovery gate.

/// Reconstruct the public base URL ("https://host") the client dialed, from the
/// reverse-proxy headers ngrok/Cloudflare set. The server binds 0.0.0.0:<port>
/// and never sees its own public hostname otherwise, so the RFC 9728 `resource`
/// (which MUST exactly match the URL claude.ai points at) is derived per request.
fn mcp_public_base(headers: &axum::http::HeaderMap) -> String {
    let host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(axum::http::header::HOST)
                .and_then(|v| v.to_str().ok())
        })
        .unwrap_or("localhost:8484");
    let is_local = host.starts_with("localhost") || host.starts_with("127.0.0.1");
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(if is_local { "http" } else { "https" });
    format!("{proto}://{host}")
}

/// Minimal percent-decoder for a single query-string value (RFC 3986 + '+'→space).
fn mcp_percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match (mcp_hex_val(bytes[i + 1]), mcp_hex_val(bytes[i + 2])) {
                    (Some(a), Some(b)) => {
                        out.push(a * 16 + b);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mcp_hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// RFC 9728 Protected Resource Metadata. `authorization_servers` points back at
/// this same origin, where the AS metadata below lives.
async fn oauth_protected_resource(
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    let base = mcp_public_base(&headers);
    axum::Json(serde_json::json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": []
    }))
}

/// RFC 8414 Authorization Server Metadata. Advertises the rubber-stamp endpoints.
async fn oauth_authorization_server(
    headers: axum::http::HeaderMap,
) -> impl axum::response::IntoResponse {
    let base = mcp_public_base(&headers);
    axum::Json(serde_json::json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "registration_endpoint": format!("{base}/register"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": []
    }))
}

/// RFC 7591 Dynamic Client Registration. Accept anyone; echo back the redirect_uris
/// the client sent (claude.ai expects its own callback echoed) and return a fixed
/// public client_id.
async fn oauth_register(
    body: Option<axum::extract::Json<serde_json::Value>>,
) -> impl axum::response::IntoResponse {
    let redirect_uris = body
        .as_ref()
        .and_then(|b| b.get("redirect_uris").cloned())
        .unwrap_or_else(|| serde_json::json!(["https://claude.ai/api/mcp/auth_callback"]));
    (
        axum::http::StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "client_id": "eli-anon",
            "token_endpoint_auth_method": "none",
            "redirect_uris": redirect_uris,
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        })),
    )
}

/// Authorization endpoint. Auto-approve: 302 straight back to the client's
/// redirect_uri with a fixed code, preserving `state` byte-for-byte (we read it
/// from the RAW query so claude.ai's base64url state round-trips unchanged).
async fn oauth_authorize(
    axum::extract::RawQuery(query): axum::extract::RawQuery,
) -> impl axum::response::IntoResponse {
    let query = query.unwrap_or_default();
    let mut redirect_uri = String::new();
    let mut state_raw = String::new();
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "redirect_uri" => redirect_uri = mcp_percent_decode(v),
            "state" => state_raw = v.to_string(), // keep RAW (still percent-encoded)
            _ => {}
        }
    }
    if redirect_uri.is_empty() {
        redirect_uri = "https://claude.ai/api/mcp/auth_callback".to_string();
    }
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    let location = if state_raw.is_empty() {
        format!("{redirect_uri}{sep}code=eli-anon-code")
    } else {
        format!("{redirect_uri}{sep}code=eli-anon-code&state={state_raw}")
    };
    (
        axum::http::StatusCode::FOUND,
        [(axum::http::header::LOCATION, location)],
    )
}

/// Token endpoint. Ignore the request entirely (no PKCE verification — it's a
/// rubber stamp) and mint a bearer token claude.ai will then send to /mcp.
async fn oauth_token() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        axum::Json(serde_json::json!({
            "access_token": "eli-anon-token",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": ""
        })),
    )
}

async fn mcp_http_handle(
    headers: axum::http::HeaderMap,
    axum::extract::Json(request): axum::extract::Json<serde_json::Value>,
) -> axum::response::Response {
    use axum::http::{HeaderValue, StatusCode};
    use axum::response::IntoResponse;

    // Streamable-HTTP session: claude.ai's runtime client treats the connection as
    // "established" only once an Mcp-Session-Id rides on the responses, and loops on
    // initialize forever without it. We are effectively stateless/single-user, so a
    // constant id suffices — claude.ai only needs it PRESENT. Reflect the client's id
    // when it sends one; never 400 on a missing id (claude.ai's runtime intermittently
    // drops it — claude-code#41836), which keeps us as tolerant as the authless path.
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "eli-mcp-session".to_string());
    let proto = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            request
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "2025-11-25".to_string());

    let method = request
        .get("method")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());

    let mut response = match method.as_deref() {
        None => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": -32600, "message": "Missing method" }
            })),
        )
            .into_response(),
        // JSON-RPC notifications/responses: HTTP 202 Accepted with NO body (spec).
        Some(m) if m.starts_with("notifications/") => StatusCode::ACCEPTED.into_response(),
        Some(m) => {
            let id = request.get("id").cloned().unwrap_or(serde_json::Value::Null);
            let body = match m {
                "initialize" => mcp_initialize(id, &request),
                "tools/list" => mcp_tools_list(id),
                "tools/call" => mcp_tools_call_full(id, &request).await,
                _ => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "Method not found" }
                }),
            };
            (StatusCode::OK, axum::Json(body)).into_response()
        }
    };

    // Attach the session headers to EVERY /mcp response (initialize + follow-ups) so
    // claude.ai can establish and carry the session.
    let h = response.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&session_id) {
        h.insert("mcp-session-id", v);
    }
    if let Ok(v) = HeaderValue::from_str(&proto) {
        h.insert("mcp-protocol-version", v);
    }
    response
}

#[cfg(test)]
mod mcp_tool_tests {
    use super::*;

    #[test]
    fn mcp_build_cli_args_maps_finance_paper() {
        let args = serde_json::json!({
            "command": "trade",
            "mode": "live_like",
            "account": "sandbox",
            "provider": "kalshi",
            "market": "KXBTC-26FEB28-B70000",
            "side": "yes",
            "action": "buy",
            "qty": 3.0,
            "price": 0.42
        });
        let built = mcp_build_cli_args("finance_paper", &args).expect("build args");
        assert_eq!(built[0], "finance");
        assert_eq!(built[1], "paper");
        assert!(built.contains(&"--mode".to_string()));
        assert!(built.contains(&"live_like".to_string()));
        assert!(built.contains(&"--provider".to_string()));
        assert!(built.contains(&"kalshi".to_string()));
        assert!(built.contains(&"--qty".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_finance_sync_extended_flags() {
        let args = serde_json::json!({
            "sources": "kalshi",
            "strict": true,
            "include_sports": true,
            "include_historical": true,
            "stream_refresh": true,
            "refresh_heartbeat_hours": 12,
            "stream_refresh_timeout_secs": 300,
            "full": true
        });
        let built = mcp_build_cli_args("finance_sync", &args).expect("build args");
        assert_eq!(built[0], "finance");
        assert_eq!(built[1], "sync");
        assert!(built.contains(&"--sources".to_string()));
        assert!(built.contains(&"kalshi".to_string()));
        assert!(built.contains(&"--strict".to_string()));
        assert!(built.contains(&"--include-sports".to_string()));
        assert!(built.contains(&"--include-historical".to_string()));
        assert!(built.contains(&"--stream-refresh".to_string()));
        assert!(built.contains(&"--refresh-heartbeat-hours".to_string()));
        assert!(built.contains(&"12".to_string()));
        assert!(built.contains(&"--stream-refresh-timeout-secs".to_string()));
        assert!(built.contains(&"300".to_string()));
        assert!(built.contains(&"--full".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_finance_options_ibkr_overrides() {
        let args = serde_json::json!({
            "ticker": "SPY",
            "provider": "ibkr",
            "ibkr_account": "U1234567",
            "ibkr_host": "127.0.0.1",
            "ibkr_port": 7497,
            "ibkr_client_id": 42,
            "ibkr_market_data_type": 3,
            "as_of": "2026-05-08",
            "summary": true,
            "near_money": 5
        });
        let built = mcp_build_cli_args("finance_options", &args).expect("build args");
        assert_eq!(built[0], "finance");
        assert_eq!(built[1], "options");
        assert!(built.contains(&"--provider".to_string()));
        assert!(built.contains(&"ibkr".to_string()));
        assert!(built.contains(&"--ibkr-account".to_string()));
        assert!(built.contains(&"U1234567".to_string()));
        assert!(built.contains(&"--ibkr-host".to_string()));
        assert!(built.contains(&"127.0.0.1".to_string()));
        assert!(built.contains(&"--ibkr-port".to_string()));
        assert!(built.contains(&"7497".to_string()));
        assert!(built.contains(&"--ibkr-client-id".to_string()));
        assert!(built.contains(&"42".to_string()));
        assert!(built.contains(&"--ibkr-market-data-type".to_string()));
        assert!(built.contains(&"3".to_string()));
        assert!(built.contains(&"--as-of".to_string()));
        assert!(built.contains(&"2026-05-08".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_finance_filings_raw_text() {
        let args = serde_json::json!({
            "ticker": "AAPL",
            "forms": "10-K",
            "limit": 1,
            "raw_text": true,
            "max_chars": 500
        });
        let built = mcp_build_cli_args("finance_filings", &args).expect("build args");
        assert_eq!(built[0], "finance");
        assert_eq!(built[1], "filings");
        assert!(built.contains(&"--ticker".to_string()));
        assert!(built.contains(&"AAPL".to_string()));
        assert!(built.contains(&"--raw-text".to_string()));
        assert!(built.contains(&"--max-chars".to_string()));
        assert!(built.contains(&"500".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_finance_search_ibkr_overrides() {
        let args = serde_json::json!({
            "query": "AAPL",
            "provider": "ibkr",
            "ibkr_host": "127.0.0.1",
            "ibkr_port": 4001,
            "ibkr_client_id": 17,
            "ibkr_market_data_type": 4
        });
        let built = mcp_build_cli_args("finance_search", &args).expect("build args");
        assert_eq!(built[0], "finance");
        assert_eq!(built[1], "search");
        assert!(built.contains(&"--provider".to_string()));
        assert!(built.contains(&"ibkr".to_string()));
        assert!(built.contains(&"--ibkr-host".to_string()));
        assert!(built.contains(&"127.0.0.1".to_string()));
        assert!(built.contains(&"--ibkr-port".to_string()));
        assert!(built.contains(&"4001".to_string()));
        assert!(built.contains(&"--ibkr-client-id".to_string()));
        assert!(built.contains(&"17".to_string()));
        assert!(built.contains(&"--ibkr-market-data-type".to_string()));
        assert!(built.contains(&"4".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_web_search_advanced_filters() {
        let args = serde_json::json!({
            "query": "fed decision",
            "mode": "news",
            "domains": "reuters.com,bloomberg.com",
            "exclude_domains": "example.com",
            "recency": "week",
            "since": "2026-01-01",
            "until": "2026-01-31",
            "top": 10,
            "probe_top": 3,
            "max_parallel": 4,
            "track_key": "fed-weekly",
            "full": true
        });
        let built = mcp_build_cli_args("web_search", &args).expect("build args");
        assert_eq!(built[0], "web");
        assert_eq!(built[1], "search");
        assert!(built.contains(&"--mode".to_string()));
        assert!(built.contains(&"news".to_string()));
        assert!(built.contains(&"--domains".to_string()));
        assert!(built.contains(&"--exclude-domains".to_string()));
        assert!(built.contains(&"--track-key".to_string()));
        assert!(built.contains(&"--full".to_string()));
    }

    #[test]
    fn mcp_build_cli_args_maps_web_read_single_and_batch() {
        let single = serde_json::json!({
            "url": "https://example.com/a"
        });
        let built_single = mcp_build_cli_args("web_read", &single).expect("single args");
        assert_eq!(built_single[0], "web");
        assert_eq!(built_single[1], "read");
        assert!(built_single.contains(&"--url".to_string()));

        let batch = serde_json::json!({
            "urls": ["https://example.com/a", "https://example.com/b"],
            "max_parallel": 8,
            "max_chars": 1600,
            "full": true
        });
        let built_batch = mcp_build_cli_args("web_read", &batch).expect("batch args");
        let url_flag_count = built_batch.iter().filter(|arg| *arg == "--url").count();
        assert_eq!(url_flag_count, 2);
        assert!(built_batch.contains(&"--max-parallel".to_string()));
        assert!(built_batch.contains(&"8".to_string()));
        assert!(built_batch.contains(&"--max-chars".to_string()));
        assert!(built_batch.contains(&"1600".to_string()));
        assert!(built_batch.contains(&"--full".to_string()));
    }

    #[test]
    fn mcp_finance_odds_summary_preserves_search_rank_without_literal_filter_note() {
        let input = serde_json::json!({
            "query": "jobs report",
            "total_markets": 2,
            "markets": [
                {
                    "source": "polymarket",
                    "ticker": "LOWVOL",
                    "event_ticker": "JOBS",
                    "title": "Will the US add between 150k and 200k jobs in April",
                    "probability_yes": 0.11,
                    "volume_usd": 1000.0
                },
                {
                    "source": "kalshi",
                    "ticker": "HIGHVOL",
                    "event_ticker": "NFP",
                    "title": "Large unrelated-looking payroll market",
                    "probability_yes": 0.62,
                    "volume_usd": 100000.0
                }
            ]
        });

        let summary = mcp_build_summary("finance_odds", &serde_json::to_string(&input).unwrap());
        assert!(
            !summary.contains("_note:"),
            "literal title filtering should not add noise"
        );
        let first = summary
            .find("Will the US add between 150k and 200k jobs in April")
            .expect("ranked first market should be present");
        let second = summary
            .find("Large unrelated-looking payroll market")
            .expect("second market should be present");
        assert!(first < second, "MCP summary should preserve search rank");
    }

    #[test]
    fn mcp_finance_options_summary_reads_nested_metrics() {
        let input = serde_json::json!({
            "ticker": "SPY",
            "underlying_price": 731.58,
            "selected_expiry": "2026-07-17",
            "selected_days_to_expiry": 70,
            "selection_reason": "nearest_monthly_opex_gte_14dte",
            "expirations": ["2026-05-08", "2026-07-17"],
            "atm_iv": 0.18,
            "metrics": {
                "put_call_ratio_volume": 0.95,
                "put_call_ratio_oi": 1.12,
                "total_call_volume": 100,
                "total_put_volume": 95,
                "total_call_oi": 1000,
                "total_put_oi": 1120,
                "max_pain": 730.0,
                "atm_iv": 0.18
            }
        });

        let summary = mcp_build_summary("finance_options", &serde_json::to_string(&input).unwrap());
        assert!(summary.contains("\"pc_vol_ratio\":0.95"));
        assert!(summary.contains("\"pc_oi_ratio\":1.12"));
        assert!(summary.contains("\"iv_pct\":18.0"));
        assert!(summary.contains("\"total_oi\":2120"));
        assert!(summary.contains("\"total_vol\":195"));
        assert!(summary.contains("\"selected_dte\":70"));
    }

    #[test]
    fn mcp_strip_metadata_removes_per_item_keys() {
        let input = serde_json::json!({
            "query": "crude",
            "markets": [
                {
                    "title": "Oil above $80?",
                    "volume_usd": 50000,
                    "freshness": { "observed_at": "2026-03-08" },
                    "delta_since_last_sync": { "probability_delta_pct_points": 1.2 },
                    "run_meta": { "source": "kalshi" }
                },
                {
                    "title": "Oil below $60?",
                    "volume_usd": 30000,
                    "freshness": { "observed_at": "2026-03-08" },
                    "delta_since_last_sync": { "probability_delta_pct_points": -0.5 },
                    "run_meta": { "source": "kalshi" }
                }
            ]
        });
        let raw = serde_json::to_string(&input).unwrap();
        let cleaned = mcp_strip_metadata(&raw).expect("should strip metadata");
        let v: serde_json::Value = serde_json::from_str(&cleaned).unwrap();

        // Items should not have stripped keys
        let markets = v["markets"].as_array().unwrap();
        for m in markets {
            assert!(m.get("freshness").is_none(), "freshness should be stripped");
            assert!(m.get("delta_since_last_sync").is_none(), "delta should be stripped");
            assert!(m.get("run_meta").is_none(), "run_meta should be stripped");
            assert!(m.get("title").is_some(), "title should remain");
            assert!(m.get("volume_usd").is_some(), "volume should remain");
        }

        // Top-level _meta should have hoisted values
        assert!(v.get("_meta").is_some(), "_meta should exist");
        assert!(v["_meta"].get("freshness").is_some());

        // Cleaned output should be smaller
        assert!(cleaned.len() < raw.len(), "cleaned should be smaller than raw");
    }

    #[test]
    fn mcp_strip_metadata_returns_none_when_nothing_to_strip() {
        let input = serde_json::json!({
            "ticker": "SPY",
            "price": 595.0
        });
        let raw = serde_json::to_string(&input).unwrap();
        assert!(mcp_strip_metadata(&raw).is_none());
    }

    /// Every advertised tool must map to CLI args. Catches the failure where a
    /// catalog entry is added (or an arg renamed) without the dispatch arm to
    /// back it — which a client only discovers as "Unknown tool" mid-task.
    #[test]
    fn every_advertised_legal_tool_dispatches() {
        let catalog: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("legal_tools.json")).expect("legal catalog parses");
        assert_eq!(catalog.len(), 9, "catalog size changed — update the samples below");

        // One minimally-valid argument set per tool, chosen to satisfy each
        // tool's required-argument check.
        let samples = [
            ("legal_cite", serde_json::json!({"cite": "597 U.S. 1"})),
            ("legal_cfr", serde_json::json!({"title": 17, "section": "240.10b5-1"})),
            ("legal_fedreg", serde_json::json!({"q": "tariff"})),
            ("legal_search", serde_json::json!({"q": "qualified immunity"})),
            ("legal_docket", serde_json::json!({"court": "nysd", "number": "1:22-cr-00673"})),
            ("legal_opinion", serde_json::json!({"cite": "597 U.S. 1"})),
            ("legal_comments", serde_json::json!({"docket": "SEC-2026-5190"})),
            ("legal_enforcement", serde_json::json!({"source": "doj"})),
            ("legal_statute", serde_json::json!({"title": 15, "section": "78j"})),
        ];

        for tool in &catalog {
            let name = tool.get("name").and_then(|n| n.as_str()).expect("tool has a name");
            assert!(
                tool.get("description").and_then(|d| d.as_str()).is_some_and(|d| d.len() > 200),
                "{name} needs a description that tells the model when to reach for it"
            );
            let (_, args) = samples
                .iter()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("no sample args for advertised tool {name}"));
            let built = mcp_build_cli_args(name, args)
                .unwrap_or_else(|e| panic!("{name} failed to build CLI args: {e}"));
            assert_eq!(built[0], "legal", "{name} must dispatch to the legal subcommand");
        }
    }

    /// The required-argument checks must actually fire — a tool that silently
    /// accepts an empty call wastes a round trip and returns a confusing error
    /// from the CLI layer instead of a usable one from the MCP layer.
    #[test]
    fn legal_tools_reject_empty_arguments() {
        let empty = serde_json::json!({});
        for tool in [
            "legal_cite",
            "legal_cfr",
            "legal_search",
            "legal_docket",
            "legal_opinion",
            "legal_comments",
            "legal_statute",
        ] {
            assert!(
                mcp_build_cli_args(tool, &empty).is_err(),
                "{tool} should reject an empty argument set"
            );
        }
    }

    /// The profile switch is what makes one binary two products; if it stops
    /// filtering, a legal-search user gets 21 markets tools they did not ask for.
    #[test]
    fn profile_filters_the_catalog() {
        let all = mcp_catalog().len();
        std::env::set_var("ELI_MCP_PROFILE", "legal");
        let legal = mcp_catalog();
        std::env::set_var("ELI_MCP_PROFILE", "finance");
        let finance = mcp_catalog();
        std::env::remove_var("ELI_MCP_PROFILE");

        assert!(legal.iter().all(|t| t["name"].as_str().is_some_and(|n| n.starts_with("legal_"))));
        assert!(finance.iter().all(|t| t["name"].as_str().is_some_and(|n| n.starts_with("finance_"))));
        assert_eq!(all, legal.len() + finance.len());
    }
}
