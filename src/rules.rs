//! Signal → tree parameter rules (RFC "Model domenowy"), one pure function each.
//!
//! Integer arithmetic only, so every platform computes the same tree (`f64::log2` may differ in
//! the last ulp between libms).

use crate::config::Config;
use crate::snapshot::RepoSignal;
use crate::state::{PhaseKind, Side};
use rand_chacha::ChaCha8Rng;
use rand_core::{RngCore, SeedableRng};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use time::Date;

/// Shortest and longest living branch, in cells. 9 keeps a 5-pad (rx 8) inside the 36-wide grid.
pub const BRANCH_MIN: u32 = 3;
pub const BRANCH_MAX: u32 = 9;
/// Pad rows per the RFC (3–7); layout caps: 5 for side pads, 6 for the apex (top repo).
pub const PAD_MIN: u32 = 3;
pub const PAD_MAX: u32 = 7;
pub const PAD_CAP_SIDE: u32 = 5;
pub const PAD_CAP_TOP: u32 = 6;
/// Length of a pruned stub.
pub const PRUNED_LEN: u32 = 3;
/// Lowest anchor row and the spacing between anchors.
pub const ANCHOR_MIN: u32 = 3;
pub const ANCHOR_STEP: u32 = 3;
/// Spacing of the tight grid used when more repos want a branch than the regular grid holds:
/// one branch per trunk row, sides alternating.
pub const ANCHOR_STEP_DENSE: u32 = 1;
/// Most branches a tree can hold (rows 2..=21 of the tallest trunk).
pub const MAX_BRANCHES: usize = 20;

/// `floor(log2(1 + n))`.
pub fn ilog2_1p(n: u64) -> u32 {
    n.saturating_add(1).ilog2()
}

/// Whole years in `days` (365.25-day years).
pub fn years(days: i64) -> u32 {
    (days.max(0) * 4 / 1461) as u32
}

/// Trunk height in cells: `clamp(8 + years × 1.6, 8, 24)`, years as a fraction of days.
pub fn trunk_height(age_days: i64) -> u32 {
    // 1.6 / 365.25 = 32 / 7305
    let h = 8 + age_days.max(0) * 32 / 7305;
    h.clamp(8, 24) as u32
}

/// Trunk girth 2–5 by account age: <2 y → 2, <6 y → 3, <12 y → 4, older → 5.
pub fn trunk_girth(age_days: i64) -> u32 {
    match years(age_days) {
        0..=1 => 2,
        2..=5 => 3,
        6..=11 => 4,
        _ => 5,
    }
}

/// `size = log2(1 + commits)`, integer part.
pub fn branch_size(commits: u64) -> u32 {
    ilog2_1p(commits)
}

/// Target branch length: `size − 1`, clamped to 3..=9 (1204 commits → 9).
pub fn branch_len(commits: u64) -> u32 {
    branch_size(commits)
        .saturating_sub(1)
        .clamp(BRANCH_MIN, BRANCH_MAX)
}

/// Pad rows from the RFC: `round(size / 2)`, clamped to 3..=7 (1204 commits → 5).
pub fn pad_size(commits: u64) -> u32 {
    (branch_size(commits).div_ceil(2)).clamp(PAD_MIN, PAD_MAX)
}

/// The pad a branch grows to: [`pad_size`] capped at 6 for the apex repo, 5 for the others.
pub fn pad_target(commits: u64, is_top: bool) -> u32 {
    pad_size(commits).min(if is_top { PAD_CAP_TOP } else { PAD_CAP_SIDE })
}

/// Consecutive days with a contribution ending at `date` (0 when `date` itself has none).
pub fn streak(calendar: &BTreeMap<Date, u32>, date: Date) -> u32 {
    let mut n = 0;
    let mut d = date;
    while calendar.get(&d).copied().unwrap_or(0) > 0 {
        n += 1;
        match d.previous_day() {
            Some(p) => d = p,
            None => break,
        }
    }
    n
}

/// Days since the last contribution on or before `date`; with none in the calendar, days since
/// the account was created.
pub fn idle_days(calendar: &BTreeMap<Date, u32>, date: Date, created: Date) -> u32 {
    let last = calendar
        .range(..=date)
        .rev()
        .find(|(_, c)| **c > 0)
        .map(|(d, _)| *d)
        .unwrap_or(created);
    (date - last).whole_days().max(0) as u32
}

