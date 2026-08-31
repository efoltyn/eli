// Legal research command handlers.
//
// Thin by design, exactly like cmd/finance/*: parse/validate CLI args, build an
// eli_core::legal request, print the JSON. All fetching, parsing and
// degradation logic lives in eli-core so the MCP layer and the CLI share one
// implementation.

/// Every legal response carries `warnings`; when a run produced only warnings
/// and no data, say so on stderr so a human running the CLI sees it (the JSON
/// on stdout stays machine-clean).
fn legal_emit<T: Serialize>(
    resp: &T,
    out: Option<PathBuf>,
    tool: &str,
    format: &str,
) -> Result<()> {
    if format.trim().to_ascii_lowercase() != "json" {
        anyhow::bail!("unsupported --format (only 'json' is implemented)");
    }
    if let Some(out_path) = out {
        let wr = write_json_out_with_meta(out_path, resp, tool, &[])?;
        println!(
            "{{\"ok\":true,\"path\":{},\"meta_path\":{}}}",
            serde_json::to_string(&wr.out_path.display().to_string())
                .unwrap_or_else(|_| "\"\"".to_string()),
            serde_json::to_string(&wr.meta_path.display().to_string())
                .unwrap_or_else(|_| "\"\"".to_string())
        );
        return Ok(());
    }
    let json = serde_json::to_string_pretty(resp).context("serialize legal response")?;
    println!("{json}");
    Ok(())
}

async fn cmd_legal_search(args: LegalSearchArgs) -> Result<()> {
    let req = eli_core::legal::caselaw::CaseSearchRequest {
        query: args.q,
        kind: args.kind,
        courts: split_csv(args.court.as_deref()),
        filed_after: args.after,
        filed_before: args.before,
        judge: args.judge,
        cited_gt: args.cited_gt,
        status: args.status,
        order_by: args.order,
        limit: args.limit.clamp(1, 100),
    };
    let resp = eli_core::legal::caselaw::fetch_case_search(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal search")?;
    legal_emit(&resp, args.out, "legal.search", &args.format)
}

async fn cmd_legal_opinion(args: LegalOpinionArgs) -> Result<()> {
    if args.id.is_none() && args.cite.is_none() {
        anyhow::bail!("legal opinion requires --id or --cite");
    }
    let req = eli_core::legal::caselaw::OpinionRequest {
        id: args.id,
        cite: args.cite,
        max_chars: args.max_chars,
    };
    let resp = eli_core::legal::caselaw::fetch_opinion(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal opinion")?;
    legal_emit(&resp, args.out, "legal.opinion", &args.format)
}

async fn cmd_legal_docket(args: LegalDocketArgs) -> Result<()> {
    if args.id.is_none() && args.number.is_none() && args.q.is_none() {
        anyhow::bail!("legal docket requires --id, or --number (with --court), or --q");
    }
    let req = eli_core::legal::docket::DocketRequest {
        court: args.court,
        docket_number: args.number,
        docket_id: args.id,
        query: args.q,
        include_entries: args.entries,
        limit: args.limit.clamp(1, 500),
        offset: args.offset.unwrap_or(0),
    };
    let resp = eli_core::legal::docket::fetch_docket(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal docket")?;
    legal_emit(&resp, args.out, "legal.docket", &args.format)
}

async fn cmd_legal_cite(args: LegalCiteArgs) -> Result<()> {
    if args.text.is_none() && args.cite.is_none() && args.id.is_none() {
        anyhow::bail!("legal cite requires --text, --cite, or --id");
    }
    let req = eli_core::legal::citator::CitationRequest {
        text: args.text,
        cite: args.cite,
        opinion_id: args.id,
        cited_by: args.cited_by,
        authorities: args.authorities,
        limit: args.limit.clamp(1, 100),
    };
    let resp = eli_core::legal::citator::fetch_citations(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal cite")?;
    legal_emit(&resp, args.out, "legal.cite", &args.format)
}

async fn cmd_legal_cfr(args: LegalCfrArgs) -> Result<()> {
    if args.title.is_none() && args.q.is_none() {
        anyhow::bail!("legal cfr requires --title (with optional --part/--section) or --q");
    }
    let req = eli_core::legal::cfr::CfrRequest {
        title: args.title,
        part: args.part,
        section: args.section,
        subpart: args.subpart,
        date: args.date,
        query: args.q,
        history: args.history,
        structure: args.structure,
        diff_date: args.diff_date,
        max_chars: args.max_chars,
    };
    let resp = eli_core::legal::cfr::fetch_cfr(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal cfr")?;
    legal_emit(&resp, args.out, "legal.cfr", &args.format)
}

async fn cmd_legal_fedreg(args: LegalFedregArgs) -> Result<()> {
    let req = eli_core::legal::fedreg::FedregRequest {
        query: args.q,
        kind: args.kind,
        agencies: split_csv(args.agency.as_deref()),
        published_after: args.after,
        published_before: args.before,
        docket: args.docket,
        cfr_title: args.cfr_title,
        cfr_part: args.cfr_part,
        document_number: args.doc,
        with_text: args.text,
        public_inspection: args.inspection,
        facet: args.facet,
        max_chars: args.max_chars,
        limit: args.limit.clamp(1, 100),
    };
    let resp = eli_core::legal::fedreg::fetch_fedreg(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal fedreg")?;
    legal_emit(&resp, args.out, "legal.fedreg", &args.format)
}

async fn cmd_legal_comments(args: LegalCommentsArgs) -> Result<()> {
    if args.docket.is_none() && args.q.is_none() && args.id.is_none() {
        anyhow::bail!("legal comments requires --docket, --q, or --id");
    }
    let req = eli_core::legal::comments::CommentsRequest {
        docket: args.docket,
        query: args.q,
        agency: args.agency,
        posted_after: args.after,
        comment_id: args.id,
        with_text: args.text,
        limit: args.limit.clamp(1, 250),
    };
    let resp = eli_core::legal::comments::fetch_comments(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal comments")?;
    legal_emit(&resp, args.out, "legal.comments", &args.format)
}

async fn cmd_legal_enforcement(args: LegalEnforcementArgs) -> Result<()> {
    let req = eli_core::legal::enforcement::EnforcementRequest {
        source: args.source,
        query: args.q,
        after: args.after,
        limit: args.limit.clamp(1, 100),
    };
    let resp = eli_core::legal::enforcement::fetch_enforcement(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal enforcement")?;
    legal_emit(&resp, args.out, "legal.enforcement", &args.format)
}

async fn cmd_legal_statute(args: LegalStatuteArgs) -> Result<()> {
    if args.title.is_none() && args.bill.is_none() {
        anyhow::bail!("legal statute requires --title (with --section) or --bill (with --congress)");
    }
    let req = eli_core::legal::statute::StatuteRequest {
        title: args.title,
        section: args.section,
        congress: args.congress,
        bill: args.bill,
        max_chars: args.max_chars,
    };
    let resp = eli_core::legal::statute::fetch_statute(req)
        .await
        .map_err(|e| anyhow::anyhow!(e))
        .context("legal statute")?;
    legal_emit(&resp, args.out, "legal.statute", &args.format)
}

/// Comma-separated CLI list -> Vec<String>, trimming blanks.
fn split_csv(input: Option<&str>) -> Vec<String> {
    input
        .map(|s| {
            s.split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
