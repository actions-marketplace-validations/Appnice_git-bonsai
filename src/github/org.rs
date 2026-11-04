//! Organizations: no contribution calendar, so everything comes from default-branch histories.
//!
//! - Repo list: the first page (100) arrives with the detection request (query 1 asks for both
//!   `user` and `organization`); further pages are cheap list queries.
//! - Commit counts per repo: `history { totalCount }` aliases (all-time and 365 days, both up to
//!   the end of the snapshot day), [`COUNT_REPOS`] repos per `nodes(ids:)` request, in parallel.
//! - Calendar: the top repos (the ones that get branches) have one `history(since, until)
//!   { totalCount }` alias per day over the last [`CALENDAR_DAYS`] days, [`DAY_ALIASES`] aliases
//!   per request, in parallel; the calendar is their sum. Exact counts, no paging cap (paging
//!   `nodes { committedDate }` took 10+ sequential requests for one busy repo). A top repo's
//!   `commits_28d` and `commits_today` come from the same counts; other repos have 0 there.
//! - Yearly counts (planting): `history(since, until)` aliases for the top repos, one request
//!   while under [`YEAR_ALIASES`] aliases. Finished years go to the year cache; the current year
//!   is the all-time count minus the finished years; the org's first year includes all earlier
//!   (imported) history.

use super::queries::{decode, not_found, timestamp};
use super::{ApiError, Client, FetchOptions, YearCache};
use crate::rules;
use crate::snapshot::{AccountKind, CalendarDay, RepoSignal, Snapshot};
use serde::Deserialize;
use std::collections::BTreeMap;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset};

/// Days of synthesised calendar: enough for a 21-day bloom streak and 60-day dormancy.
pub const CALENDAR_DAYS: i64 = 70;
/// Repos whose default-branch history makes an organization's calendar.
pub const ORG_CALENDAR_REPOS: usize = 7;
/// Day aliases per calendar request (2 repos × 70 days).
pub const DAY_ALIASES: usize = 140;
/// Repos per commit-count request (2 aliases each).
pub const COUNT_REPOS: usize = 60;
/// `history` aliases per yearly request.
pub const YEAR_ALIASES: usize = 60;
/// Repo list pages at most (10 000 repos).
const MAX_PAGES: usize = 100;

/// The organization fields shared by query 1 and the list query.
macro_rules! org_fields {
    () => {
        r#"login createdAt
    repositories(first: 100, after: $after, privacy: PUBLIC,
                 orderBy: {field: PUSHED_AT, direction: DESC}) {
      totalCount
      pageInfo { hasNextPage endCursor }
      nodes {
        id name isFork isArchived archivedAt stargazerCount
        primaryLanguage { name color }
      }
    }"#
    };
}

/// Further pages of an organization's repos.
pub const ORG_LIST: &str = concat!(
    "query($login: String!, $after: String) {\n  organization(login: $login) { ",
    org_fields!(),
    " }\n}"
);

/// Commit counts for a batch of repos.
pub const ORG_COUNTS: &str = r#"query($ids: [ID!]!, $s365: GitTimestamp!, $d1: GitTimestamp!) {
  nodes(ids: $ids) {
    ... on Repository {
      id
      defaultBranchRef { target { ... on Commit {
        all: history(until: $d1) { totalCount }
        y: history(since: $s365, until: $d1) { totalCount }
      } } }
    }
  }
}"#;

#[derive(Deserialize)]
struct OrgData {
    organization: Option<OrgNode>,
}

