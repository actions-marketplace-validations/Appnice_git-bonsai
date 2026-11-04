//! Incremental growth: one snapshot adds the missing days to the tree (RFC "Reguły wzrostu").
//!
//! Only the shape (`trunk`, `branches`) accumulates; `vitals` is recomputed for the last day.

use crate::config::{Config, ConfigError};
use crate::rules;
use crate::snapshot::{AccountKind, RepoSignal, Snapshot};
use crate::state::{Branch, HistoryEntry, PhaseKind, Side, TreeState, Trunk, Vitals, SCHEMA};
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, Duration};

/// Days a gap is replayed at most.
pub const MAX_REPLAY_DAYS: i64 = 30;
/// Days kept one by one in `history`; older entries roll up into months.
pub const HISTORY_DAYS: i64 = 400;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum GrowError {
    #[error("snapshot is for {snapshot}, state is for {state}")]
    LoginMismatch { state: String, snapshot: String },
    #[error("snapshot date {date} is before the tree was planted ({planted})")]
    BeforePlanted { date: Date, planted: Date },
    #[error("state schema {0} is not {SCHEMA}; load it with state::load to migrate")]
    Schema(u32),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

/// What one growth step sees.
struct Day<'a> {
    date: Date,
    repos: &'a [RepoSignal],
    /// The account's commits so far (Sapling threshold).
    commits_account: u64,
    streak: u32,
    idle: u32,
}

/// What one step produced.
struct StepOut {
    phase: PhaseKind,
    grew: BTreeMap<String, u32>,
}

/// Grows `state` by the days between `last_grown` and `snapshot.date` (at most the last 30,
/// each replayed with the calendar). Idempotent: an old or same-day snapshot changes nothing.
pub fn grow(state: &TreeState, snapshot: &Snapshot, cfg: &Config) -> Result<TreeState, GrowError> {
    cfg.validate()?;
    if state.schema != SCHEMA {
        return Err(GrowError::Schema(state.schema));
    }
    if !state.login.eq_ignore_ascii_case(&snapshot.login) {
        return Err(GrowError::LoginMismatch {
            state: state.login.clone(),
            snapshot: snapshot.login.clone(),
        });
    }
    if state.last_grown.is_some_and(|g| snapshot.date <= g) {
        return Ok(state.clone());
    }
    if snapshot.date < state.planted {
        return Err(GrowError::BeforePlanted {
            date: snapshot.date,
            planted: state.planted,
        });
    }
    let first_missing = match state.last_grown {
        Some(g) => g + Duration::days(1),
        None => state.planted,
    };
    let start = first_missing
        .max(snapshot.date - Duration::days(MAX_REPLAY_DAYS - 1))
        .max(state.planted);

    let calendar: BTreeMap<Date, u32> = snapshot
        .calendar
        .iter()
        .map(|c| (c.date, c.count))
        .collect();
    let commits_account = account_commits(snapshot, &snapshot.repos);
    let origin = idle_origin(snapshot, state.planted);
    let mut next = state.clone();
    let mut out = None;
    let mut day = start;
    while day <= snapshot.date {
        let d = Day {
            date: day,
            repos: &snapshot.repos,
            commits_account,
            streak: rules::streak(&calendar, day),
            idle: rules::idle_days(&calendar, day, origin),
        };
        out = Some(step(&mut next, &d, cfg));
        day += Duration::days(1);
    }
    finish(&mut next, snapshot, out, &calendar, cfg);
    Ok(next)
}

/// A new tree for `snapshot.login`, planted on the account's creation day and grown day by day
/// up to `snapshot.date` with the same caps, so an old account starts with a grown tree.
///
/// History per repo is estimated: each repo's commits of a year (`commits_by_year`) are spread
/// over that year's days in proportion to the contribution calendar. Repos are archived on their
/// `archived_at` day. The last day uses the snapshot's exact numbers.
pub fn plant(snapshot: &Snapshot, cfg: &Config) -> Result<TreeState, GrowError> {
    cfg.validate()?;
    let planted = snapshot.account_created;
    if snapshot.date < planted {
        return Err(GrowError::BeforePlanted {
            date: snapshot.date,
            planted,
        });
    }
    let seed = rules::seed(&snapshot.login, planted);
    let mut state = TreeState {
        schema: SCHEMA,
        login: snapshot.login.clone(),
        trunk: Trunk {
            height: rules::trunk_height(0),
            girth: rules::trunk_girth(0),
            bends: rules::trunk_bends(&seed, rules::trunk_height(0)),
        },
        seed,
        planted,
        last_grown: None,
        branches: Vec::new(),
        year_cache: BTreeMap::new(),
        history: Vec::new(),
        vitals: Vitals::default(),
        kind: snapshot.kind,
    };
    let calendar: BTreeMap<Date, u32> = snapshot
        .calendar
        .iter()
        .map(|c| (c.date, c.count))
        .collect();
    let est = Estimator::new(snapshot, &calendar);
    let mut repos = snapshot.repos.clone();
    let (mut streak, mut last_active) = (0u32, planted);
    // Days before the calendar starts (an org's calendar covers 70 days) count as active:
    // unknown activity must not make the simulated tree dormant.
    let origin = idle_origin(snapshot, planted);
    let mut day = planted;
    let mut i = 0usize;
    while day < snapshot.date {
        let count = calendar.get(&day).copied().unwrap_or(0);
        if count > 0 {
            streak += 1;
            last_active = day;
        } else if day < origin {
            streak = 0;
            last_active = day;
        } else {
            streak = 0;
        }
        for (k, r) in repos.iter_mut().enumerate() {
            let src = &snapshot.repos[k];
            r.commits_total = est.cum(k, i);
            r.commits_365d = est.cum(k, i) - if i >= 365 { est.cum(k, i - 365) } else { 0 };
            r.is_archived = src.archived_at.is_some_and(|a| a <= day);
        }
        let d = Day {
            date: day,
            repos: &repos,
            commits_account: account_commits(snapshot, &repos),
            streak,
            idle: (day - last_active).whole_days() as u32,
        };
        step(&mut state, &d, cfg);
        day += Duration::days(1);
        i += 1;
    }
    let d = Day {
        date: snapshot.date,
        repos: &snapshot.repos,
        commits_account: account_commits(snapshot, &snapshot.repos),
        streak: rules::streak(&calendar, snapshot.date),
        idle: rules::idle_days(&calendar, snapshot.date, origin),
    };
    let out = step(&mut state, &d, cfg);
    finish(&mut state, snapshot, Some(out), &calendar, cfg);
    Ok(state)
}

