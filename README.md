<div align="center">

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/gallery/reference_state-dark.svg">
  <img alt="A pixel bonsai grown from a GitHub account's activity" src="docs/gallery/reference_state-light.svg" width="480">
</picture>

# Git Bonsai

**A pixel bonsai that grows from your GitHub activity, one day at a time.**

[![GitHub Marketplace](https://img.shields.io/badge/Marketplace-Pixel%20Git%20Bonsai-2ea043?logo=github)](https://github.com/marketplace/actions/pixel-git-bonsai)
[![Release](https://img.shields.io/github/v/release/appnice/git-bonsai?color=a97bff)](https://github.com/appnice/git-bonsai/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-d29922)](LICENSE)

Every repo is a branch. Every language a colour. Archive a repo and the branch is pruned, leaving a scar
forever. Keep a 21-day streak and it blooms. Go quiet for two months and it falls asleep.
One commit wakes it up.

</div>

---

## Why it's not another contribution graph

- **It has memory.** The tree lives in `bonsai/state.json` in your profile repo and grows a little every
  night. Its shape is your history: age makes the trunk, pruned repos leave scars, nothing resets.
- **You can't farm it.** Each branch grows at most one cell a day and size follows `log2(commits)`:
  ten thousand commits in a day buy you one cell. The trunk only grows with your account's age.
- **It's yours.** The same login always grows the same tree (seeded from your login), byte for byte.
- **Zero servers.** A GitHub Action, the GitHub API and files in your repo. Nothing to host, no third-party
  image service, no tracking.
- **It moves.** The SVG animates right in your README: the empty days fade in like the contribution graph,
  the tree grows out of the pot cell by cell, today's growth pulses, blossoms twinkle, autumn leaves fall.
  All CSS, no JavaScript, and it respects `prefers-reduced-motion`.

## Gallery

| A brand-new account | A polyglot with 14 years of history |
|---|---|
| <img src="docs/gallery/sapling_state-dark.svg" width="360" alt="Sapling"> | <img src="docs/gallery/polyglot_state-dark.svg" width="360" alt="Polyglot tree"> |
| **Two months of silence** | **An organization** (`anthropics`, from its public repos) |
| <img src="docs/gallery/dormant_state-dark.svg" width="360" alt="Dormant tree"> | <img src="docs/gallery/anthropics_state-dark.svg" width="360" alt="Organization tree"> |

Organizations work too: an org has no contribution calendar of its own, so its tree grows from the
commits on the default branches of its most active public repos. It gets a celadon pot and an ORG tag.

## How your activity becomes a tree

| GitHub | Tree |
|---|---|
| Account age and how many repos you work in | Trunk height (tall enough for every branch), girth and root flare by age |
| A repo you commit to (yours, not a fork, not archived) | A branch with a pad of leaves (up to 16, by commits in the last year) |
| More than 12 active repos | One branch per language instead, sized by all its repos (`6 repos · python`) |
| Commits in that repo | Branch length and pad size, `log2(1 + commits)` |
| The repo's main language | The pad's colour (linguist colour, four shades in OKLCH) |
| Activity in the last 28 days | Darker leaves, like the contribution graph |
| Archived or deleted repo | Pruned: a 3-cell stub with a scar, kept forever |
| A streak of 21+ days | Blossoms |
| 60+ days without a contribution | Dormant: ghost leaves, a snoring tree |
| Public repos | Pot width |
| Stars on your repos | Moss in the pot (one per 50 stars, up to six) |

## Get one

1. In your profile repo (`github.com/<you>/<you>`), add `.github/workflows/bonsai.yml`:

   ```yaml
   name: Git Bonsai
   on:
     schedule:
       - cron: "17 3 * * *"
     workflow_dispatch:
     push:
       paths: [.github/workflows/bonsai.yml]
   permissions:
     contents: write
   concurrency: bonsai
   jobs:
     grow:
       runs-on: ubuntu-latest
       steps:
         - uses: actions/checkout@v4
         - uses: appnice/git-bonsai@v0
   ```

2. Commit it. The push runs the workflow once: it plants the tree and replays your whole history day by
   day, so an old account starts with a grown tree. After that it grows every night.
   You can do all of this in the browser: *Add file → Create new file*.

3. Put it in your `README.md`:

   ```html
   <picture>
     <source media="(prefers-color-scheme: dark)" srcset="bonsai/bonsai-dark.svg">
     <img alt="My Git Bonsai, grown from my GitHub activity" src="bonsai/bonsai-light.svg" width="560">
   </picture>
   ```

On the Marketplace the action is listed as
[Pixel Git Bonsai](https://github.com/marketplace/actions/pixel-git-bonsai) (the plain name was taken).

### See your tree before you commit

Download the binary for your system from the [latest release](https://github.com/appnice/git-bonsai/releases/latest), then:

```sh
git-bonsai plant --login <you> --state preview/state.json --out preview
open preview/bonsai-dark.svg     # or drag it into a browser
```

It reads public data only, with the token from `gh auth token`, `GITHUB_TOKEN` or `GH_TOKEN`.

### Action inputs

| Input | Default | What |
|---|---|---|
| `login` | repo owner | Account the tree grows from |
| `token` | `GITHUB_TOKEN` | Reads public data only |
| `output-dir` | `bonsai` | Where `state.json` and the images go |
| `themes` | `dark,light` | Variants to render |
| `animate` | `true` | CSS animation in the SVG |
| `gif` | `false` | Also write `bonsai.gif` |
| `max-branches` | `16` | Branch limit (1–20) |
| `exclude` | | Repos to skip, comma-separated globs |
| `commit-message` | `chore(bonsai): grow {date}` | |

Optional `bonsai/bonsai.toml` in the profile repo:

```toml
max_branches = 16
exclude = ["dotfiles", "*.github.io"]
pin = ["my-favourite"]      # always gets a branch
max_growth_per_day = 1
bloom_streak_days = 21
dormant_after_days = 60

[colors]
Kotlin = "#a97bff"          # override a language colour
```

## CLI

The same engine as a single binary: a [release download](https://github.com/appnice/git-bonsai/releases/latest)
or `cargo install --git https://github.com/appnice/git-bonsai`.

```
git-bonsai grow     --login L [--state bonsai/state.json] [--out bonsai]   # fetch, grow missing days, render
git-bonsai render   --state S --theme dark --out tree.svg                  # render only, offline
git-bonsai snapshot --login L --out snap.json                              # one day's data as a fixture
git-bonsai plant    --login L --from-snapshot snap.json                    # new state from history
git-bonsai debug    --state S                                              # the grid as a character map
```

Token: `GITHUB_TOKEN`, `GH_TOKEN` or `gh auth token`. It is never logged or stored.

## FAQ

**Will the nightly commit flood my contribution graph?** No. The action commits as
`github-actions[bot]`, which doesn't count as your contribution, and only when the tree changed.

**Do private repos count?** No. The action reads public data only, so private repos never grow a branch,
and a repo you make private is pruned like a deleted one.

**I renamed or archived a repo.** An archived or deleted repo is pruned and leaves a scar. A rename keeps
its branch.

**Can I start over?** Delete `bonsai/state.json` and run the workflow by hand (*Actions → Git Bonsai →
Run workflow*). The same login grows the same tree again.

**Will it work outside GitHub?** Set `gif: true` to also get `bonsai/bonsai.gif`, which works on sites that
don't show SVG, like LinkedIn or Slack.

## Under the hood

`fetch → snapshot → grow → layout → render`. Only `fetch` touches the network; everything else is a pure
function, so the image is always a function of the state. Rust, no image libraries: the SVG is written by
hand (`<rect>` per tree cell, a shared `<use>` per empty day, CSS), ~70 KB for a full tree. Golden tests pin every pixel; a property
test lays out a thousand random trees without a single overlapping pad.

## License

MIT.