/// An organization with one page of repos.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgNode {
    login: String,
    created_at: String,
    repositories: RepoConn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoConn {
    total_count: u32,
    page_info: PageInfo,
    nodes: Vec<Option<RepoNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoNode {
    id: String,
    name: String,
    is_fork: bool,
    is_archived: bool,
    archived_at: Option<String>,
    stargazer_count: u64,
    primary_language: Option<LangNode>,
}

#[derive(Deserialize)]
struct LangNode {
    name: String,
    color: Option<String>,
}

fn ts(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn start_of(d: Date, offset: UtcOffset) -> OffsetDateTime {
    d.midnight().assume_offset(offset)
}

fn end_of(d: Date, offset: UtcOffset) -> OffsetDateTime {
    start_of(d, offset) + Duration::seconds(86_399)
}

/// A GraphQL string literal.
fn lit(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// `history { totalCount }` under `alias` in the default-branch target of a repo node.
fn total(node: Option<&serde_json::Value>, alias: &str) -> u64 {
    node.and_then(|r| r.get("defaultBranchRef"))
        .and_then(|r| r.get("target"))
        .and_then(|t| t.get(alias))
        .and_then(|h| h.get("totalCount"))
        .and_then(|c| c.as_u64())
        .unwrap_or(0)
}

/// Runs `f` on every item in its own scoped thread and returns the results in order.
fn parallel<T: Sync, R: Send>(
    items: &[T],
    f: impl Fn(usize, &T) -> Result<R, ApiError> + Sync,
) -> Result<Vec<R>, ApiError> {
    let results: Vec<Result<R, ApiError>> = std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(k, it)| s.spawn(move || f(k, it)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(ApiError::Decode("worker thread panicked".into())))
            })
            .collect()
    });
    results.into_iter().collect()
}