/// Whether branches grow per language: above [`rules::GROUP_ABOVE`] active repos, and a tree that
/// already grows per language stays so until [`rules::GROUP_BACK_AT`] or fewer.
fn is_grouped(state: &TreeState, repos: &[RepoSignal], cfg: &Config) -> bool {
    let active = rules::active_repos(repos, &state.login, cfg);
    let now = living(state).any(|b| b.repo_id.starts_with(rules::LANG_PREFIX));
    if now {
        active > rules::GROUP_BACK_AT
    } else {
        active > rules::GROUP_ABOVE
    }
}

/// One day of growth.
fn step(state: &mut TreeState, day: &Day, cfg: &Config) -> StepOut {
    let age = (day.date - state.planted).whole_days();
    // Trunk: age only, never shrinks.
    state.trunk.height = state.trunk.height.max(rules::trunk_height(age));
    state.trunk.girth = state.trunk.girth.max(rules::trunk_girth(age));
    state.trunk.bends = rules::trunk_bends(&state.seed, state.trunk.height);

    let phase = rules::phase(age, day.commits_account, day.streak, day.idle, cfg);

    // Many active repos: one branch per language. Switching drops the other kind of branch
    // without a scar (nothing was pruned, the tree changed how it counts).
    let grouped = is_grouped(state, day.repos, cfg);
    state
        .branches
        .retain(|b| b.pruned.is_some() || b.repo_id.starts_with(rules::LANG_PREFIX) == grouped);
    let virt;
    let repos: &[RepoSignal] = if grouped {
        virt = rules::group_by_language(day.repos, &state.login, cfg);
        &virt
    } else {
        day.repos
    };
    let by_id: BTreeMap<&str, &RepoSignal> = repos.iter().map(|r| (r.id.as_str(), r)).collect();

    // Refresh living branches; prune archived or deleted repos; drop excluded ones.
    let mut drop = BTreeSet::new();
    for (i, b) in state.branches.iter_mut().enumerate() {
        if b.pruned.is_some() {
            continue;
        }
        match by_id.get(b.repo_id.as_str()) {
            Some(r) => {
                b.name = r.name.clone();
                b.lang = r.language.clone();
                b.color = color_of(r, cfg);
                b.commits_total = b.commits_total.max(r.commits_total);
                if r.is_archived {
                    prune(b, day.date);
                } else if !rules::eligible(r, &state.login, cfg) {
                    drop.insert(i);
                }
            }
            // A missing repo was deleted (or made private); an empty list is a broken fetch.
            None if !repos.is_empty() => prune(b, day.date),
            None => {}
        }
    }
    remove_indices(&mut state.branches, &drop);

    let mut grew = BTreeMap::new();
    if phase != PhaseKind::Dormant {
        let selected = rules::select_branches(repos, &state.login, cfg);
        let pins = selected.iter().filter(|r| rules::pinned(r, cfg)).count();
        let cap = cfg.max_branches.max(pins);
        let want = selected.len().min(cap);
        // The trunk grows to hold a branch for every repo you work in.
        let fit = rules::trunk_height_for(want);
        if fit > state.trunk.height {
            state.trunk.height = fit;
            state.trunk.bends = rules::trunk_bends(&state.seed, state.trunk.height);
        }
        for repo in &selected {
            if living(state).any(|b| b.repo_id == repo.id) {
                continue;
            }
            if let Some(b) = new_branch(state, repo, &selected, &by_id, cap, want, cfg) {
                grew.insert(b.repo_id.clone(), 1);
                state.branches.push(b);
            }
        }
        let top = top_repo(state);
        for b in state.branches.iter_mut().filter(|b| b.pruned.is_none()) {
            let is_top = top.as_deref() == Some(b.repo_id.as_str());
            let pad_target = rules::pad_target(b.commits_total, is_top);
            if !is_top {
                b.pad = b.pad.min(rules::PAD_CAP_SIDE);
            }
            if grew.contains_key(&b.repo_id) {
                continue; // born today: the first cell is the day's growth
            }
            let mut units = cfg.max_growth_per_day;
            let mut gained = 0;
            let len_target = rules::branch_len(b.commits_total);
            while units > 0 && b.length < len_target {
                b.length += 1;
                units -= 1;
                gained += 1;
            }
            while units > 0 && b.pad < pad_target {
                b.pad += 1;
                units -= 1;
                gained += 1;
            }
            if gained > 0 {
                grew.insert(b.repo_id.clone(), gained);
            }
        }
    }
    sort_branches(&mut state.branches);
    let cells: u32 = grew.values().sum();
    if cells > 0 {
        state.history.push(HistoryEntry {
            date: ymd(day.date),
            cells_added: cells,
        });
    }
    StepOut { phase, grew }
}

