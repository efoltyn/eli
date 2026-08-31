# legal-search

Nine MCP tools for primary legal sources: case law, federal dockets, citation
verification, the CFR *with its history*, the Federal Register, rulemaking
comments, statutes, and agency enforcement.

Same binary and same architecture as `market-search` — `eli-core` fetches and
parses, the CLI is a thin JSON printer, the MCP server advertises a catalog.
The only difference is which catalog it advertises.

```bash
# the legal catalog only
claude mcp add legal-search -- legal-search mcp

# or, if you already have market-search installed
claude mcp add legal-search -- market-search mcp --profile legal

# both catalogs in one server (30 tools)
claude mcp add eli -- market-search mcp --profile all
```

`ELI_MCP_PROFILE=legal|finance|all` does the same thing as `--profile`.

## Why these tools exist

Every one of them earns its place by answering a question a web search
structurally cannot. Not "answers it better" — cannot.

| Tool | What it does that a search engine can't |
|---|---|
| `legal_cite` | Proves a citation is **not** real. Search engines index what exists and have no representation of an empty reporter page, so absence of results conflates fabricated / unpublished / not-digitized / not-indexed. This resolves the cite directly and distinguishes them — and catches the nastier case, a real reporter page attributed to the wrong case. |
| `legal_cfr` | Regulation text **as it read on a past date**, plus the amendment log and a diff between two dates. The open web has today's text only, and hands it to you with no sign that anything changed. |
| `legal_fedreg` | Tomorrow's Federal Register (public inspection: filed, legally effective, not yet published — nothing has indexed it yet, by construction), and exact counts over the whole corpus via facets. |
| `legal_docket` | Docket sheets. PACER charges per page and the aggregators paywall them, so search returns news coverage *about* a case instead of the record *of* it. |
| `legal_search` | Full-text search over ~10M opinions with court, date, judge and citation-count filters, plus boolean and proximity operators. |
| `legal_comments` | Individual public comments on a rulemaking — behind a JS app, mostly PDF attachments, crawler-blocked. |
| `legal_statute` | Statutory text resolved through GPO, with the HTML-error-page-served-as-200 failure mode detected rather than passed through as if it were the statute. |
| `legal_opinion` | The complete opinion as a payload you can pincite, not a summarizer's description of one. |
| `legal_enforcement` | The agencies' own records, rather than reporting about them. |

## State law

The federal tools are national; state coverage is deliberately narrow, and each
row was verified against the live source. `legal_statute --state`,
`legal_state_record` and `legal_state_opinions` say which states they cover and
name them in the error when you ask for one they don't.

| State | Statutes | Trial-court records | Unpublished opinions |
|---|---|---|---|
| Wisconsin | ✅ `docs.legis.wisconsin.gov` | ✅ **WCCA — the only one** | — |
| Massachusetts | ✅ JSON API, cleanest of the 20 states probed | — | — |
| Illinois | — | — | ✅ Rule 23 orders |
| Pennsylvania | — | — | ✅ Superior memoranda |
| Michigan | — | — | ✅ unpublished per curiams |
| New Jersey | — | — | ✅ unpublished appellate |

**Wisconsin is the only state where the loop closes end to end**: find the
traffic case, read the statute charged, pull the appellate law, verify the
citations. Every other state is missing the trial-court leg, which is where a
traffic ticket, a small-claims suit or a misdemeanor actually lives.

Why it's per-state and not a 50-state sweep, all measured rather than assumed:

- **No all-50-state statute source exists.** Open States and LegiScan carry
  bills, not codified code. Justia has the text but 403s a real client.
  OpenLaws is gated commercial.
- **No centralized traffic penalty or points data anywhere.** NHTSA's speed-law
  digest is a 2013 PDF; IIHS and GHSA publish HTML tables of speed limits only.
- **No shared court platform.** Michigan's API route, tested against nine other
  state judiciaries, 404s or 403s on every one. Tyler Odyssey covers many
  states' dockets but exposes nothing to adapt.
