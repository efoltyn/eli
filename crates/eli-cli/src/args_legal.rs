// Legal research CLI surface. Kept in its own file so the legal tools can grow
// without turning args.rs into a 3000-line wall; included from lib.rs.

#[derive(Subcommand, Debug)]
enum LegalCommand {
    /// Full-text search of federal + state court opinions, with court/date/judge filters.
    Search(LegalSearchArgs),
    /// Fetch the full text of a single opinion by CourtListener id or by reporter citation.
    Opinion(LegalOpinionArgs),
    /// Federal docket sheet: parties, entries, and free RECAP documents.
    Docket(LegalDocketArgs),
    /// Verify citations and walk the citation network (cited-by / authorities).
    Cite(LegalCiteArgs),
    /// Code of Federal Regulations: current text, text as of a past date, and amendment history.
    Cfr(LegalCfrArgs),
    /// Federal Register: rules, proposed rules, notices, and tomorrow's public-inspection queue.
    #[command(name = "fedreg", visible_alias = "register")]
    Fedreg(LegalFedregArgs),
    /// Public comments and dockets on regulations.gov.
    Comments(LegalCommentsArgs),
    /// Agency enforcement actions (SEC, CFPB, DOJ).
    Enforcement(LegalEnforcementArgs),
    /// US Code sections and bill text via govinfo, or a state code with --state.
    Statute(LegalStatuteArgs),
    /// State trial-court case records — where traffic and small-claims cases actually live.
    Record(LegalRecordArgs),
    /// State appellate opinions from the state's own feed, including unpublished ones.
    Opinions(LegalOpinionsArgs),
    /// Case deadlines computed from statute — limitations, government notice, certificate of review.
    Deadlines(LegalDeadlinesArgs),
    /// Damages caps resolved by accrual date and filing date.
    Caps(LegalCapsArgs),
    /// Business entity and registered-agent lookup.
    Entity(LegalEntityArgs),
}