/// Last-day bookkeeping: vitals, year cache, history roll-up, `last_grown`.
fn finish(
    state: &mut TreeState,
    snap: &Snapshot,
    out: Option<StepOut>,
    calendar: &BTreeMap<Date, u32>,
    cfg: &Config,
) {
    let year = snap.date.year();
    for y in snap.years.iter().filter(|y| **y < year) {
        let cache = state.year_cache.entry(y.to_string()).or_default();
        for r in &snap.repos {
            if let Some(c) = r.commits_by_year.get(&y.to_string()) {
                if *c > 0 {
                    cache.insert(r.id.clone(), *c);
                }
            }
        }
    }
    let virt = rules::group_by_language(&snap.repos, &state.login, cfg);
    let by_id: BTreeMap<&str, &RepoSignal> = snap
        .repos
        .iter()
        .chain(virt.iter())
        .map(|r| (r.id.as_str(), r))
        .collect();
    let (phase, grew) = match out {
        Some(o) => (o.phase, o.grew),
        None => (state.vitals.phase, BTreeMap::new()),
    };
    state.vitals = Vitals {
        phase,
        streak_days: rules::streak(calendar, snap.date),
        idle_days: rules::idle_days(calendar, snap.date, idle_origin(snap, state.planted)),
        commits_total: account_commits(snap, &snap.repos),
        stars_total: snap
            .repos
            .iter()
            .filter(|r| !r.is_fork)
            .map(|r| r.stars)
            .sum(),
        public_repos: snap.public_repos,
        activity_28d: living(state)
            .map(|b| {
                let c = by_id.get(b.repo_id.as_str()).map_or(0, |r| r.commits_28d);
                (b.repo_id.clone(), c)
            })
            .collect(),
        grew,
    };
    state.kind = snap.kind;
    state.history = rollup(std::mem::take(&mut state.history), snap.date);
    state.last_grown = Some(snap.date);
}

/// The account's commits: all owned repos for a user; for an org, the all-time default-branch
/// commits of its non-fork repos (a fork's history is upstream's).
fn account_commits(snap: &Snapshot, repos: &[RepoSignal]) -> u64 {
    repos
        .iter()
        .filter(|r| snap.kind == AccountKind::User || !r.is_fork)
        .map(|r| r.commits_total)
        .sum()
}

/// Where idle days count from when the calendar shows no activity: account creation for a
/// user (the calendar covers the whole history), the calendar's first day for an org.
fn idle_origin(snap: &Snapshot, planted: Date) -> Date {
    match (snap.kind, snap.calendar.first()) {
        (AccountKind::Org, Some(first)) => first.date.max(planted),
        (AccountKind::Org, None) => snap.date,
        (AccountKind::User, _) => planted,
    }
}

fn living(state: &TreeState) -> impl Iterator<Item = &Branch> {
    state.branches.iter().filter(|b| b.pruned.is_none())
}

fn prune(b: &mut Branch, date: Date) {
    b.pruned = Some(date);
    b.length = rules::PRUNED_LEN;
    b.pad = 0;
}

fn remove_indices(v: &mut Vec<Branch>, idx: &BTreeSet<usize>) {
    let mut i = 0;
    v.retain(|_| {
        let keep = !idx.contains(&i);
        i += 1;
        keep
    });
}

fn color_of(r: &RepoSignal, cfg: &Config) -> Option<String> {
    r.language
        .as_ref()
        .and_then(|l| cfg.colors.get(l).cloned())
        .or_else(|| r.color.clone())
}

/// The living branch with the most commits (the apex pad), ties by `repo_id`.
fn top_repo(state: &TreeState) -> Option<String> {
    living(state)
        .max_by(|a, b| {
            a.commits_total
                .cmp(&b.commits_total)
                .then(b.repo_id.cmp(&a.repo_id))
        })
        .map(|b| b.repo_id.clone())
}

