//! Layout: turns a [`TreeState`] into a [`Scene`] (cell grid, palettes, header, legend).
//!
//! Drawing order, later overwrites earlier: pot → soil and moss → trunk → branches → pads →
//! blossoms and fallen leaves. Everything random comes from the state's seed plus the element's
//! name, so the same state always gives the same grid.

use crate::grid::{Grid, LangId, PotPart, Role, Shade, GRID_H, GRID_W, MAX_LANGS};
use crate::palette::{LangPalette, PotGlaze, Rgb};
use crate::render::{Header, LegendItem, Phase, Scene};
use crate::state::{Branch, PhaseKind, Side, TreeState};
use rand_chacha::rand_core::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// Row of the soil; the pot is below it, the tree above.
pub const SOIL_Y: i32 = 27;
/// Grid centre column (the pot and the trunk base are centred on it).
const CENTER_X: i32 = 17;
/// Colour for repos without a language.
const NO_LANG: Rgb = Rgb::hex(0x8b949e);

/// Seeded RNG for one named element of the tree.
fn rng(seed: &str, name: &str) -> ChaCha8Rng {
    let base = u64::from_str_radix(seed.get(..16).unwrap_or(seed), 16).unwrap_or(0);
    // FNV-1a of the element name.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    ChaCha8Rng::seed_from_u64(base ^ h)
}

/// Uniform integer in `lo..=hi`.
fn range(r: &mut ChaCha8Rng, lo: i32, hi: i32) -> i32 {
    if hi <= lo {
        return lo;
    }
    lo + (r.next_u32() % (hi - lo + 1) as u32) as i32
}

/// Uniform float in `0..1`.
fn unit(r: &mut ChaCha8Rng) -> f64 {
    f64::from(r.next_u32()) / f64::from(u32::MAX)
}

/// A grid being drawn, with signed coordinates (writes outside are clipped).
struct Canvas {
    grid: Grid,
}

impl Canvas {
    fn set(&mut self, x: i32, y: i32, role: Role) {
        if x >= 0 && y >= 0 {
            self.grid.set(x as usize, y as usize, role);
        }
    }

    fn get(&self, x: i32, y: i32) -> Option<Role> {
        if x < 0 || y < 0 {
            return None;
        }
        self.grid.get(x as usize, y as usize)
    }
}

/// An elliptic leaf pad.
#[derive(Debug, Clone, Copy)]
struct Pad {
    cx: i32,
    cy: i32,
    rx: i32,
    ry: i32,
}

impl Pad {
    /// Dome over a flat underside, like a clipped bonsai pad: an ellipse above the centre row, a
    /// squarer superellipse below it.
    fn contains(&self, x: i32, y: i32) -> bool {
        let dx = (f64::from(x - self.cx) / (f64::from(self.rx) + 0.5)).abs();
        let dy = (f64::from(y - self.cy) / (f64::from(self.ry) + 0.5)).abs();
        if y <= self.cy {
            dx * dx + dy * dy <= 1.0
        } else {
            dx.powi(4) + dy.powi(2) <= 1.0
        }
    }

    fn cells(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        (self.cy - self.ry..=self.cy + self.ry).flat_map(move |y| {
            (self.cx - self.rx..=self.cx + self.rx)
                .filter(move |x| self.contains(*x, y))
                .map(move |x| (x, y))
        })
    }

    /// True when the pads touch or overlap (pads keep one empty cell between them).
    fn overlaps(&self, other: &Pad) -> bool {
        self.cells()
            .any(|(x, y)| (-1..=1).any(|dy| (-1..=1).any(|dx| other.contains(x + dx, y + dy))))
    }

    /// True for a cell of the pad with a 4-neighbour outside it.
    fn is_edge(&self, x: i32, y: i32) -> bool {
        [(1, 0), (-1, 0), (0, 1), (0, -1)]
            .iter()
            .any(|(dx, dy)| !self.contains(x + dx, y + dy))
    }

    fn fits(&self) -> bool {
        self.cx - self.rx >= 0
            && self.cx + self.rx < GRID_W as i32
            && self.cy - self.ry >= 0
            && self.cy + self.ry < SOIL_Y - 1
    }
}

