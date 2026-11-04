//! Growth on real (public) snapshots saved by `git-bonsai snapshot`.
use git_bonsai::snapshot::Snapshot;
use git_bonsai::state::{self, PhaseKind};
use git_bonsai::{grow, plant, Config};
use time::Duration;

fn fixture(name: &str) -> Snapshot {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn torvalds_planted_tree_is_stable() {
    let snap = fixture("torvalds-2026-10-31");
    let a = plant(&snap, &Config::default()).unwrap();
    assert_eq!(
        state::to_json(&a),
        state::to_json(&plant(&snap, &Config::default()).unwrap())
    );
    assert_eq!(a.trunk.height, 24);
    let living: Vec<_> = a.branches.iter().filter(|b| b.pruned.is_none()).collect();
    assert!(living.iter().any(|b| b.name == "linux"));
    assert!(living.len() <= 7);
    assert!(a.branches.iter().all(|b| b.pad <= 6));
    insta::assert_snapshot!("torvalds_planted", state::to_json(&a));
}

#[test]
fn torvalds_next_days() {
    let snap = fixture("torvalds-2026-10-31");
    let cfg = Config::default();
    let planted = plant(&snap, &cfg).unwrap();
    assert_eq!(grow(&planted, &snap, &cfg).unwrap(), planted);
    let mut next = snap.clone();
    next.date += Duration::days(3);
    let g = grow(&planted, &next, &cfg).unwrap();
    assert_eq!(g.last_grown, Some(next.date));
    for (b0, b1) in planted.branches.iter().zip(&g.branches) {
        assert!(b1.length + b1.pad <= b0.length + b0.pad + 3);
    }
}

#[test]
fn new_account_is_a_sapling() {
    let snap = fixture("appnice-2026-10-31");
    let s = plant(&snap, &Config::default()).unwrap();
    assert_eq!(s.vitals.phase, PhaseKind::Sapling);
    assert!(s.branches.is_empty());
    assert_eq!(s.trunk.height, 8);
}

#[test]
fn anthropics_org_is_planted_from_its_synthetic_calendar() {
    let snap = fixture("anthropics-2026-11-01");
    assert_eq!(snap.kind, git_bonsai::snapshot::AccountKind::Org);
    assert_eq!(snap.calendar.len(), 70);
    let a = plant(&snap, &Config::default()).unwrap();
    assert_eq!(a.kind, git_bonsai::snapshot::AccountKind::Org);
    assert_ne!(a.vitals.phase, PhaseKind::Dormant);
    assert!(!a.branches.is_empty() && a.branches.len() <= 16);
    assert_eq!(
        state::to_json(&a),
        state::to_json(&plant(&snap, &Config::default()).unwrap())
    );
    insta::assert_snapshot!("anthropics_planted", state::to_json(&a));
}