/// A branch for `repo`, or `None` when the tree has no room. With the tree full, a living branch
/// that dropped out of the selection yields to `repo` if `repo` has at least twice its commits
/// over 365 days (hysteresis: ranks 7 and 8 do not swap every day); it goes without a scar.
fn new_branch(
    state: &mut TreeState,
    repo: &RepoSignal,
    selected: &[&RepoSignal],
    by_id: &BTreeMap<&str, &RepoSignal>,
    cap: usize,
    want: usize,
    cfg: &Config,
) -> Option<Branch> {
    let full = living(state).count() >= cap || free_slots(state, want).is_empty();
    if full {
        let victim = state
            .branches
            .iter()
            .enumerate()
            .filter(|(_, b)| b.pruned.is_none() && !selected.iter().any(|s| s.id == b.repo_id))
            .filter_map(|(i, b)| {
                let r = by_id.get(b.repo_id.as_str());
                if r.is_some_and(|r| rules::pinned(r, cfg)) {
                    return None;
                }
                let c = r.map_or(0, |r| r.commits_365d);
                (c.saturating_mul(2) <= repo.commits_365d).then_some((c, b.repo_id.clone(), i))
            })
            .min();
        let (_, _, i) = victim?;
        state.branches.remove(i);
        if living(state).count() >= cap || free_slots(state, want).is_empty() {
            return None;
        }
    }
    let nth = state
        .branches
        .iter()
        .filter(|b| b.repo_id == repo.id)
        .count();
    let anchor = choose_anchor(state, repo, nth, want)?;
    let side = choose_side(state, anchor, &repo.id, nth);
    Some(Branch {
        repo_id: repo.id.clone(),
        name: repo.name.clone(),
        lang: repo.language.clone(),
        color: color_of(repo, cfg),
        anchor,
        side,
        length: 1,
        pad: rules::PAD_MIN,
        commits_total: repo.commits_total,
        pruned: None,
    })
}

/// Anchor rows still free: a grid step from every living branch, not on a scar.
fn free_slots(state: &TreeState, want: usize) -> Vec<u32> {
    let (slots, step) = rules::anchor_grid(state.trunk.height, want);
    slots
        .into_iter()
        .filter(|s| {
            state.branches.iter().all(|b| {
                if b.pruned.is_some() {
                    b.anchor != *s
                } else {
                    b.anchor.abs_diff(*s) >= step
                }
            })
        })
        .collect()
}

/// Where a new branch leaves the trunk. The repo with the most commits takes the top slot;
/// the others go lower the bigger their pad (bigger pads lower), a re-opened repo goes next to
/// its scar; ties are broken by the seed.
fn choose_anchor(state: &TreeState, repo: &RepoSignal, nth: usize, want: usize) -> Option<u32> {
    let free = free_slots(state, want);
    if free.is_empty() {
        return None;
    }
    let (slots, _) = rules::anchor_grid(state.trunk.height, want);
    let top_slot = slots.last().copied();
    let is_top = living(state).all(|b| repo.commits_total > b.commits_total);
    let score: Vec<(u32, u32)> = if is_top && top_slot.is_some_and(|t| free.contains(&t)) {
        return top_slot;
    } else if let Some(scar) = state
        .branches
        .iter()
        .find(|b| b.repo_id == repo.id && b.pruned.is_some())
    {
        free.iter().map(|s| (s.abs_diff(scar.anchor), *s)).collect()
    } else {
        let mine = rules::pad_size(repo.commits_total);
        let top = top_repo(state);
        let others: Vec<&Branch> = living(state)
            .filter(|b| Some(&b.repo_id) != top.as_ref())
            .collect();
        let bigger = others
            .iter()
            .filter(|b| rules::pad_size(b.commits_total) > mine)
            .count() as u32;
        free.iter()
            .filter(|s| Some(**s) != top_slot || free.len() == 1)
            .map(|s| {
                let below = others.iter().filter(|b| b.anchor < *s).count() as u32;
                (below.abs_diff(bigger), *s)
            })
            .collect()
    };
    let best = score.iter().map(|(d, _)| *d).min()?;
    let ties: Vec<u32> = score
        .iter()
        .filter(|(d, _)| *d == best)
        .map(|(_, s)| *s)
        .collect();
    let mut r = rules::rng(&state.seed, &format!("slot:{}:{nth}", repo.id));
    ties.get(rules::pick(&mut r, ties.len())).copied()
}

/// Opposite side to the nearest living branch (alternating sides); random from the seed when
/// there is none or the two nearest disagree.
fn choose_side(state: &TreeState, anchor: u32, repo_id: &str, nth: usize) -> Side {
    let mut r = rules::rng(&state.seed, &format!("side:{repo_id}:{nth}"));
    let best = living(state).map(|b| b.anchor.abs_diff(anchor)).min();
    let near: BTreeSet<Side> = living(state)
        .filter(|b| Some(b.anchor.abs_diff(anchor)) == best)
        .map(|b| b.side)
        .collect();
    match (near.len(), near.first()) {
        (1, Some(Side::Left)) => Side::Right,
        (1, Some(Side::Right)) => Side::Left,
        _ => rules::coin_side(&mut r),
    }
}

/// Render order: anchor, then repo id.
pub fn sort_branches(b: &mut [Branch]) {
    b.sort_by(|x, y| x.anchor.cmp(&y.anchor).then(x.repo_id.cmp(&y.repo_id)));
}

/// Keeps the last 400 days as days, rolls older days into month sums (`2025-06`).
pub fn rollup(history: Vec<HistoryEntry>, today: Date) -> Vec<HistoryEntry> {
    let mut out: Vec<HistoryEntry> = Vec::with_capacity(history.len());
    for e in history {
        let old = e.date.len() == 10
            && Date::parse(&e.date, crate::snapshot::YMD)
                .is_ok_and(|d| (today - d).whole_days() >= HISTORY_DAYS);
        let key = if old || e.date.len() == 7 {
            e.date[..7].to_string()
        } else {
            e.date.clone()
        };
        match out.last_mut() {
            Some(last) if key.len() == 7 && last.date == key => last.cells_added += e.cells_added,
            _ => out.push(HistoryEntry {
                date: key,
                cells_added: e.cells_added,
            }),
        }
    }
    out
}

