#![forbid(unsafe_code)]

//! `legal-search` — the legal-research face of the same binary.
//!
//! Identical CLI to `market-search`, except the MCP server advertises the
//! `legal_*` catalog by default instead of the finance one, so a user can
//! wire up legal tools without also handing their assistant 21 markets tools.
//! `market-search mcp --profile legal` is the equivalent for anyone who
//! already has the other binary installed.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var_os("ELI_MCP_PROFILE").is_none() {
        std::env::set_var("ELI_MCP_PROFILE", "legal");
    }
    eli_cli::run().await
}