/// Phase of a day: `Sapling` (account < 30 days or < 10 commits), `Dormant` (idle ≥ threshold),
/// `Bloom` (streak ≥ threshold), else `Growing`.
pub fn phase(
    age_days: i64,
    commits_total: u64,
    streak_days: u32,
    idle_days: u32,
    cfg: &Config,
) -> PhaseKind {
    if age_days < 30 || commits_total < 10 {
        PhaseKind::Sapling
    } else if idle_days >= cfg.dormant_after_days {
        PhaseKind::Dormant
    } else if streak_days >= cfg.bloom_streak_days {
        PhaseKind::Bloom
    } else {
        PhaseKind::Growing
    }
}

/// Pot width 16–24 cells, log scale of public repos: `16 + log2(1 + repos)` (255+ → 24).
pub fn pot_width(public_repos: u32) -> u32 {
    (16 + ilog2_1p(public_repos as u64)).min(24)
}

/// Moss cells: one per 50 stars, at most 6.
pub fn moss(stars: u64) -> u32 {
    (stars / 50).min(6) as u32
}

/// Case-insensitive glob with `*` and `?`.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// May this repo carry a branch at all? Not a fork, not archived, not the profile repo
/// (`login/login`), not excluded.
pub fn eligible(repo: &RepoSignal, login: &str, cfg: &Config) -> bool {
    !repo.is_fork
        && !repo.is_archived
        && !repo.name.eq_ignore_ascii_case(login)
        && !cfg.exclude.iter().any(|g| glob_match(g, &repo.name))
}

/// More active repos than this and branches grow per language instead of per repo; a grouped
/// tree goes back to repos only at [`GROUP_BACK_AT`] or fewer (no flip-flop around one number).
pub const GROUP_ABOVE: usize = 12;
pub const GROUP_BACK_AT: usize = 10;
/// Id prefix of a language branch (`lang:Python`, `lang:` for repos without a language).
pub const LANG_PREFIX: &str = "lang:";

/// Eligible repos with commits in the last 365 days.
pub fn active_repos(repos: &[RepoSignal], login: &str, cfg: &Config) -> usize {
    repos
        .iter()
        .filter(|r| eligible(r, login, cfg) && r.commits_365d > 0)
        .count()
}

/// One virtual repo per language over the eligible repos: commits summed, named `N repos`, the
/// language and colour kept. Repos without a language form one group too.
pub fn group_by_language(repos: &[RepoSignal], login: &str, cfg: &Config) -> Vec<RepoSignal> {
    let mut groups: BTreeMap<String, (RepoSignal, u32)> = BTreeMap::new();
    for r in repos.iter().filter(|r| eligible(r, login, cfg)) {
        let key = r.language.clone().unwrap_or_default();
        let (g, n) = groups.entry(key.clone()).or_insert_with(|| {
            (
                RepoSignal {
                    id: format!("{LANG_PREFIX}{key}"),
                    name: String::new(),
                    is_fork: false,
                    is_archived: false,
                    stars: 0,
                    language: r.language.clone(),
                    color: r.color.clone(),
                    commits_total: 0,
                    commits_28d: 0,
                    commits_today: 0,
                    commits_365d: 0,
                    commits_by_year: BTreeMap::new(),
                    archived_at: None,
                },
                0,
            )
        });
        g.stars += r.stars;
        g.commits_total += r.commits_total;
        g.commits_28d += r.commits_28d;
        g.commits_today += r.commits_today;
        g.commits_365d += r.commits_365d;
        if r.commits_365d > 0 {
            *n += 1;
        }
    }
    groups
        .into_values()
        .map(|(mut g, n)| {
            g.name = if n == 1 {
                "1 repo".into()
            } else {
                format!("{n} repos")
            };
            g
        })
        .collect()
}

/// Is this repo pinned in the config?
pub fn pinned(repo: &RepoSignal, cfg: &Config) -> bool {
    cfg.pin.iter().any(|p| p.eq_ignore_ascii_case(&repo.name))
}

