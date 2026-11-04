//! Character maps: the grid as text, the format of the design board, the golden tests and `debug`.
//!
//! One character per cell, one line per row, `.` for an empty day. Leaves use one row of keys per
//! language, lightest to darkest (`qwer`, `asdf`, `zxcv`, `1234`, `5678`, `ABCD`, `EFGH`).
//!
//! | char | role | char | role |
//! |---|---|---|---|
//! | `T` | trunk | `b` | branch |
//! | `h` | trunk highlight | `k` | trunk shadow |
//! | `p` | blossom | `o` | scar |
//! | `S` | soil | `g` | moss |
//! | `P` | pot rim | `Q` | pot body |
//! | `R` | pot lower edge, feet | `n` | ghost |
//! | `l` | fallen leaf | | |
//!
//! Board 07 marks the trunk shadow with `s`, the same key as a level-2 leaf of language 1, and tells
//! them apart by position (an `s` right after trunk is shadow). [`parse`] accepts that form; [`print`]
//! always writes `k`, so canonical maps are unambiguous. The board also draws branches with `T`.

use crate::grid::{Grid, PotPart, Role, Shade, GRID_H, GRID_W, MAX_LANGS};

/// Leaf keys per language, level 1 to 4.
pub const LEAF_KEYS: [&str; MAX_LANGS] = [
    "qwer", "asdf", "zxcv", "1234", "5678", "ABCD", "EFGH", "ijmt", "uy90", "IJKL", "MNOU", "VWXY",
    "Z!@#", "$%^&", "*()-", "+=[]",
];

/// A map that cannot become a grid.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MapError {
    #[error("map has {0} rows, the grid has {GRID_H}")]
    TooManyRows(usize),
    #[error("row {row} has {len} cells, the grid has {GRID_W}")]
    RowTooLong { row: usize, len: usize },
    #[error("unknown cell {ch:?} at row {row}, column {col}")]
    UnknownChar { row: usize, col: usize, ch: char },
}

fn role_of(ch: char, prev: Option<Role>) -> Option<Role> {
    if ch == 's' && matches!(prev, Some(Role::Trunk(_))) {
        return Some(Role::Trunk(Shade::Shadow));
    }
    let fixed = match ch {
        '.' => Role::Empty,
        'T' => Role::Trunk(Shade::Base),
        'h' => Role::Trunk(Shade::Highlight),
        'k' => Role::Trunk(Shade::Shadow),
        'b' => Role::Branch,
        'p' => Role::Blossom,
        'o' => Role::Scar,
        'S' => Role::Soil,
        'g' => Role::Moss,
        'P' => Role::Pot(PotPart::Rim),
        'Q' => Role::Pot(PotPart::Body),
        'R' => Role::Pot(PotPart::Dark),
        'n' => Role::Ghost,
        'l' => Role::Fallen,
        _ => {
            return LEAF_KEYS.iter().enumerate().find_map(|(lang, keys)| {
                keys.chars().position(|k| k == ch).map(|i| Role::Leaf {
                    lang: lang as u8,
                    level: i as u8 + 1,
                })
            })
        }
    };
    Some(fixed)
}

fn char_of(role: Role) -> char {
    match role {
        Role::Empty => '.',
        Role::Trunk(Shade::Base) => 'T',
        Role::Trunk(Shade::Highlight) => 'h',
        Role::Trunk(Shade::Shadow) => 'k',
        Role::Branch => 'b',
        Role::Blossom => 'p',
        Role::Scar => 'o',
        Role::Soil => 'S',
        Role::Moss => 'g',
        Role::Pot(PotPart::Rim) => 'P',
        Role::Pot(PotPart::Body) => 'Q',
        Role::Pot(PotPart::Dark) => 'R',
        Role::Ghost => 'n',
        Role::Fallen => 'l',
        Role::Leaf { lang, level } => LEAF_KEYS
            .get(usize::from(lang))
            .and_then(|k| k.chars().nth(usize::from(level.clamp(1, 4) - 1)))
            .unwrap_or('?'),
    }
}

/// Parses a character map. Short rows and missing bottom rows are empty days.
pub fn parse(map: &str) -> Result<Grid, MapError> {
    let lines: Vec<&str> = map.lines().collect();
    if lines.len() > GRID_H {
        return Err(MapError::TooManyRows(lines.len()));
    }
    let mut grid = Grid::new();
    for (y, line) in lines.iter().enumerate() {
        let len = line.chars().count();
        if len > GRID_W {
            return Err(MapError::RowTooLong { row: y, len });
        }
        let mut prev = None;
        for (x, ch) in line.chars().enumerate() {
            let role = role_of(ch, prev).ok_or(MapError::UnknownChar { row: y, col: x, ch })?;
            grid.set(x, y, role);
            prev = Some(role);
        }
    }
    Ok(grid)
}

/// Prints the canonical map: every row full width, newline-terminated.
pub fn print(grid: &Grid) -> String {
    let mut out = String::with_capacity((GRID_W + 1) * GRID_H);
    for row in grid.rows() {
        out.extend(row.iter().map(|r| char_of(*r)));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_role_round_trips() {
        let mut g = Grid::new();
        let roles = [
            Role::Trunk(Shade::Base),
            Role::Trunk(Shade::Highlight),
            Role::Trunk(Shade::Shadow),
            Role::Branch,
            Role::Blossom,
            Role::Scar,
            Role::Soil,
            Role::Moss,
            Role::Pot(PotPart::Rim),
            Role::Pot(PotPart::Body),
            Role::Pot(PotPart::Dark),
            Role::Ghost,
            Role::Fallen,
        ];
        for (i, r) in roles.iter().enumerate() {
            g.set(i, 0, *r);
        }
        for lang in 0..MAX_LANGS as u8 {
            for level in 1..=4 {
                g.set(
                    usize::from(level),
                    1 + usize::from(lang),
                    Role::Leaf { lang, level },
                );
            }
        }
        assert_eq!(parse(&print(&g)), Ok(g));
    }

    #[test]
    fn board_shadow_s_after_trunk_only() {
        let g = parse("hTs.s").unwrap();
        assert_eq!(g.get(2, 0), Some(Role::Trunk(Shade::Shadow)));
        assert_eq!(g.get(4, 0), Some(Role::Leaf { lang: 1, level: 2 }));
    }

    #[test]
    fn errors() {
        assert_eq!(
            parse(&"x".repeat(GRID_W + 1)),
            Err(MapError::RowTooLong {
                row: 0,
                len: GRID_W + 1
            })
        );
        assert_eq!(
            parse("..~"),
            Err(MapError::UnknownChar {
                row: 0,
                col: 2,
                ch: '~'
            })
        );
        assert_eq!(
            parse(&"\n".repeat(GRID_H + 1)),
            Err(MapError::TooManyRows(GRID_H + 1))
        );
    }
}