/// Pad radii for a pad size 0..=7 (RFC: `rx = pad + 3`, `ry = max(2, pad/2 + 1)`).
fn pad_radii(pad: u32) -> (i32, i32) {
    let p = pad.min(7) as i32;
    (p + 3, (p / 2 + 1).max(2))
}

/// Trunk geometry, row by row from the soil up.
struct TrunkShape {
    /// `(left x, girth)` per trunk row; index 0 is the row just above the soil.
    rows: Vec<(i32, i32)>,
}

impl TrunkShape {
    fn new(state: &TreeState) -> TrunkShape {
        let h = state.trunk.height.clamp(6, 24) as i32;
        let g = state.trunk.girth.clamp(2, 5) as i32;
        let mut bends: Vec<[i32; 2]> = state.trunk.bends.clone();
        bends.sort_by_key(|b| b[0]);
        if bends.first().is_none_or(|b| b[0] > 0) {
            bends.insert(0, [0, 0]);
        }
        let offset = |row: i32| -> f64 {
            let mut prev = bends[0];
            for b in &bends {
                if b[0] >= row {
                    if b[0] == prev[0] {
                        return f64::from(b[1]);
                    }
                    let t = f64::from(row - prev[0]) / f64::from(b[0] - prev[0]);
                    return f64::from(prev[1]) + t * f64::from(b[1] - prev[1]);
                }
                prev = *b;
            }
            f64::from(prev[1])
        };
        let rows = (0..h)
            .map(|r| {
                // Taper from the full girth at the base to 2 cells at the top.
                let girth = g - ((g - 2) * r + h / 2) / h.max(1);
                let cx = f64::from(CENTER_X) + 0.5 + offset(r).clamp(-6.0, 6.0);
                let left = (cx - f64::from(girth) / 2.0).round() as i32;
                (left, girth)
            })
            .collect();
        TrunkShape { rows }
    }

    fn height(&self) -> i32 {
        self.rows.len() as i32
    }

    fn y(row: i32) -> i32 {
        SOIL_Y - 1 - row
    }

    /// The trunk cell on `side` at trunk row `row`.
    fn edge(&self, row: i32, side: Side) -> (i32, i32) {
        let r = row.clamp(0, self.height() - 1);
        let (left, girth) = self.rows[r as usize];
        let x = match side {
            Side::Left => left,
            Side::Right => left + girth - 1,
        };
        (x, Self::y(r))
    }

    fn top(&self) -> (i32, i32) {
        let (left, girth) = self.rows[self.rows.len() - 1];
        (left + girth / 2, Self::y(self.height() - 1))
    }
}

fn draw_pot(c: &mut Canvas, width: i32) {
    let body_left = CENTER_X + 1 - width / 2;
    let row = |c: &mut Canvas, y: i32, left: i32, w: i32, role: Role| {
        for x in left..left + w {
            c.set(x, y, role);
        }
    };
    row(c, SOIL_Y, body_left + 1, width - 2, Role::Soil);
    row(
        c,
        SOIL_Y + 1,
        body_left - 1,
        width + 2,
        Role::Pot(PotPart::Rim),
    );
    row(c, SOIL_Y + 2, body_left, width, Role::Pot(PotPart::Body));
    row(c, SOIL_Y + 3, body_left, width, Role::Pot(PotPart::Body));
    row(
        c,
        SOIL_Y + 4,
        body_left + 1,
        width - 2,
        Role::Pot(PotPart::Dark),
    );
    for x in [
        body_left + 2,
        body_left + 3,
        body_left + width - 4,
        body_left + width - 3,
    ] {
        c.set(x, SOIL_Y + 5, Role::Pot(PotPart::Dark));
    }
}