/// Fetches an organization's snapshot. `first` is the detection request's org node, if any.
pub fn fetch(
    client: &Client,
    login: &str,
    date: Date,
    opts: &FetchOptions,
    year_cache: &YearCache,
    first: Option<OrgNode>,
) -> Result<Snapshot, ApiError> {
    let offset = opts.offset;

    // Repo list.
    let mut page = match first {
        Some(p) => p,
        None => list_page(client, login, None, 1)?,
    };
    let org_login = page.login.clone();
    let created = timestamp(&page.created_at)?.to_offset(offset).date();
    let public_repos = page.repositories.total_count;
    let mut repos: Vec<RepoSignal> = Vec::new();
    for n in 2..=MAX_PAGES + 1 {
        for r in std::mem::take(&mut page.repositories.nodes)
            .into_iter()
            .flatten()
        {
            repos.push(RepoSignal {
                id: r.id,
                name: r.name,
                is_fork: r.is_fork,
                is_archived: r.is_archived,
                stars: r.stargazer_count,
                language: r.primary_language.as_ref().map(|l| l.name.clone()),
                color: r.primary_language.and_then(|l| l.color),
                commits_total: 0,
                commits_28d: 0,
                commits_today: 0,
                commits_365d: 0,
                commits_by_year: BTreeMap::new(),
                archived_at: r
                    .archived_at
                    .as_deref()
                    .map(timestamp)
                    .transpose()?
                    .map(|t| t.to_offset(offset).date()),
            });
        }
        let info = &page.repositories.page_info;
        match (&info.end_cursor, info.has_next_page) {
            (Some(c), true) => page = list_page(client, login, Some(c.clone()), n)?,
            _ => break,
        }
    }

    // Commit counts.
    let ids: Vec<String> = repos.iter().map(|r| r.id.clone()).collect();
    let chunks: Vec<&[String]> = ids.chunks(COUNT_REPOS).collect();
    let vars = |chunk: &[String]| {
        serde_json::json!({
            "ids": chunk,
            "s365": ts(start_of(date - Duration::days(364), offset)),
            "d1": ts(end_of(date, offset)),
        })
    };
    let counted = parallel(&chunks, |k, chunk| {
        client.query(&format!("org counts {}", k + 1), ORG_COUNTS, vars(chunk))
    })?;
    let mut counts: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    for data in &counted {
        for node in data
            .get("nodes")
            .and_then(|n| n.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(id) = node.get("id").and_then(|i| i.as_str()) {
                counts.insert(
                    id.to_string(),
                    (total(Some(node), "all"), total(Some(node), "y")),
                );
            }
        }
    }
    for r in &mut repos {
        let (all, y) = counts.get(&r.id).copied().unwrap_or((0, 0));
        r.commits_total = all;
        r.commits_365d = y;
    }

    // The biggest repos that get branches decide the calendar and the yearly counts (at most
    // ORG_CALENDAR_REPOS, so a daily run stays within the query budget).
    let top: Vec<String> = rules::select_branches(&repos, &org_login, &opts.config)
        .iter()
        .take(ORG_CALENDAR_REPOS)
        .map(|r| r.id.clone())
        .collect();

    let cal_from = (date - Duration::days(CALENDAR_DAYS - 1)).max(created);
    let mut span = Vec::new();
    let mut d = cal_from;
    while d <= date {
        span.push(d);
        d += Duration::days(1);
    }
    let this_year = date.year();
    let missing: Vec<i32> = (created.year()..this_year)
        .filter(|y| !year_cache.contains_key(&y.to_string()))
        .collect();
    // Calendar and yearly counts are independent: run them side by side.
    let (per_repo, fetched) = std::thread::scope(|s| {
        let years = s.spawn(|| year_counts(client, &top, &missing, created.year(), offset));
        let days = commit_days(client, &top, &span, offset);
        let years = years
            .join()
            .unwrap_or_else(|_| Err(ApiError::Decode("worker thread panicked".into())));
        (days, years)
    });
    let (per_repo, fetched) = (per_repo?, fetched?);
    let mut daily: BTreeMap<Date, u32> = span.iter().map(|d| (*d, 0)).collect();
    let since28 = date - Duration::days(27);
    for r in repos.iter_mut() {
        let Some(days) = per_repo.get(&r.id) else {
            continue;
        };
        for (d, c) in days {
            if let Some(v) = daily.get_mut(d) {
                *v = v.saturating_add(*c);
            }
        }
        r.commits_28d = days.range(since28..).map(|(_, c)| *c).sum();
        r.commits_today = days.get(&date).copied().unwrap_or(0);
    }

    // Yearly counts for the top repos.
    for r in repos.iter_mut().filter(|r| top.contains(&r.id)) {
        let mut by_year = BTreeMap::new();
        for y in created.year()..this_year {
            let key = y.to_string();
            let c = match year_cache.get(&key) {
                Some(cache) => cache.get(&r.id).copied().unwrap_or(0),
                None => fetched.get(&(r.id.clone(), y)).copied().unwrap_or(0),
            };
            if c > 0 {
                by_year.insert(key, c);
            }
        }
        let current = r.commits_total.saturating_sub(by_year.values().sum());
        if current > 0 {
            by_year.insert(this_year.to_string(), current);
        }
        r.commits_by_year = by_year;
    }

    repos.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    Ok(Snapshot {
        login: org_login,
        date,
        account_created: created,
        public_repos,
        repos,
        calendar: daily
            .into_iter()
            .map(|(date, count)| CalendarDay { date, count })
            .collect(),
        years: (created.year()..=this_year).collect(),
        kind: AccountKind::Org,
    })
}

fn list_page(
    client: &Client,
    login: &str,
    after: Option<String>,
    n: usize,
) -> Result<OrgNode, ApiError> {
    let data = client
        .query(
            &format!("org repos p{n}"),
            ORG_LIST,
            serde_json::json!({ "login": login, "after": after }),
        )
        .map_err(|e| not_found(e, login))?;
    let d: OrgData = decode(data)?;
    d.organization
        .ok_or_else(|| ApiError::UserNotFound(login.to_string()))
}