#[derive(clap::Args, Debug)]
pub struct LegalDeadlinesArgs {
    /// State code. Currently co.
    #[arg(long, default_value = "co")]
    pub state: String,
    /// Date of injury (YYYY-MM-DD).
    #[arg(long)]
    pub injury_date: String,
    /// motor_vehicle | general | medical | premises | product | other.
    #[arg(long, default_value = "general")]
    pub mechanism: String,
    /// Any public entity or public employee implicated (RTD, the city, Denver Health...).
    #[arg(long, default_value_t = false)]
    pub public_entity: bool,
    /// A licensed professional is a defendant (drives the certificate of review).
    #[arg(long, default_value_t = false)]
    pub professional_defendant: bool,
    /// Date suit was filed (YYYY-MM-DD), for post-commencement deadlines.
    #[arg(long)]
    pub filing_date: Option<String>,
    /// Date the complaint was served (YYYY-MM-DD).
    #[arg(long)]
    pub service_date: Option<String>,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalCapsArgs {
    /// State code. Currently co.
    #[arg(long, default_value = "co")]
    pub state: String,
    /// noneconomic | wrongful_death | solatium | dram_shop | medmal | all.
    #[arg(long, default_value = "all")]
    pub claim_type: String,
    /// When the claim accrued (YYYY-MM-DD).
    #[arg(long)]
    pub accrual_date: String,
    /// When suit was or will be filed (YYYY-MM-DD). Colorado keys off both dates.
    #[arg(long)]
    pub filing_date: Option<String>,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalEntityArgs {
    /// State code. Currently co.
    #[arg(long, default_value = "co")]
    pub state: String,
    /// Entity name (substring match).
    #[arg(long)]
    pub name: Option<String>,
    /// Exact entity id.
    #[arg(long)]
    pub id: Option<String>,
    /// Status filter, e.g. "Good Standing".
    #[arg(long)]
    pub status: Option<String>,
    /// Max results (default 10).
    #[arg(long, default_value = "10")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalRecordArgs {
    /// State code, e.g. wi. Only states with an open trial-court source are covered.
    #[arg(long)]
    pub state: String,
    /// County name, e.g. Milwaukee. Required for a date-range search.
    #[arg(long)]
    pub county: Option<String>,
    /// Case-type code: TR (traffic), SC (small claims), CV (civil), CM (criminal misdemeanor).
    #[arg(long)]
    pub case_type: Option<String>,
    /// Exact case number, e.g. "2024TR000321".
    #[arg(long)]
    pub case_no: Option<String>,
    /// Filed on or after (YYYY-MM-DD).
    #[arg(long)]
    pub after: Option<String>,
    /// Filed on or before (YYYY-MM-DD).
    #[arg(long)]
    pub before: Option<String>,
    /// Include dates of birth. Off by default: these are records about private
    /// individuals, and a DOB is almost never needed to answer the question.
    #[arg(long, default_value_t = false)]
    pub include_dob: bool,
    /// Max records (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalOpinionsArgs {
    /// State code: il, pa, mi, nj.
    #[arg(long)]
    pub state: String,
    /// Keyword filter.
    #[arg(long)]
    pub q: Option<String>,
    /// Only non-precedential dispositions — the ones a Published-only search hides.
    #[arg(long, default_value_t = false)]
    pub unpublished: bool,
    /// Max results (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalSearchArgs {
    /// Query. Supports boolean AND/OR/NOT, "exact phrases", and /s proximity.
    #[arg(long)]
    pub q: String,
    /// Result type: o=opinions, r=RECAP filings, d=dockets, oa=oral argument, p=judges.
    #[arg(long, default_value = "o")]
    pub kind: String,
    /// Court id filter, comma-separated (e.g. scotus,ca9,nysd).
    #[arg(long)]
    pub court: Option<String>,
    /// Filed on or after (YYYY-MM-DD).
    #[arg(long)]
    pub after: Option<String>,
    /// Filed on or before (YYYY-MM-DD).
    #[arg(long)]
    pub before: Option<String>,
    /// Judge name filter.
    #[arg(long)]
    pub judge: Option<String>,
    /// Only opinions cited more than N times (a crude significance filter).
    #[arg(long)]
    pub cited_gt: Option<u32>,
    /// Precedential status filter (Published, Unpublished, Errata, Separate, ...).
    #[arg(long)]
    pub status: Option<String>,
    /// Sort order (score desc, dateFiled desc, dateFiled asc, citeCount desc).
    #[arg(long)]
    pub order: Option<String>,
    /// Max results (default 20).
    #[arg(long, default_value = "20")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalOpinionArgs {
    /// CourtListener opinion id.
    #[arg(long)]
    pub id: Option<u64>,
    /// Reporter citation, e.g. "597 U.S. 1" or "410 U.S. 113".
    #[arg(long)]
    pub cite: Option<String>,
    /// Max characters of opinion text to return (default 60000).
    #[arg(long, default_value = "60000")]
    pub max_chars: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalDocketArgs {
    /// Court id (e.g. nysd, cacd, ca9).
    #[arg(long)]
    pub court: Option<String>,
    /// Docket number as filed, e.g. "1:22-cr-00673".
    #[arg(long)]
    pub number: Option<String>,
    /// CourtListener docket id (skips the lookup).
    #[arg(long)]
    pub id: Option<u64>,
    /// Free-text search over docket case names instead of an exact number.
    #[arg(long)]
    pub q: Option<String>,
    /// Skip the docket sheet entries and return only the case metadata.
    /// Entries are included by default — they are the reason to call this.
    #[arg(long, default_value_t = false)]
    pub no_entries: bool,
    /// Max docket entries to return (default 50).
    #[arg(long, default_value = "50")]
    pub limit: usize,
    /// Start at this entry number (docket sheets can run to thousands).
    #[arg(long)]
    pub offset: Option<usize>,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalCiteArgs {
    /// Free text to scan for citations — every cite found is verified.
    #[arg(long)]
    pub text: Option<String>,
    /// A single citation to verify, e.g. "576 U.S. 644".
    #[arg(long)]
    pub cite: Option<String>,
    /// Also return what cites this opinion (requires --id or a resolved cite).
    #[arg(long, default_value_t = false)]
    pub cited_by: bool,
    /// Also return what this opinion cites.
    #[arg(long, default_value_t = false)]
    pub authorities: bool,
    /// CourtListener opinion id for the network walk.
    #[arg(long)]
    pub id: Option<u64>,
    /// Max network results (default 25).
    #[arg(long, default_value = "25")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalCfrArgs {
    /// CFR title number (1-50).
    #[arg(long)]
    pub title: Option<u32>,
    /// Part, e.g. "240".
    #[arg(long)]
    pub part: Option<String>,
    /// Section, e.g. "240.10b5-1".
    #[arg(long)]
    pub section: Option<String>,
    /// Subpart filter.
    #[arg(long)]
    pub subpart: Option<String>,
    /// Point-in-time: text as it read on this date (YYYY-MM-DD). Defaults to current.
    #[arg(long)]
    pub date: Option<String>,
    /// Full-text search across the CFR instead of fetching a citation.
    #[arg(long)]
    pub q: Option<String>,
    /// Return the amendment history (every date this part changed) instead of text.
    #[arg(long, default_value_t = false)]
    pub history: bool,
    /// Return the hierarchy/table of contents instead of text.
    #[arg(long, default_value_t = false)]
    pub structure: bool,
    /// Compare the text on --date against --diff-date and report what changed.
    #[arg(long)]
    pub diff_date: Option<String>,
    /// Max characters of regulation text (default 80000).
    #[arg(long, default_value = "80000")]
    pub max_chars: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalFedregArgs {
    /// Full-text query.
    #[arg(long)]
    pub q: Option<String>,
    /// Document type filter: rule, proposed, notice, presidential.
    #[arg(long)]
    pub kind: Option<String>,
    /// Agency slug filter, comma-separated (e.g. securities-and-exchange-commission).
    #[arg(long)]
    pub agency: Option<String>,
    /// Published on or after (YYYY-MM-DD).
    #[arg(long)]
    pub after: Option<String>,
    /// Published on or before (YYYY-MM-DD).
    #[arg(long)]
    pub before: Option<String>,
    /// Regulations.gov docket id filter.
    #[arg(long)]
    pub docket: Option<String>,
    /// CFR title affected.
    #[arg(long)]
    pub cfr_title: Option<u32>,
    /// CFR part affected.
    #[arg(long)]
    pub cfr_part: Option<String>,
    /// Fetch one document by its Federal Register document number.
    #[arg(long)]
    pub doc: Option<String>,
    /// Include the full plain text of each document (expensive — use with a small --limit).
    #[arg(long, default_value_t = false)]
    pub text: bool,
    /// Return the public-inspection queue (filed, not yet published) instead of a search.
    #[arg(long, default_value_t = false)]
    pub inspection: bool,
    /// Return counts over time for the query instead of documents (daily|weekly|monthly|quarterly|yearly|agency|type).
    #[arg(long)]
    pub facet: Option<String>,
    /// Max characters of document text when --text is set (default 40000).
    #[arg(long, default_value = "40000")]
    pub max_chars: usize,
    /// Max results (default 20).
    #[arg(long, default_value = "20")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalCommentsArgs {
    /// Regulations.gov docket id, e.g. "FINCEN-2024-0006".
    #[arg(long)]
    pub docket: Option<String>,
    /// Full-text query across comments.
    #[arg(long)]
    pub q: Option<String>,
    /// Agency id filter, e.g. SEC, EPA, CFPB.
    #[arg(long)]
    pub agency: Option<String>,
    /// Posted on or after (YYYY-MM-DD).
    #[arg(long)]
    pub after: Option<String>,
    /// Fetch one comment by its regulations.gov id (returns full text).
    #[arg(long)]
    pub id: Option<String>,
    /// Fetch the full text of every comment returned, not just the metadata.
    #[arg(long, default_value_t = false)]
    pub text: bool,
    /// Max results (default 25).
    #[arg(long, default_value = "25")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalEnforcementArgs {
    /// Source: sec | cfpb | doj | all (default all).
    #[arg(long, default_value = "all")]
    pub source: String,
    /// Keyword filter.
    #[arg(long)]
    pub q: Option<String>,
    /// On or after (YYYY-MM-DD).
    #[arg(long)]
    pub after: Option<String>,
    /// Max results per source (default 20).
    #[arg(long, default_value = "20")]
    pub limit: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct LegalStatuteArgs {
    /// State code (e.g. wi, ma). Switches from the US Code to that state's code.
    #[arg(long)]
    pub state: Option<String>,
    /// Chapter, for states that address law as chapter + section (e.g. MA ch. 90).
    #[arg(long)]
    pub chapter: Option<String>,
    /// US Code title, e.g. 15.
    #[arg(long)]
    pub title: Option<u32>,
    /// US Code section, e.g. "78j".
    #[arg(long)]
    pub section: Option<String>,
    /// Congress number for bill lookups, e.g. 119.
    #[arg(long)]
    pub congress: Option<u32>,
    /// Bill id, e.g. "hr3076".
    #[arg(long)]
    pub bill: Option<String>,
    /// Max characters of statutory text (default 60000).
    #[arg(long, default_value = "60000")]
    pub max_chars: usize,
    #[arg(long, default_value = "json")]
    pub format: String,
    #[arg(long)]
    pub out: Option<PathBuf>,
}