/// The repos that should carry branches, best first: pinned eligible repos, then the top
/// eligible repos by commits in the last 365 days (> 0), up to `max_branches` in total (pins
/// may exceed it). Ties: more commits overall, then id.
pub fn select_branches<'a>(
    repos: &'a [RepoSignal],
    login: &str,
    cfg: &Config,
) -> Vec<&'a RepoSignal> {
    let mut pins: Vec<&RepoSignal> = repos
        .iter()
        .filter(|r| eligible(r, login, cfg) && pinned(r, cfg))
        .collect();
    pins.sort_by(|a, b| rank(a, b));
    let mut rest: Vec<&RepoSignal> = repos
        .iter()
        .filter(|r| eligible(r, login, cfg) && !pinned(r, cfg) && r.commits_365d > 0)
        .collect();
    rest.sort_by(|a, b| rank(a, b));
    let room = cfg.max_branches.saturating_sub(pins.len());
    pins.extend(rest.into_iter().take(room));
    pins
}

fn rank(a: &RepoSignal, b: &RepoSignal) -> std::cmp::Ordering {
    b.commits_365d
        .cmp(&a.commits_365d)
        .then(b.commits_total.cmp(&a.commits_total))
        .then(a.id.cmp(&b.id))
}

/// `sha256(login + planted)`, first 8 bytes as 16 hex chars.
pub fn seed(login: &str, planted: Date) -> String {
    let d = Sha256::digest(format!("{login}{}", ymd(planted)).as_bytes());
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// The RNG for one element of the tree: `ChaCha8Rng` seeded with `sha256(seed + ":" + element)`.
pub fn rng(seed: &str, element: &str) -> ChaCha8Rng {
    let d = Sha256::digest(format!("{seed}:{element}").as_bytes());
    let mut s = [0u8; 32];
    s.copy_from_slice(&d);
    ChaCha8Rng::from_seed(s)
}

/// Uniform pick in `0..n` (n > 0) from the RNG; modulo bias is irrelevant at these sizes.
pub fn pick(rng: &mut ChaCha8Rng, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        (rng.next_u64() % n as u64) as usize
    }
}

/// A random side.
pub fn coin_side(rng: &mut ChaCha8Rng) -> Side {
    if rng.next_u32() & 1 == 0 {
        Side::Left
    } else {
        Side::Right
    }
}

/// Trunk control points `[row, x offset]` for a trunk of `height`: from the seed, a point every
/// 5–6 rows, offsets −1..=1 changing by at most 1. Stable as the trunk grows: a taller trunk only
/// adds points on top.
pub fn trunk_bends(seed: &str, height: u32) -> Vec<[i32; 2]> {
    let mut r = rng(seed, "trunk");
    let mut out = vec![[0, 0]];
    let (mut row, mut off) = (0i32, 0i32);
    loop {
        row += 5 + (r.next_u32() % 2) as i32;
        off = (off + (r.next_u32() % 3) as i32 - 1).clamp(-1, 1);
        if row >= height as i32 {
            break;
        }
        out.push([row, off]);
    }
    out
}

/// Anchor rows a trunk of `height` offers, bottom up: every 3 rows from row 3 up to
/// `height − 6` (the lower two thirds, where side pads have room), plus one top slot at
/// `height − 3` for the apex repo when it is at least 3 rows above the last one.
pub fn anchor_slots(height: u32) -> Vec<u32> {
    let mut v = Vec::new();
    let mut r = ANCHOR_MIN;
    while r + 6 <= height {
        v.push(r);
        r += ANCHOR_STEP;
    }
    let top = height.saturating_sub(3);
    if v.last().is_none_or(|l| top >= l + ANCHOR_STEP) && top >= ANCHOR_MIN {
        v.push(top);
    }
    v
}

/// Anchor rows and their spacing for `want` branches. The regular grid when it has room for
/// them; otherwise one branch per row from row 2 to `height − 3`, so every active repo gets a
/// branch (the trunk grows to fit them, see [`trunk_height_for`]).
pub fn anchor_grid(height: u32, want: usize) -> (Vec<u32>, u32) {
    let v = anchor_slots(height);
    if v.len() >= want {
        return (v, ANCHOR_STEP);
    }
    let dense: Vec<u32> = (2..=height.saturating_sub(3)).collect();
    if dense.len() > v.len() {
        (dense, ANCHOR_STEP_DENSE)
    } else {
        (v, ANCHOR_STEP)
    }
}