/// Commits per day on the default branch of each of `ids` over `days`.
fn commit_days(
    client: &Client,
    ids: &[String],
    days: &[Date],
    offset: UtcOffset,
) -> Result<BTreeMap<String, BTreeMap<Date, u32>>, ApiError> {
    let mut out = BTreeMap::new();
    if ids.is_empty() || days.is_empty() {
        return Ok(out);
    }
    let per_request = (DAY_ALIASES / days.len()).max(1);
    let indexed: Vec<(usize, &String)> = ids.iter().enumerate().collect();
    let chunks: Vec<&[(usize, &String)]> = indexed.chunks(per_request).collect();
    let results = parallel(&chunks, |k, chunk| {
        client.query(
            &format!("org calendar {}", k + 1),
            &day_query(chunk, days, offset),
            serde_json::json!({}),
        )
    })?;
    for (chunk, data) in chunks.iter().zip(&results) {
        for (i, id) in chunk.iter() {
            let node = data.get(format!("r{i}"));
            let per: BTreeMap<Date, u32> = days
                .iter()
                .enumerate()
                .map(|(k, d)| {
                    let c = total(node, &format!("d{k}"));
                    (*d, u32::try_from(c).unwrap_or(u32::MAX))
                })
                .collect();
            out.insert((*id).clone(), per);
        }
    }
    Ok(out)
}

/// One alias per repo and day: `rI: node(id) { … dK: history(since, until) { totalCount } }`.
pub fn day_query(chunk: &[(usize, &String)], days: &[Date], offset: UtcOffset) -> String {
    let mut q = String::from("query {\n");
    for (i, id) in chunk {
        let mut hist = String::new();
        for (k, d) in days.iter().enumerate() {
            hist.push_str(&format!(
                "d{k}: history(since: {}, until: {}) {{ totalCount }} ",
                lit(&ts(start_of(*d, offset))),
                lit(&ts(end_of(*d, offset)))
            ));
        }
        q.push_str(&format!(
            "  r{i}: node(id: {}) {{ ... on Repository {{ defaultBranchRef {{ target {{ ... on Commit {{ {hist}}} }} }} }} }}\n",
            lit(id)
        ));
    }
    q.push_str("}\n");
    q
}

/// Default-branch commits of `ids` per year in `years`. The account's first year also counts
/// everything before it (imported history).
fn year_counts(
    client: &Client,
    ids: &[String],
    years: &[i32],
    first_year: i32,
    offset: UtcOffset,
) -> Result<BTreeMap<(String, i32), u64>, ApiError> {
    let mut out = BTreeMap::new();
    if years.is_empty() || ids.is_empty() {
        return Ok(out);
    }
    let per_request = (YEAR_ALIASES / years.len()).max(1);
    let indexed: Vec<(usize, &String)> = ids.iter().enumerate().collect();
    let chunks: Vec<&[(usize, &String)]> = indexed.chunks(per_request).collect();
    let results = parallel(&chunks, |k, chunk| {
        let mut q = String::from("query {\n");
        for (i, id) in chunk.iter() {
            let mut hist = String::new();
            for y in years {
                let jan1 = Date::from_calendar_date(*y, Month::January, 1).unwrap_or(Date::MIN);
                let dec31 = Date::from_calendar_date(*y, Month::December, 31).unwrap_or(Date::MIN);
                let since = if *y == first_year {
                    String::new()
                } else {
                    format!("since: {}, ", lit(&ts(start_of(jan1, offset))))
                };
                hist.push_str(&format!(
                    "y{y}: history({since}until: {}) {{ totalCount }} ",
                    lit(&ts(end_of(dec31, offset)))
                ));
            }
            q.push_str(&format!(
                "  r{i}: node(id: {}) {{ ... on Repository {{ defaultBranchRef {{ target {{ ... on Commit {{ {hist}}} }} }} }} }}\n",
                lit(id)
            ));
        }
        q.push_str("}\n");
        client.query(&format!("org years {}", k + 1), &q, serde_json::json!({}))
    })?;
    for (chunk, data) in chunks.iter().zip(&results) {
        for (i, id) in chunk.iter() {
            let node = data.get(format!("r{i}"));
            for y in years {
                out.insert(((*id).clone(), *y), total(node, &format!("y{y}")));
            }
        }
    }
    Ok(out)
}