fn draw_trunk(c: &mut Canvas, t: &TrunkShape) {
    for (r, (left, girth)) in t.rows.iter().enumerate() {
        let y = TrunkShape::y(r as i32);
        for i in 0..*girth {
            let shade = if i == 0 {
                Shade::Highlight
            } else if i == girth - 1 {
                Shade::Shadow
            } else {
                Shade::Base
            };
            c.set(left + i, y, Role::Trunk(shade));
        }
    }
    // Nebari: the root flare just above the soil.
    if let Some((left, girth)) = t.rows.first() {
        let y = TrunkShape::y(0);
        for x in [left - 2, left - 1, left + girth, left + girth + 1] {
            c.set(x, y, Role::Trunk(Shade::Base));
        }
    }
}

/// A placed branch: its cells (trunk side first) and, unless pruned, its pad.
struct Placed {
    /// The side it was drawn on (the state's side, unless only the other side had room).
    side: Side,
    cells: Vec<(i32, i32)>,
    pad: Option<Pad>,
    scar: bool,
}

/// Stair-stepped branch path: one cell sideways per step, one row up every `rise_every` cells.
fn branch_path(
    start: (i32, i32),
    side: Side,
    length: i32,
    rise_every: i32,
    lift: i32,
) -> Vec<(i32, i32)> {
    let dir = if side == Side::Left { -1 } else { 1 };
    let (mut x, mut y) = start;
    let mut out = Vec::with_capacity((length.unsigned_abs() + lift.unsigned_abs()) as usize);
    let mut lifted = 0;
    for i in 1..=length {
        x += dir;
        if i > 1 && (i - 1) % rise_every.max(2) == 0 {
            y -= 1;
            out.push((x, y + 1));
        } else if lifted < lift.abs() && i > 1 {
            // Positive lift raises the branch, negative lets it droop.
            let dy = lift.signum();
            y -= dy;
            lifted += 1;
            out.push((x, y + dy));
        }
        out.push((x, y));
    }
    out.dedup();
    out
}

fn pad_for(tip: (i32, i32), side: Side, rx: i32, ry: i32) -> Pad {
    let dir = if side == Side::Left { -1 } else { 1 };
    Pad {
        cx: tip.0 + dir * (rx - 3).max(0),
        cy: tip.1 - (ry - 1),
        rx,
        ry,
    }
}

/// Shift a pad sideways so it stays inside the grid.
fn clamp_pad(mut p: Pad) -> Pad {
    let max_x = GRID_W as i32 - 1;
    if p.cx - p.rx < 0 {
        p.cx = p.rx;
    }
    if p.cx + p.rx > max_x {
        p.cx = max_x - p.rx;
    }
    if p.cy - p.ry < 0 {
        p.cy = p.ry;
    }
    p
}