fn ymd(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Spreads each repo's yearly commits over the calendar days of that year.
struct Estimator {
    /// Per day index from `planted`: (year slot, fraction of that year's activity done by then).
    days: Vec<(usize, f64)>,
    /// Per repo: (commits before each year slot, commits in each year slot).
    repos: Vec<(Vec<u64>, Vec<u64>)>,
}

impl Estimator {
    fn new(snap: &Snapshot, calendar: &BTreeMap<Date, u32>) -> Estimator {
        let start = snap.account_created;
        let n = (snap.date - start).whole_days().max(0) as usize + 1;
        let by_year = snap.repos.iter().any(|r| !r.commits_by_year.is_empty());
        let first_year = start.year();
        // Without yearly data the whole history is one bucket.
        let slot_of = |d: Date| {
            if by_year {
                (d.year() - first_year) as usize
            } else {
                0
            }
        };
        let slots = slot_of(snap.date) + 1;
        let mut counts = vec![0u64; n];
        let mut slot_total = vec![0u64; slots];
        let mut slot_days = vec![0u64; slots];
        for (i, c) in counts.iter_mut().enumerate() {
            let d = start + Duration::days(i as i64);
            *c = calendar.get(&d).copied().unwrap_or(0) as u64;
            slot_total[slot_of(d)] += *c;
            slot_days[slot_of(d)] += 1;
        }
        let mut days = Vec::with_capacity(n);
        let (mut cur, mut acc, mut nd) = (usize::MAX, 0u64, 0u64);
        for (i, c) in counts.iter().enumerate() {
            let s = slot_of(start + Duration::days(i as i64));
            if s != cur {
                cur = s;
                acc = 0;
                nd = 0;
            }
            acc += c;
            nd += 1;
            let f = if slot_total[s] > 0 {
                acc as f64 / slot_total[s] as f64
            } else {
                nd as f64 / slot_days[s] as f64
            };
            days.push((s, f));
        }
        let repos = snap
            .repos
            .iter()
            .map(|r| {
                let per: Vec<u64> = (0..slots)
                    .map(|s| {
                        if by_year {
                            let y = (first_year + s as i32).to_string();
                            r.commits_by_year.get(&y).copied().unwrap_or(0)
                        } else {
                            r.commits_total
                        }
                    })
                    .collect();
                let mut before = Vec::with_capacity(slots);
                let mut sum = 0;
                for c in &per {
                    before.push(sum);
                    sum += c;
                }
                (before, per)
            })
            .collect();
        Estimator { days, repos }
    }

    /// Estimated commits of repo `k` by the end of day index `i`.
    fn cum(&self, k: usize, i: usize) -> u64 {
        let (Some((s, f)), Some((before, per))) = (self.days.get(i), self.repos.get(k)) else {
            return 0;
        };
        before[*s] + (per[*s] as f64 * f) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::CalendarDay;
    use time::macros::date;

    fn repo(id: &str, total: u64, c365: u64) -> RepoSignal {
        RepoSignal {
            id: id.into(),
            name: format!("name-{id}"),
            is_fork: false,
            is_archived: false,
            stars: 10,
            language: Some("Rust".into()),
            color: Some("#dea584".into()),
            commits_total: total,
            commits_28d: 5,
            commits_today: 1,
            commits_365d: c365,
            commits_by_year: BTreeMap::new(),
            archived_at: None,
        }
    }

    fn snap(date: Date, repos: Vec<RepoSignal>) -> Snapshot {
        let created = date!(2020 - 01 - 01);
        let mut calendar = Vec::new();
        let mut d = date - Duration::days(399);
        while d <= date {
            calendar.push(CalendarDay { date: d, count: 3 });
            d += Duration::days(1);
        }
        Snapshot {
            login: "octo".into(),
            date,
            account_created: created,
            public_repos: repos.len() as u32,
            repos,
            calendar,
            years: Vec::new(),
            kind: AccountKind::User,
        }
    }

    fn seedling(planted: Date, last: Date) -> TreeState {
        let seed = rules::seed("octo", planted);
        TreeState {
            schema: SCHEMA,
            login: "octo".into(),
            trunk: Trunk {
                height: 8,
                girth: 2,
                bends: vec![[0, 0]],
            },
            seed,
            planted,
            last_grown: Some(last),
            branches: Vec::new(),
            year_cache: BTreeMap::new(),
            history: Vec::new(),
            vitals: Vitals::default(),
            kind: AccountKind::User,
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn same_or_older_snapshot_changes_nothing() {
        let s = snap(date!(2026 - 10 - 31), vec![repo("R_1", 500, 100)]);
        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 30));
        let once = grow(&st, &s, &cfg()).unwrap();
        assert_ne!(once, st);
        let twice = grow(&once, &s, &cfg()).unwrap();
        assert_eq!(once, twice);
        let older = snap(date!(2026 - 10 - 20), vec![repo("R_1", 900, 100)]);
        assert_eq!(grow(&once, &older, &cfg()).unwrap(), once);
    }

    #[test]
    fn gap_is_replayed_day_by_day_up_to_30() {
        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 26));
        let s = snap(date!(2026 - 10 - 31), vec![repo("R_1", 1204, 100)]);
        let g = grow(&st, &s, &cfg()).unwrap();
        // day 1: born (1), days 2-5: +1 each
        assert_eq!(g.branches[0].length, 5);
        assert_eq!(g.history.len(), 5);
        assert_eq!(g.last_grown, Some(date!(2026 - 10 - 31)));

        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 01 - 01));
        let g = grow(&st, &s, &cfg()).unwrap();
        // 30 days replayed: 1 birth + 8 cells to length 9 + 2 pad rows to 5
        assert_eq!(g.branches[0].length, 9);
        assert_eq!(g.branches[0].pad, 5);
        assert_eq!(g.history.len(), 11);
        assert_eq!(g.history[0].date, "2026-10-02");
    }

    #[test]
    fn daily_cap_and_anti_farming() {
        let mut st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 01));
        let mut farm = repo("R_1", 10, 10);
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 02), vec![farm.clone()]),
            &cfg(),
        )
        .unwrap();
        assert_eq!(st.branches[0].length, 1);
        farm.commits_total += 10_000;
        farm.commits_today = 10_000;
        farm.commits_365d += 10_000;
        let before = st.branches[0].clone();
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 03), vec![farm.clone()]),
            &cfg(),
        )
        .unwrap();
        let b = &st.branches[0];
        assert!(b.length + b.pad - before.length - before.pad <= 1);
        assert_eq!(st.vitals.grew.get("R_1"), Some(&1));
        let c = Config {
            max_growth_per_day: 3,
            ..cfg()
        };
        let st2 = grow(&st, &snap(date!(2026 - 10 - 04), vec![farm]), &c).unwrap();
        assert_eq!(st2.branches[0].length, b.length + 3);
    }

    #[test]
    fn archived_repo_is_pruned_forever_and_reopening_grows_a_new_branch() {
        let mut st = seedling(date!(2010 - 01 - 01), date!(2026 - 01 - 01));
        st.trunk.height = 24;
        let r = repo("R_1", 1204, 100);
        let s = snap(date!(2026 - 10 - 01), vec![r.clone(), repo("R_2", 50, 10)]);
        st = grow(&st, &s, &cfg()).unwrap();
        let mut arch = r.clone();
        arch.is_archived = true;
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 02), vec![arch, repo("R_2", 50, 10)]),
            &cfg(),
        )
        .unwrap();
        let p = st.branches.iter().find(|b| b.repo_id == "R_1").unwrap();
        assert_eq!(
            (p.length, p.pad, p.pruned),
            (3, 0, Some(date!(2026 - 10 - 02)))
        );
        // unarchived: the scar stays, a new branch grows beside it
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 03), vec![r, repo("R_2", 50, 10)]),
            &cfg(),
        )
        .unwrap();
        let mine: Vec<&Branch> = st.branches.iter().filter(|b| b.repo_id == "R_1").collect();
        assert_eq!(mine.len(), 2);
        assert!(mine.iter().any(|b| b.pruned.is_some() && b.length == 3));
        assert!(mine.iter().any(|b| b.pruned.is_none() && b.length == 1));
        // deleted repo is pruned too
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 04), vec![repo("R_2", 50, 10)]),
            &cfg(),
        )
        .unwrap();
        assert!(st
            .branches
            .iter()
            .filter(|b| b.repo_id == "R_1")
            .all(|b| b.pruned.is_some()));
        // an empty repo list (broken fetch) prunes nothing
        let before = st.branches.clone();
        st = grow(&st, &snap(date!(2026 - 10 - 05), vec![]), &cfg()).unwrap();
        assert_eq!(
            st.branches.iter().filter(|b| b.pruned.is_some()).count(),
            before.iter().filter(|b| b.pruned.is_some()).count()
        );
    }

    #[test]
    fn every_active_repo_gets_a_branch_and_the_trunk_grows_to_fit() {
        let st = seedling(date!(2026 - 09 - 01), date!(2026 - 10 - 01));
        let repos: Vec<RepoSignal> = (0..10)
            .map(|i| repo(&format!("R_{i}"), 10 * (i + 1), 10 * (i + 1)))
            .collect();
        let st = grow(&st, &snap(date!(2026 - 10 - 02), repos), &cfg()).unwrap();
        let live: Vec<&Branch> = st.branches.iter().filter(|b| b.pruned.is_none()).collect();
        assert_eq!(live.len(), 10);
        assert_eq!(st.trunk.height, rules::trunk_height_for(10));
        let mut rows: Vec<u32> = live.iter().map(|b| b.anchor).collect();
        rows.sort_unstable();
        rows.dedup();
        assert_eq!(rows.len(), 10);
    }

    #[test]
    fn many_repos_grow_one_branch_per_language() {
        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 01));
        let langs = ["Rust", "Go", "Python"];
        let repos: Vec<RepoSignal> = (0..30)
            .map(|i| {
                let mut r = repo(&format!("R_{i}"), 10 + i, 10 + i);
                r.language = Some(langs[i as usize % 3].into());
                r
            })
            .collect();
        let st = grow(&st, &snap(date!(2026 - 10 - 02), repos.clone()), &cfg()).unwrap();
        let mut ids: Vec<&str> = st
            .branches
            .iter()
            .filter(|b| b.pruned.is_none())
            .map(|b| b.repo_id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, ["lang:Go", "lang:Python", "lang:Rust"]);
        assert!(st.branches.iter().all(|b| b.name == "10 repos"));
        // back to per-repo branches only at 10 active repos or fewer, without scars
        let few: Vec<RepoSignal> = repos.into_iter().take(11).collect();
        let st2 = grow(&st, &snap(date!(2026 - 10 - 03), few.clone()), &cfg()).unwrap();
        assert!(st2.branches.iter().all(|b| b.repo_id.starts_with("lang:")));
        let st3 = grow(
            &st2,
            &snap(date!(2026 - 10 - 04), few[..10].to_vec()),
            &cfg(),
        )
        .unwrap();
        assert!(st3.branches.iter().all(|b| b.pruned.is_none()));
        assert_eq!(st3.branches.len(), 10);
    }

    #[test]
    fn anchors_are_spaced_alternating_and_capped() {
        let mut st = seedling(date!(2010 - 01 - 01), date!(2026 - 01 - 01));
        st.trunk.height = 24;
        let repos: Vec<RepoSignal> = (0..10)
            .map(|i| repo(&format!("R_{i}"), 100 * (i + 1), 10 * (i + 1)))
            .collect();
        let cfg7 = Config {
            max_branches: 7,
            ..cfg()
        };
        let st = grow(&st, &snap(date!(2026 - 10 - 01), repos), &cfg7).unwrap();
        let live: Vec<&Branch> = st.branches.iter().filter(|b| b.pruned.is_none()).collect();
        assert_eq!(live.len(), 7);
        for w in live.windows(2) {
            assert!(w[1].anchor >= w[0].anchor + 3);
        }
        assert!(live
            .iter()
            .all(|b| b.pad <= 6 && b.length <= rules::BRANCH_MAX));
        assert!(live.iter().filter(|b| b.pad > 5).count() <= 1);
        // the top repo sits in the top slot
        let top = live.iter().max_by_key(|b| b.commits_total).unwrap();
        assert_eq!(top.anchor, 21);
        // neighbours alternate sides
        let lefts = live.iter().filter(|b| b.side == Side::Left).count();
        assert!((3..=4).contains(&lefts), "{lefts}");
    }

    #[test]
    fn dropped_repo_yields_only_to_a_much_busier_one() {
        let mut st = seedling(date!(2010 - 01 - 01), date!(2026 - 01 - 01));
        st.trunk.height = 24;
        let c = Config {
            max_branches: 2,
            ..cfg()
        };
        let a = repo("R_a", 100, 50);
        let b = repo("R_b", 100, 40);
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 01), vec![a.clone(), b.clone()]),
            &c,
        )
        .unwrap();
        let new = repo("R_c", 100, 60);
        st = grow(
            &st,
            &snap(date!(2026 - 10 - 02), vec![a.clone(), b.clone(), new]),
            &c,
        )
        .unwrap();
        assert!(st.branches.iter().any(|x| x.repo_id == "R_b"));
        let busy = repo("R_c", 100, 80);
        st = grow(&st, &snap(date!(2026 - 10 - 03), vec![a, b, busy]), &c).unwrap();
        let ids: Vec<&str> = st.branches.iter().map(|x| x.repo_id.as_str()).collect();
        assert!(ids.contains(&"R_c") && !ids.contains(&"R_b"), "{ids:?}");
    }

    #[test]
    fn dormant_tree_does_not_grow() {
        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 01));
        let mut s = snap(date!(2026 - 10 - 02), vec![repo("R_1", 500, 100)]);
        for c in &mut s.calendar {
            c.count = 0;
        }
        let g = grow(&st, &s, &cfg()).unwrap();
        assert_eq!(g.vitals.phase, PhaseKind::Dormant);
        assert!(g.branches.is_empty());
    }

    #[test]
    fn four_hundred_days_in_sequence() {
        let mut st = seedling(date!(2025 - 09 - 01), date!(2025 - 09 - 01));
        let mut d = date!(2025 - 09 - 02);
        let mut total = 0u64;
        for i in 0..400u64 {
            total += i % 7;
            let mut repos = vec![repo("R_1", total, total.min(365))];
            if i > 100 {
                repos.push(repo("R_2", (i - 100) * 3, (i - 100) * 3));
            }
            if i == 300 {
                repos[0].is_archived = true;
            }
            if i > 300 {
                repos.remove(0);
            }
            st = grow(&st, &snap(d, repos), &cfg()).unwrap();
            assert!(st.trunk.height >= 8 && st.trunk.height <= 24);
            for w in st.branches.windows(2) {
                assert!((w[0].anchor, &w[0].repo_id) < (w[1].anchor, &w[1].repo_id));
            }
            d += Duration::days(1);
        }
        assert_eq!(
            st.trunk.height,
            rules::trunk_height((d - Duration::days(1) - st.planted).whole_days())
        );
        let r1 = st.branches.iter().find(|b| b.repo_id == "R_1").unwrap();
        assert!(r1.pruned.is_some() && r1.length == 3);
        let r2 = st.branches.iter().find(|b| b.repo_id == "R_2").unwrap();
        assert_eq!(r2.length, rules::branch_len(r2.commits_total));
        assert!(st.history.iter().all(|h| h.date.len() == 10));
    }

    #[test]
    fn history_rolls_up_into_months() {
        let h = vec![
            HistoryEntry {
                date: "2024-05-01".into(),
                cells_added: 1,
            },
            HistoryEntry {
                date: "2024-05-20".into(),
                cells_added: 2,
            },
            HistoryEntry {
                date: "2024-06-02".into(),
                cells_added: 4,
            },
            HistoryEntry {
                date: "2026-10-30".into(),
                cells_added: 1,
            },
        ];
        let r = rollup(h, date!(2026 - 10 - 31));
        let got: Vec<(&str, u32)> = r.iter().map(|e| (e.date.as_str(), e.cells_added)).collect();
        assert_eq!(got, [("2024-05", 3), ("2024-06", 4), ("2026-10-30", 1)]);
        assert_eq!(rollup(r.clone(), date!(2026 - 10 - 31)), r);
    }

    fn history_snapshot() -> Snapshot {
        let date = date!(2026 - 10 - 31);
        let mut repos = vec![
            repo("R_old", 300, 0),
            repo("R_new", 1500, 900),
            repo("R_mid", 200, 50),
        ];
        repos[0].commits_by_year = [("2017".to_string(), 200), ("2018".to_string(), 100)].into();
        repos[0].is_archived = true;
        repos[0].archived_at = Some(date!(2019 - 06 - 01));
        repos[1].commits_by_year = [("2025".to_string(), 600), ("2026".to_string(), 900)].into();
        repos[2].commits_by_year = [("2022".to_string(), 150), ("2026".to_string(), 50)].into();
        let mut s = snap(date, repos);
        s.account_created = date!(2016 - 03 - 14);
        let mut calendar = Vec::new();
        let mut d = s.account_created;
        while d <= date {
            calendar.push(CalendarDay {
                date: d,
                count: (d.ordinal() % 3) as u32 + 1,
            });
            d += Duration::days(1);
        }
        s.calendar = calendar;
        s.years = (2016..=2026).collect();
        s
    }

    #[test]
    fn plant_grows_an_old_account_and_is_deterministic() {
        let s = history_snapshot();
        let a = plant(&s, &cfg()).unwrap();
        let b = plant(&s, &cfg()).unwrap();
        assert_eq!(a, b);
        assert_eq!(state_json(&a), state_json(&b));
        assert_eq!(a.planted, date!(2016 - 03 - 14));
        assert_eq!(a.seed, rules::seed("octo", a.planted));
        assert_eq!(a.trunk.height, 24);
        assert_eq!(a.trunk.girth, 4);
        let old = a.branches.iter().find(|b| b.repo_id == "R_old").unwrap();
        assert_eq!(old.pruned, Some(date!(2019 - 06 - 01)));
        let new = a.branches.iter().find(|b| b.repo_id == "R_new").unwrap();
        assert_eq!(new.length, rules::branch_len(1500));
        assert!(a.branches.iter().any(|b| b.repo_id == "R_mid"));
        assert_eq!(a.year_cache["2017"]["R_old"], 200);
        assert!(a.year_cache.contains_key("2016") && !a.year_cache.contains_key("2026"));
        assert!(a.history.iter().any(|h| h.date.len() == 7));
        // growing the planted tree the next day works and is idempotent
        let mut s2 = s.clone();
        s2.date = date!(2026 - 11 - 01);
        let g = grow(&a, &s2, &cfg()).unwrap();
        assert_eq!(grow(&g, &s2, &cfg()).unwrap(), g);
    }

    #[test]
    fn org_with_a_70_day_calendar_plants_a_grown_tree() {
        let mut s = history_snapshot();
        s.kind = AccountKind::Org;
        let from = s.date - Duration::days(69);
        s.calendar.retain(|c| c.date >= from);
        let mut fork = repo("R_fork", 99_999, 0);
        fork.is_fork = true;
        s.repos.push(fork);
        let a = plant(&s, &cfg()).unwrap();
        assert_eq!(a.kind, AccountKind::Org);
        // unknown activity before the calendar does not make the tree dormant for years
        let new = a.branches.iter().find(|b| b.repo_id == "R_new").unwrap();
        assert_eq!(new.length, rules::branch_len(1500));
        assert!(a.history.iter().any(|h| h.date.starts_with("2025")));
        // forks do not count towards an org's commits
        assert_eq!(a.vitals.commits_total, 300 + 1500 + 200);
        assert_eq!(a.vitals.phase, PhaseKind::Bloom); // every calendar day has commits
                                                      // a silent org is dormant, idle counted from the calendar's start
        for c in &mut s.calendar {
            c.count = 0;
        }
        let d = plant(&s, &cfg()).unwrap();
        assert_eq!(d.vitals.phase, PhaseKind::Dormant);
        assert_eq!(d.vitals.idle_days, 69);
    }

    fn state_json(s: &TreeState) -> String {
        crate::state::to_json(s)
    }

    #[test]
    fn errors() {
        let st = seedling(date!(2020 - 01 - 01), date!(2026 - 10 - 01));
        let mut s = snap(date!(2026 - 10 - 02), vec![]);
        s.login = "other".into();
        assert!(matches!(
            grow(&st, &s, &cfg()),
            Err(GrowError::LoginMismatch { .. })
        ));
        s.login = "OCTO".into();
        assert!(grow(&st, &s, &cfg()).is_ok());
        let bad = Config {
            max_branches: 0,
            ..cfg()
        };
        assert!(matches!(grow(&st, &s, &bad), Err(GrowError::Config(_))));
    }
}