- **Trial records are walled almost everywhere.** Maryland's famously open case
  search sits behind a DataDome captcha and 403s every programmatic request.

### Handling of personal data

Trial-court records name private individuals. `legal_state_record` withholds
dates of birth unless you pass `include_dob`, and never emits one that the
source marks sealed. The per-case detail view behind Wisconsin's index is
captcha-gated; this tool does not attempt it and says so in `warnings`.

### State-source traps worth knowing

- **`statutes.capitol.texas.gov` is now an Angular shell that returns an
  identical 250,874-byte page for every path** — the real statute, `robots.txt`,
  and a nonsense URL all produce the same md5. A scraper against it returns
  200 OK forever while getting nothing. Verify by content, never by status code.
- **Massachusetts spells `/` inside a section code as `~`** (`7D1~2`, not
  `7D1/2`); `%2F` is rejected by IIS before the API sees it. A miss returns
  **400**, not 404, and a repealed section returns `Text: ""` with the repeal
  note in the heading — so an empty string must never be surfaced as law.
- **New Jersey's `filter[status]=1` is mandatory**, and omitting it does not
  fail loudly — it returns silently short pages. The sort field is
  `field_posted_date`, and `page[limit]` caps at 50.
- **Opinion feeds are a rolling window** of roughly the last 50-100 decisions
  per court. They are a watcher, not an archive: an empty result means "not
  recent", never "no such case". Use `legal_search` for the historical corpus.

## API keys

Everything works with no key at all. Two upgrades are worth taking:

| Env var | Unlocks | Cost |
|---|---|---|
| `COURTLISTENER_TOKEN` | Real rate limits on case law and dockets. Anonymous is 100/day and the per-user buckets (5/min, 50/hour, 125/day) key off **client IP**, so a shared or NAT'd address burns quota you never spent. Also unlocks docket entries, opinion records and the authoritative citation-lookup endpoint. | Free, instant: courtlistener.com/profile/api/ |
| `REGULATIONS_GOV_API_KEY` | Public comments. Without it `legal_comments` degrades to the key-free Federal Register view of the same docket and says so. | Free, instant: api.data.gov/signup/ |
| `SEC_USER_AGENT` | SEC hosts 403 any request whose User-Agent lacks a contact email. Not a rate limit — a UA content check. | Just your email |

`DEMO_KEY` is not a workaround for the api.data.gov ones: it is a ~10 req/hr
bucket shared by every anonymous caller on the internet and is permanently
exhausted.

`LEGAL_SEARCH_USER_AGENT` overrides the UA sent to every upstream — set it if
you run this at volume, so the contact address is yours.

## Things that will bite you

Measured, not assumed:

- **eCFR's floor is 2016-12-31.** Earlier dates 404 with the *same* message as a
  bad section number. For older text use the GPO annual CFR editions on govinfo
  (1996-present).
- **"Today" is usually not a valid eCFR issue date.** A title's latest issue
  lags the calendar by days to weeks. Omitting `--date` resolves the ceiling
  from the title rather than asking for today and 404ing.
- **Slice the CFR.** Title 17 whole is ~16 MB; `--part 240` is 3.7 MB; a single
  `--section` is about a kilobyte. Always pass the section you have.
- **Federal Register search totals saturate at 10000.** Use `--facet` for a true
  count.
- **Federal Register `docket_ids` is not the regulations.gov docket** — it is
  the agency's own release numbers. The bridge is
  `regulations_gov_document_id`.
- **CourtListener v4 is mostly authenticated.** `search`, `courts`, `people`,
  `positions` and `audio` read anonymously; opinions, clusters, dockets,
  docket-entries and citation-lookup are 401 without a token. Every tool here
  has a key-free path behind the gated one, and says in `warnings` what the
  token would add.
- **DOJ's press-release API ignores `sort=`** and pages oldest-first, so
  recency ordering happens client-side.

Every tool returns `warnings` and degrades rather than failing: a partly
answered question beats a hard error, and an honest gap beats a confident
blank.