fn place_branches(state: &TreeState, t: &TrunkShape, apex: Option<usize>) -> Vec<Placed> {
    let mut taken: Vec<Pad> = Vec::new();
    let mut out: Vec<Placed> = Vec::new();

    // The apex pad (the biggest repo) sits on the trunk's top.
    let order: Vec<usize> = {
        let mut idx: Vec<usize> = (0..state.branches.len()).collect();
        // Bigger pads claim space first; ties by render order.
        idx.sort_by_key(|i| {
            (
                Some(*i) != apex,
                std::cmp::Reverse(state.branches[*i].pad),
                *i,
            )
        });
        idx
    };
    let mut placed: Vec<Option<Placed>> = (0..state.branches.len()).map(|_| None).collect();

    // A crowded tree (more living branches than the regular grid's 7) gets smaller pads, so every
    // repo keeps some leaves: the three biggest side pads at most size 2, the rest size 0.
    let living = state.branches.iter().filter(|b| b.pruned.is_none()).count();
    let crowded = living > 7;
    let mut side_rank = 0usize;

    for i in order {
        let b = &state.branches[i];
        let mut r = rng(&state.seed, &format!("branch:{}", b.repo_id));
        if Some(i) == apex {
            let (rx, ry) = pad_radii(b.pad);
            let top = t.top();
            let pad = clamp_pad(Pad {
                cx: top.0,
                cy: top.1 - ry + 1,
                rx,
                ry,
            });
            taken.push(pad);
            placed[i] = Some(Placed {
                side: b.side,
                cells: Vec::new(),
                pad: Some(pad),
                scar: false,
            });
            continue;
        }
        let anchor = (b.anchor as i32).clamp(1, (t.height() - 2).max(1));
        let start = t.edge(anchor, b.side);
        let rise_every = range(&mut r, 3, 4);
        if b.pruned.is_some() {
            let cells = branch_path(start, b.side, 3, rise_every, 0);
            placed[i] = Some(Placed {
                side: b.side,
                cells,
                pad: None,
                scar: true,
            });
            continue;
        }
        // RFC: on a collision lengthen the branch or lift it a row; then shrink the pad.
        // Negative lifts let the branch droop below a big pad above it.
        const MOVES: [(i32, i32); 22] = [
            (0, 0),
            (2, 0),
            (0, 1),
            (0, -1),
            (4, 0),
            (2, 1),
            (2, -1),
            (0, 2),
            (0, -2),
            (2, -2),
            (4, -2),
            (6, 1),
            (4, 2),
            (2, -3),
            (4, -3),
            (6, 3),
            (6, 0),
            (6, -1),
            (8, 0),
            (8, 1),
            (8, -1),
            (8, -2),
        ];
        let pad_size = if crowded {
            side_rank += 1;
            b.pad.min(if side_rank <= 3 { 2 } else { 0 })
        } else {
            b.pad
        };
        let (mut rx, mut ry) = pad_radii(pad_size);
        let base_len = (b.length as i32).max(2);
        'shrink: loop {
            let other = if b.side == Side::Left {
                Side::Right
            } else {
                Side::Left
            };
            // The state's side first; the other side before giving up size.
            for side in [b.side, other] {
                let start = t.edge(anchor, side);
                for (dl, lift) in MOVES {
                    let cells = branch_path(start, side, base_len + dl, rise_every, lift);
                    let tip = *cells.last().unwrap_or(&start);
                    let pad = clamp_pad(pad_for(tip, side, rx, ry));
                    if pad.fits() && !taken.iter().any(|p| p.overlaps(&pad)) {
                        taken.push(pad);
                        placed[i] = Some(Placed {
                            side,
                            cells,
                            pad: Some(pad),
                            scar: false,
                        });
                        break 'shrink;
                    }
                }
            }
            if rx <= 2 && ry <= 1 {
                // No room at all: a bare branch.
                let cells = branch_path(start, b.side, base_len, rise_every, 0);
                placed[i] = Some(Placed {
                    side: b.side,
                    cells,
                    pad: None,
                    scar: false,
                });
                break;
            }
            if rx > ry + 2 {
                rx -= 1;
            } else {
                ry = (ry - 1).max(1);
                rx = (rx - 1).max(2);
            }
        }
    }
    for p in placed.into_iter().flatten() {
        out.push(p);
    }
    out
}

/// Language ids in first-use order over the branches; at most [`MAX_LANGS`].
fn languages(state: &TreeState) -> (Vec<String>, Vec<LangPalette>) {
    let mut names: Vec<String> = Vec::new();
    let mut pals = Vec::new();
    for b in &state.branches {
        let name = b.lang.clone().unwrap_or_else(|| "other".into());
        if !names.contains(&name) && names.len() < MAX_LANGS {
            let base = b
                .color
                .as_deref()
                .and_then(Rgb::from_hex)
                .unwrap_or(NO_LANG);
            names.push(name);
            pals.push(LangPalette::from_linguist(base));
        }
    }
    (names, pals)
}

fn lang_of(names: &[String], b: &Branch) -> LangId {
    let name = b.lang.clone().unwrap_or_else(|| "other".into());
    names
        .iter()
        .position(|n| *n == name)
        .unwrap_or(names.len().saturating_sub(1)) as LangId
}

fn phase_of(state: &TreeState) -> Phase {
    let v = &state.vitals;
    match v.phase {
        PhaseKind::Sapling => Phase::Sapling,
        PhaseKind::Growing => Phase::Growing,
        PhaseKind::Bloom => Phase::Bloom {
            streak_days: v.streak_days,
        },
        PhaseKind::Dormant => Phase::Dormant {
            idle_days: v.idle_days,
        },
    }
}

