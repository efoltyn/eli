pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG")
                .unwrap_or_else(|_| {
                    "error,eli=warn,eli_cli=warn".to_string()
                }),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::try_parse()?;

    match cli.cmd {
        None => {
            // Default: show help. (Previously launched the chat agent; that's gone.)
            use clap::CommandFactory as _;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
        Some(Command::Setup) => cmd_setup().await,
        Some(Command::Init) => cmd_init().await,
        Some(Command::Config { set, value }) => cmd_config(set, value).await,
        Some(Command::ToolInfo { path }) => cmd_tool_info(path),
        Some(Command::Finance { cmd }) => cmd_finance(cmd).await,
        Some(Command::Web { cmd }) => cmd_web(cmd).await,
        Some(Command::Legal { cmd }) => cmd_legal(cmd).await,
        Some(Command::Mcp(args)) => {
            // An explicit --profile wins; otherwise whatever the binary set at
            // startup (legal-search => legal) stands.
            if let Some(profile) = args.profile.as_deref() {
                std::env::set_var("ELI_MCP_PROFILE", profile.trim().to_ascii_lowercase());
            }
            if let Some(McpSubcommand::Share(share_args)) = args.cmd {
                cmd_mcp_share(share_args).await
            } else if args.http {
                cmd_mcp_http(args.port).await
            } else {
                cmd_mcp().await
            }
        }
        Some(Command::Picks { cmd }) => cmd_picks(cmd).await,
    }
}

async fn cmd_finance(cmd: FinanceCommand) -> Result<()> {
    match cmd {
        FinanceCommand::Timeseries(args) => cmd_finance_timeseries(args).await,
        FinanceCommand::Movers(args) => cmd_finance_movers(args).await,
        FinanceCommand::Fundamentals(args) => cmd_finance_fundamentals(args).await,
        FinanceCommand::Search(args) => cmd_finance_search(args).await,
        FinanceCommand::Filings(args) | FinanceCommand::Sec(args) => {
            cmd_finance_filings(args).await
        }
        FinanceCommand::Schedule(args) => cmd_finance_schedule(args).await,
        FinanceCommand::RatePath(args) => cmd_finance_rate_path(args).await,
        FinanceCommand::Odds(args) => cmd_finance_odds(args).await,
        FinanceCommand::Options(args) => cmd_finance_options(args).await,
        FinanceCommand::Sync(args) => cmd_finance_sync(args).await,
        FinanceCommand::Paper(args) => cmd_finance_paper(args).await,
        FinanceCommand::Ibkr(args) => cmd_finance_ibkr(args).await,
        FinanceCommand::Auctions(args) => cmd_finance_auctions(args).await,
        FinanceCommand::Cot(args) => cmd_finance_cot(args).await,
        FinanceCommand::Curve(args) => cmd_finance_curve(args).await,
        FinanceCommand::Nyfed(args) => cmd_finance_nyfed(args).await,
        FinanceCommand::Volsurface(args) => cmd_finance_volsurface(args).await,
        FinanceCommand::Stress(args) => cmd_finance_stress(args).await,
        FinanceCommand::Fiscal(args) => cmd_finance_fiscal(args).await,
        FinanceCommand::Ecb(args) => cmd_finance_ecb(args).await,
        FinanceCommand::Eia(args) => cmd_finance_eia(args).await,
        FinanceCommand::Bis(args) => cmd_finance_bis(args).await,
        FinanceCommand::Boj(args) => cmd_finance_boj(args).await,
        FinanceCommand::Boe(args) => cmd_finance_boe(args).await,
    }
}

async fn cmd_legal(cmd: LegalCommand) -> Result<()> {
    match cmd {
        LegalCommand::Search(args) => cmd_legal_search(args).await,
        LegalCommand::Opinion(args) => cmd_legal_opinion(args).await,
        LegalCommand::Docket(args) => cmd_legal_docket(args).await,
        LegalCommand::Cite(args) => cmd_legal_cite(args).await,
        LegalCommand::Cfr(args) => cmd_legal_cfr(args).await,
        LegalCommand::Fedreg(args) => cmd_legal_fedreg(args).await,
        LegalCommand::Comments(args) => cmd_legal_comments(args).await,
        LegalCommand::Enforcement(args) => cmd_legal_enforcement(args).await,
        LegalCommand::Statute(args) => cmd_legal_statute(args).await,
        LegalCommand::Record(args) => cmd_legal_record(args).await,
        LegalCommand::Opinions(args) => cmd_legal_opinions(args).await,
        LegalCommand::Deadlines(args) => cmd_legal_deadlines(args).await,
        LegalCommand::Caps(args) => cmd_legal_caps(args).await,
        LegalCommand::Entity(args) => cmd_legal_entity(args).await,
    }
}