/// The trunk height that holds `want` branches: the trunk grows with the number of repos you
/// work in, not only with the account's age.
pub fn trunk_height_for(want: usize) -> u32 {
    if anchor_slots(8).len() >= want {
        8
    } else {
        (want as u32 + 4).clamp(8, 24)
    }
}

fn ymd(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::date;

    fn repo(name: &str, c365: u64) -> RepoSignal {
        RepoSignal {
            id: format!("R_{name}"),
            name: name.into(),
            is_fork: false,
            is_archived: false,
            stars: 0,
            language: None,
            color: None,
            commits_total: c365,
            commits_28d: 0,
            commits_today: 0,
            commits_365d: c365,
            commits_by_year: BTreeMap::new(),
            archived_at: None,
        }
    }

    #[test]
    fn trunk_height_table() {
        for (days, h) in [
            (0, 8),
            (365, 9),
            (1461, 14),
            (3652, 23),
            (3653, 24),
            (3884, 24),
            (8000, 24),
            (-5, 8),
        ] {
            assert_eq!(trunk_height(days), h, "days {days}");
        }
    }

    #[test]
    fn girth_table() {
        for (days, g) in [
            (0, 2),
            (700, 2),
            (731, 3),
            (2190, 3),
            (2192, 4),
            (3884, 4),
            (4383, 5),
        ] {
            assert_eq!(trunk_girth(days), g, "days {days}");
        }
    }

    #[test]
    fn branch_len_table() {
        for (c, l) in [
            (0, 3),
            (1, 3),
            (15, 3),
            (31, 4),
            (100, 5),
            (1204, 9),
            (10_000, 9),
            (u64::MAX, 9),
        ] {
            assert_eq!(branch_len(c), l, "commits {c}");
        }
    }

    #[test]
    fn ten_times_commits_is_about_three_cells() {
        assert_eq!(branch_size(1000) - branch_size(100), 3);
    }

    #[test]
    fn pad_table() {
        for (c, p) in [
            (0, 3),
            (31, 3),
            (63, 3),
            (127, 4),
            (1204, 5),
            (5000, 6),
            (20_000, 7),
        ] {
            assert_eq!(pad_size(c), p, "commits {c}");
        }
        assert_eq!(pad_target(20_000, false), 5);
        assert_eq!(pad_target(20_000, true), 6);
        assert_eq!(pad_target(10, true), 3);
    }

    #[test]
    fn streak_and_idle() {
        let mut cal = BTreeMap::new();
        for (i, c) in [0, 3, 1, 2, 0, 5, 5].iter().enumerate() {
            cal.insert(date!(2026 - 10 - 01) + time::Duration::days(i as i64), *c);
        }
        assert_eq!(streak(&cal, date!(2026 - 10 - 07)), 2);
        assert_eq!(streak(&cal, date!(2026 - 10 - 04)), 3);
        assert_eq!(streak(&cal, date!(2026 - 10 - 05)), 0);
        assert_eq!(
            idle_days(&cal, date!(2026 - 10 - 05), date!(2020 - 01 - 01)),
            1
        );
        assert_eq!(
            idle_days(&cal, date!(2026 - 10 - 09), date!(2020 - 01 - 01)),
            2
        );
        let empty = BTreeMap::new();
        assert_eq!(
            idle_days(&empty, date!(2026 - 10 - 09), date!(2026 - 10 - 01)),
            8
        );
    }

    #[test]
    fn phase_table() {
        let c = Config::default();
        for (age, commits, streak, idle, p) in [
            (10, 500, 30, 0, PhaseKind::Sapling),
            (400, 9, 30, 0, PhaseKind::Sapling),
            (400, 500, 0, 0, PhaseKind::Growing),
            (400, 500, 20, 0, PhaseKind::Growing),
            (400, 500, 21, 0, PhaseKind::Bloom),
            (400, 500, 0, 59, PhaseKind::Growing),
            (400, 500, 0, 60, PhaseKind::Dormant),
        ] {
            assert_eq!(
                phase(age, commits, streak, idle, &c),
                p,
                "{age} {commits} {streak} {idle}"
            );
        }
        let c = Config {
            bloom_streak_days: 7,
            dormant_after_days: 14,
            ..Config::default()
        };
        assert_eq!(phase(400, 500, 7, 0, &c), PhaseKind::Bloom);
        assert_eq!(phase(400, 500, 0, 14, &c), PhaseKind::Dormant);
    }

    #[test]
    fn pot_and_moss_tables() {
        for (n, w) in [
            (0, 16),
            (1, 17),
            (3, 18),
            (30, 20),
            (300, 24),
            (100_000, 24),
        ] {
            assert_eq!(pot_width(n), w, "repos {n}");
        }
        for (s, m) in [(0, 0), (49, 0), (50, 1), (149, 2), (300, 6), (10_000, 6)] {
            assert_eq!(moss(s), m, "stars {s}");
        }
    }

    #[test]
    fn globs() {
        for (p, n, ok) in [
            ("dotfiles", "dotfiles", true),
            ("dotfiles", "DotFiles", true),
            ("*-old", "lib-old", true),
            ("*-old", "lib-older", false),
            ("*.github.io", "x.github.io", true),
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("*", "", true),
            ("a*b*c", "axxbyyc", true),
        ] {
            assert_eq!(glob_match(p, n), ok, "{p} {n}");
        }
    }

    #[test]
    fn selection_top_n_pins_excludes() {
        let mut repos: Vec<RepoSignal> = (0..10).map(|i| repo(&format!("r{i}"), 10 + i)).collect();
        repos.push(repo("me", 1000)); // profile repo
        let mut fork = repo("fork", 999);
        fork.is_fork = true;
        repos.push(fork);
        let mut arch = repo("arch", 998);
        arch.is_archived = true;
        repos.push(arch);
        repos.push(repo("quiet", 0));
        repos.push(repo("dotfiles", 997));
        let cfg = Config {
            exclude: vec!["dot*".into()],
            pin: vec!["quiet".into()],
            max_branches: 7,
            ..Config::default()
        };
        let names: Vec<&str> = select_branches(&repos, "me", &cfg)
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(names, ["quiet", "r9", "r8", "r7", "r6", "r5", "r4"]);
        let cfg = Config {
            max_branches: 2,
            ..Config::default()
        };
        assert_eq!(select_branches(&repos, "me", &cfg).len(), 2);
    }

    #[test]
    fn seed_is_sha256_prefix() {
        let s = seed("mira-codes", date!(2016 - 03 - 14));
        assert_eq!(s.len(), 16);
        let full = Sha256::digest(b"mira-codes2016-03-14");
        assert!(format!("{full:x}").starts_with(&s));
        let mut a = rng(&s, "trunk");
        let mut b = rng(&s, "trunk");
        assert_eq!(a.next_u64(), b.next_u64());
        assert_ne!(rng(&s, "trunk").next_u64(), rng(&s, "pad").next_u64());
    }

    #[test]
    fn bends_are_stable_and_gentle() {
        let s = "9f2c41a07be3d5e1";
        let short = trunk_bends(s, 12);
        let tall = trunk_bends(s, 24);
        assert_eq!(&tall[..short.len()], &short[..]);
        assert_eq!(tall[0], [0, 0]);
        for w in tall.windows(2) {
            assert!(w[1][0] - w[0][0] >= 5 && (w[1][1] - w[0][1]).abs() <= 1);
            assert!(w[1][0] < 24);
        }
    }

    #[test]
    fn slots_table() {
        assert_eq!(anchor_slots(8), vec![5]);
        assert_eq!(anchor_slots(9), vec![3, 6]);
        assert_eq!(anchor_slots(14), vec![3, 6, 11]);
        assert_eq!(anchor_slots(24), vec![3, 6, 9, 12, 15, 18, 21]);
        assert_eq!(anchor_grid(8, 1), (vec![5], ANCHOR_STEP));
        assert_eq!(anchor_grid(8, 4), (vec![2, 3, 4, 5], ANCHOR_STEP_DENSE));
        assert_eq!(
            anchor_grid(14, 5),
            (vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11], ANCHOR_STEP_DENSE)
        );
        assert_eq!(trunk_height_for(1), 8);
        assert_eq!(trunk_height_for(16), 20);
        assert_eq!(trunk_height_for(MAX_BRANCHES), 24);
        assert_eq!(anchor_grid(24, 7), (anchor_slots(24), ANCHOR_STEP));
    }
}