/// Pot body width from the number of public repos: 16–24 cells, log scale, even.
fn pot_width(public_repos: u32) -> i32 {
    let w = 16.0 + (f64::from(public_repos) + 1.0).log2() * 1.2;
    ((w.round() as i32).clamp(16, 24) / 2) * 2
}

/// Builds the scene for a state: the image is always a function of the state alone.
pub fn build_scene(state: &TreeState) -> Scene {
    let mut c = Canvas { grid: Grid::new() };
    let v = &state.vitals;

    // Pot, soil, moss.
    let width = pot_width(v.public_repos);
    draw_pot(&mut c, width);
    let moss = (v.stars_total / 50).min(6) as i32;
    let mut r = rng(&state.seed, "moss");
    let soil: Vec<i32> = (0..GRID_W as i32)
        .filter(|x| c.get(*x, SOIL_Y) == Some(Role::Soil))
        .collect();
    for _ in 0..moss {
        if !soil.is_empty() {
            let x = soil[r.next_u32() as usize % soil.len()];
            c.set(x, SOIL_Y, Role::Moss);
        }
    }

    // Trunk.
    let trunk = TrunkShape::new(state);
    draw_trunk(&mut c, &trunk);

    // Branches: the living one with most commits becomes the apex.
    let apex = state
        .branches
        .iter()
        .enumerate()
        .filter(|(_, b)| b.pruned.is_none() && b.pad > 0)
        .max_by_key(|(i, b)| (b.commits_total, std::cmp::Reverse(*i)))
        .map(|(i, _)| i);
    let placed = place_branches(state, &trunk, apex);
    let (names, langs) = languages(state);
    let live: Vec<&Branch> = state
        .branches
        .iter()
        .filter(|b| b.pruned.is_none())
        .collect();
    let max_act = live
        .iter()
        .map(|b| v.activity_28d.get(&b.repo_id).copied().unwrap_or(0))
        .max()
        .unwrap_or(0)
        .max(1);

    // Branch wood first, then all pads over it.
    for p in &placed {
        for (x, y) in &p.cells {
            c.set(*x, *y, Role::Branch);
        }
    }
    let mut fresh: Vec<(usize, usize)> = Vec::new();
    let order: Vec<usize> = {
        let mut idx: Vec<usize> = (0..state.branches.len()).collect();
        idx.sort_by_key(|i| (Some(*i) == apex, *i));
        idx
    };
    let mut pads_drawn: Vec<(Pad, LangId)> = Vec::new();
    for i in order {
        let (b, p) = (&state.branches[i], &placed[i]);
        let Some(pad) = p.pad else { continue };
        let lang = lang_of(&names, b);
        let act =
            f64::from(v.activity_28d.get(&b.repo_id).copied().unwrap_or(0)) / f64::from(max_act);
        let mut r = rng(&state.seed, &format!("pad:{}", b.repo_id));
        let (top, bottom) = (pad.cy - pad.ry, pad.cy + pad.ry);
        for (x, y) in pad.cells() {
            // Ragged outline: some edge cells stay empty (never on the underside).
            if y < bottom && pad.is_edge(x, y) && unit(&mut r) < 0.22 {
                continue;
            }
            let level = if y == top {
                1
            } else if y == bottom {
                4
            } else {
                let t = f64::from(y - top) / f64::from((bottom - top).max(1));
                let l = 1.0 + 2.2 * t + 0.9 * act + (unit(&mut r) - 0.5) * 1.6;
                l.round().clamp(1.0, 4.0) as u8
            };
            c.set(x, y, Role::Leaf { lang, level });
        }
        // Cells grown on the last day: the branch tip and the pad's outer edge.
        let grew = v.grew.get(&b.repo_id).copied().unwrap_or(0) as usize;
        if grew > 0 {
            let tips = p.cells.iter().rev().take(grew);
            fresh.extend(tips.map(|(x, y)| (*x as usize, *y as usize)));
            let edge_x = if p.side == Side::Left {
                pad.cx - pad.rx
            } else {
                pad.cx + pad.rx
            };
            for (x, y) in pad.cells() {
                if (x - edge_x).abs() <= 1 {
                    fresh.push((x as usize, y as usize));
                }
            }
        }
        pads_drawn.push((pad, lang));
    }

    // Pruned stubs and their scars stay visible over the pads: a scar is forever.
    for p in placed.iter().filter(|p| p.scar) {
        for (k, (x, y)) in p.cells.iter().enumerate() {
            let role = if k + 1 == p.cells.len() {
                Role::Scar
            } else {
                Role::Branch
            };
            c.set(*x, *y, role);
        }
    }

    // Blossoms in bloom: 3–8 cells in the upper rows of the pads.
    if v.phase == PhaseKind::Bloom {
        let mut r = rng(&state.seed, "blossom");
        let n = range(&mut r, 3, 8);
        let mut spots: Vec<(i32, i32)> = pads_drawn
            .iter()
            .flat_map(|(p, _)| {
                p.cells()
                    .filter(move |(_, y)| *y <= p.cy)
                    .collect::<Vec<_>>()
            })
            .collect();
        for _ in 0..n {
            if spots.is_empty() {
                break;
            }
            let k = r.next_u32() as usize % spots.len();
            let (x, y) = spots.swap_remove(k);
            c.set(x, y, Role::Blossom);
        }
    }

    // Fallen leaves in autumn (September to November), on empty days under the pads.
    let autumn = state
        .last_grown
        .is_some_and(|d| (9..=11).contains(&(d.month() as u8)));
    if autumn && v.phase != PhaseKind::Dormant {
        let mut r = rng(&state.seed, "fallen");
        let n = range(&mut r, 1, 2);
        let mut placed_n = 0;
        for (p, _) in &pads_drawn {
            if placed_n >= n {
                break;
            }
            let x = p.cx + range(&mut r, -p.rx + 2, p.rx - 2);
            let y = p.cy + p.ry + range(&mut r, 3, 6);
            if y < SOIL_Y - 1 && c.get(x, y) == Some(Role::Empty) {
                c.set(x, y, Role::Fallen);
                placed_n += 1;
            }
        }
    }

    if v.phase == PhaseKind::Dormant {
        c.grid.make_dormant();
        fresh.clear();
    }
    fresh.sort_unstable();
    fresh.dedup();
    fresh.retain(|(x, y)| *x < GRID_W && *y < GRID_H);

    // Legend: three biggest living branches, then the latest pruned one (or a fourth branch).
    let mut legend: Vec<LegendItem> = Vec::new();
    let mut by_size: Vec<&Branch> = live.clone();
    by_size.sort_by_key(|b| (std::cmp::Reverse(b.commits_total), b.repo_id.clone()));
    for b in by_size.iter().take(3) {
        legend.push(LegendItem::Branch {
            repo: b.name.clone(),
            language: b
                .lang
                .clone()
                .unwrap_or_else(|| "other".into())
                .to_lowercase(),
            commits: b.commits_total,
            lang: lang_of(&names, b),
        });
    }
    let pruned = state
        .branches
        .iter()
        .filter(|b| b.pruned.is_some())
        .max_by_key(|b| b.pruned);
    match (pruned, by_size.get(3)) {
        (Some(p), _) => legend.push(LegendItem::Pruned {
            repo: p.name.clone(),
        }),
        (None, Some(b)) => legend.push(LegendItem::Branch {
            repo: b.name.clone(),
            language: b
                .lang
                .clone()
                .unwrap_or_else(|| "other".into())
                .to_lowercase(),
            commits: b.commits_total,
            lang: lang_of(&names, b),
        }),
        _ => {}
    }

    let age_years = state
        .last_grown
        .map(|d| ((d - state.planted).whole_days().max(0) / 365) as u32)
        .unwrap_or(0);
    Scene {
        grid: c.grid,
        langs,
        header: Header {
            login: state.login.clone(),
            commits: v.commits_total,
            age_years,
            phase: phase_of(state),
            kind: state.kind,
            repos: v.public_repos,
        },
        legend,
        fresh,
        glaze: match state.kind {
            crate::snapshot::AccountKind::User => PotGlaze::Terracotta,
            crate::snapshot::AccountKind::Org => PotGlaze::Celadon,
        },
    }
}

#[cfg(test)]
mod tests;
