//! The cell grid every tree is drawn on. The renderer knows only this, nothing about trees.

/// Grid width in cells (fixed: the README image has a fixed width).
pub const GRID_W: usize = 36;
/// Grid height in cells.
pub const GRID_H: usize = 34;

/// Index into the scene's language palettes (one per distinct repo language).
pub type LangId = u8;

/// Most distinct languages a grid can hold (one per branch, `max-branches` defaults to 7).
pub const MAX_LANGS: usize = 16;

/// Trunk shading across its girth: the leftmost cell is lit, the rightmost in shadow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Shade {
    Base,
    Highlight,
    Shadow,
}

/// Parts of the pot, bottom to top: feet and lower edge (`Dark`), body, rim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PotPart {
    Rim,
    Body,
    Dark,
}

/// What a cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// An empty contribution day: the grid background.
    Empty,
    Trunk(Shade),
    Branch,
    /// A leaf of a pad; `level` 1 (lightest) to 4 (darkest), like the contribution graph.
    Leaf {
        lang: LangId,
        level: u8,
    },
    Blossom,
    /// The end of a pruned branch (archived or deleted repo). Kept forever.
    Scar,
    Soil,
    Moss,
    Pot(PotPart),
    /// A leaf of a dormant tree.
    Ghost,
    /// A fallen leaf below the pads.
    Fallen,
}

/// A fixed-size grid of cells, row-major, row 0 at the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grid {
    cells: Vec<Role>,
}

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

impl Grid {
    /// An all-empty grid.
    pub fn new() -> Self {
        Self {
            cells: vec![Role::Empty; GRID_W * GRID_H],
        }
    }

    pub fn width(&self) -> usize {
        GRID_W
    }

    pub fn height(&self) -> usize {
        GRID_H
    }

    /// The cell at column `x`, row `y`; `None` outside the grid.
    pub fn get(&self, x: usize, y: usize) -> Option<Role> {
        (x < GRID_W && y < GRID_H).then(|| self.cells[y * GRID_W + x])
    }

    /// Sets a cell; writes outside the grid are ignored (layout clips, it never panics).
    pub fn set(&mut self, x: usize, y: usize, role: Role) {
        if x < GRID_W && y < GRID_H {
            self.cells[y * GRID_W + x] = role;
        }
    }

    /// Rows top to bottom.
    pub fn rows(&self) -> impl Iterator<Item = &[Role]> {
        self.cells.chunks(GRID_W)
    }

    /// Cells with their coordinates, row-major.
    pub fn iter(&self) -> impl Iterator<Item = (usize, usize, Role)> + '_ {
        self.cells
            .iter()
            .enumerate()
            .map(|(i, r)| (i % GRID_W, i / GRID_W, *r))
    }

    /// Dormant phase: every leaf and blossom turns into a ghost.
    pub fn make_dormant(&mut self) {
        for c in &mut self.cells {
            if matches!(c, Role::Leaf { .. } | Role::Blossom) {
                *c = Role::Ghost;
            }
        }
    }

    /// Languages used by leaves, ascending.
    pub fn langs(&self) -> Vec<LangId> {
        let mut out: Vec<LangId> = self
            .cells
            .iter()
            .filter_map(|c| match c {
                Role::Leaf { lang, .. } => Some(*lang),
                _ => None,
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_outside_is_ignored() {
        let mut g = Grid::new();
        g.set(GRID_W, 0, Role::Soil);
        g.set(0, GRID_H, Role::Soil);
        assert!(g.iter().all(|(_, _, r)| r == Role::Empty));
        assert_eq!(g.get(GRID_W, 0), None);
    }

    #[test]
    fn dormant_turns_leaves_and_blossoms_into_ghosts() {
        let mut g = Grid::new();
        g.set(1, 1, Role::Leaf { lang: 0, level: 2 });
        g.set(2, 1, Role::Blossom);
        g.set(3, 1, Role::Trunk(Shade::Base));
        g.make_dormant();
        assert_eq!(g.get(1, 1), Some(Role::Ghost));
        assert_eq!(g.get(2, 1), Some(Role::Ghost));
        assert_eq!(g.get(3, 1), Some(Role::Trunk(Shade::Base)));
    }
}
