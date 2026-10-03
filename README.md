# nirifetch

**English** | [简体中文](README_zh.md)

A lightweight fetch tool built specifically for the [Niri](https://github.com/YaLTeR/niri) Wayland compositor.

Most fetch tools treat the compositor as an afterthought. nirifetch talks to niri directly over its IPC socket, so the window, output and version it reports are the ones niri actually knows about — not a guess assembled from environment variables.

```
                          OS         user@host
                          ─────────  ────────────────────────────────────────────
  _   _ ___ ____  ___     WM         Niri 25.05 (1a2b3c4)
 | \ | |_ _|  _ \|_ _|    Structure  Modular (3 included files)
 |  \| || || |_) || |     Config     ~/.config/niri/config.kdl (5.2 KiB · 120 lines) ✓
 | |\  || ||  _ < | |     Output     DP-1 (2560x1440@144Hz · Scale 1x · Focused)
 |_| \_|___|_| \_\___|    Bar        Waybar
  ~ endless scroll ~      Terminal   foot (Window: 笔记 — nvim)
                          Font       JetBrains Mono
                          Workspace  2 of 3 · 4 windows
                          Keyboard   English (US)
                          CPU        AMD Ryzen 5 5600X (6 cores / 12 threads)
                          GPU        AMD Radeon RX 6700 XT (amdgpu)
                          Memory     6.1 GiB / 19.3 GiB
                          Disk       37.9 GiB / 231.9 GiB · 16%
                          Kernel     Linux 6.12.4-arch1-1
                          Shell      fish
                          Uptime     3d 4h
                          Packages   1061 (pacman)
                          Battery    85% · Charging
                          Load       0.20 0.49 0.38
                          Palette    ● ● ● ● ● ● ● ●
```

*(Illustrative example, re-indented by two columns to fit this page — your output reflects your own session. Note the CJK window title: it is measured as double-width when aligning the row. Use `--short` for a compact subset, or `--fields …` to pick your own.)*


## Features

- **Fast, and parallel.** Every probe — niri IPC, hardware, font, config validation — is independent and runs on its own thread, so the wall-clock cost is the slowest probe rather than their sum. End to end it stays well under a tenth of a second, and roughly 0 ms of that is nirifetch's own computation. Hardware data comes straight from `procfs` and `sysfs`; nothing shells out to `lspci`, `lscpu` or friends.
- **More than the compositor.** Alongside the niri session it reports the focused workspace and window count, the active keyboard layout, and whether `niri validate` accepts your config (a quiet `✓`, or a `✗` with the first error). It also fills in the classic fields — kernel, uptime, load, shell, memory, disk, installed packages, battery — from procfs/sysfs, plus your terminal's real palette when it can read it.
- **Config structure detection.** Scans `config.kdl` for `include` statements and reports `Modular (N included files)` or `Monolithic (Single)`. Backup and editor droppings (`config.kdl.bak`, `.#binds.kdl`) are filtered out so a stray `cp` doesn't flip the verdict.
- **Status bar detection.** Reports the bar or shell niri launched — Waybar, Noctalia Shell, Dank Material Shell, Eww, Ironbar, AGS — by reading the `spawn-at-startup` directives out of your config, **including every file it `include`s**. Falls back to scanning `/proc` for anyone who starts their bar from a systemd user unit instead. Keybinds that reference a bar (`spawn-sh "pkill waybar"`) are never mistaken for one.
- **Pick your own layout.** `--fields os,cpu,gpu` shows exactly the fields you want, `--short` prints a compact subset, and `--no-logo` / `--ascii` / `--logo-file <path>` put the left column under your control.
- **Narrow-terminal safe.** Every value is measured in display cells and degraded in priority order before anything is cut. No row ever wraps or breaks the column alignment — verified from 46 to 220 columns.
- **Real Nerd Font width accounting.** Icon codepoints were measured against actual font tables, so labels and values line up whether or not icons are enabled. CJK titles are measured as double-width.
- **No heavyweight dependencies.** `serde`, `serde_json` and `colored`. No async runtime, no `sysinfo`, no `lspci` parsing library.
- **Never panics.** Every probe fails independently into `Unknown`. Run it outside a niri session and it prints a friendly error and exits `1`. Piping into something that exits early (`nirifetch | head -1`) doesn't panic either — all output goes through writes that tolerate `EPIPE`.

## Requirements

- Linux with a running Niri session
- A [Nerd Font](https://www.nerdfonts.com/) for the icons (optional — set `NIRIFETCH_ICONS=0` to disable)
- `hwdata` for accurate GPU names (optional — see [GPU naming](#gpu-naming))

## Installation

### From source

```sh
git clone https://github.com/504b3urd/nirifetch
cd nirifetch
cargo install --path .
```

Requires Rust 1.85 or newer (edition 2024).

### Arch Linux

There is no `nirifetch` package in the AUR, and it can't be published right now. The AUR has been locked down through 2026 after a wave of malicious uploads: new account registration was closed outright from 15 June to 13 July 2026, package adoption is still disabled, and signups remain heavily restricted by anti-bot measures. Build the package locally instead — the recipe below is the exact file that would be published:

```sh
git clone https://github.com/504b3urd/nirifetch
cd nirifetch/packaging
makepkg -si          # -s installs missing makedepends, -i installs the result
```

`check()` runs the full test suite, and [`packaging/README.md`](packaging/README.md) covers upgrading and rebuilding. Plain `cargo install --path .` works on Arch too — the `cargo` package provides the toolchain.

## Usage

```sh
nirifetch                    # print the fetch
nirifetch --json             # same data as JSON, for scripts
nirifetch --short            # a compact subset of the fields
nirifetch --fields os,cpu,gpu  # show exactly these fields, in this order
nirifetch --no-logo          # hide the left column
nirifetch --logo-file art.txt  # use your own logo
nirifetch --help             # list every field and environment variable
nirifetch -v                 # print the version
```

Interested in adding it to your shell greeting? Call it from `~/.config/fish/config.fish`, `~/.zshrc`, or a niri `spawn-at-startup` — it's fast enough that the cost is invisible.

### JSON output

`--json` prints the same model the fetch layout is built from, so the two can't disagree:

```sh
nirifetch --json | jq -r '.bar'
# Waybar
```

Missing values are `null` and a missing GPU list is `[]`, so `jq` needs no presence checks. The keys are exactly the fields above, plus `home` (used to abbreviate the config path in the terminal layout).

Unknown options are a usage error (exit `2`) rather than being silently ignored — `nirifetch --jsno` should not look like it worked.

### Environment

| Variable | Effect |
| --- | --- |
| `NIRIFETCH_ICONS=0` | Disable Nerd Font icons |
| `NO_COLOR=1` | Disable colors (honors the [no-color](https://no-color.org/) convention) |
| `CLICOLOR_FORCE=1` | Force colors even when stdout is not a TTY |
| `NIRI_SOCKET` | Path to the niri IPC socket |

## How it works

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | CLI parsing, env checks and orchestration (parallel probing) |
| `src/ipc.rs` | Niri dynamic state via `niri msg --json`, plus `niri validate` |
| `src/config.rs` | `config.kdl` path, size, line count, structure detection, `include` expansion |
| `src/bar.rs` | Status bar / shell: config `spawn-at-startup` directives, then `/proc` |
| `src/hardware.rs` | CPU from `/proc/cpuinfo`, GPUs from `/sys/class/drm` + `pci.ids` |
| `src/system.rs` | Kernel, uptime, load, shell, memory, disk, packages, battery |
| `src/font.rs` | Terminal from the process tree, font and palette from the terminal's own config |
| `src/sys.rs` | Shared primitives: subprocesses with timeouts, capped file reads |
| `src/ui.rs` | Logo, column layout, field selection, colors, palette, JSON rendering, help text |

Every subprocess call is wrapped in a timeout, so an unresponsive compositor or a hung D-Bus daemon can't wedge the program.

### GPU naming

GPU names are resolved from `pci.ids` — the same data file `lspci` reads — in three steps, most trustworthy first:

1. **Bracketed name.** `pci.ids` writes the marketing name in brackets (`Rembrandt [Radeon 680M]`). 818 of its 1146 AMD device entries do this. Used verbatim.
2. **Subsystem name**, but only when it actually looks like a GPU name. Subsystem entries on laptops are usually the *machine* model — a laptop's subsystem ID might resolve to something like `Z50-75`, which is a notebook, not a graphics chip. Displaying that as a GPU would be worse than useless, so board names are rejected.
3. **Codename alias.** Newer APUs only have a codename upstream (`HawkPoint1`, `Phoenix1`, `Raphael`), so they're mapped to a marketing name.

> **Step 3 is an inference, not a lookup.** One codename often spans several SKUs: `HawkPoint1` ships as both the Radeon 780M and the Radeon 760M depending on the part, and the PCI ID cannot tell them apart. The table picks the highest-volume name for each codename, so a lower-binned SKU may read slightly high. Patches with a more reliable source are very welcome.

Without `hwdata` installed, step 1 and 2 are unavailable and names fall back to the codename.

## Development

```sh
cargo test        # 203 tests, no network or special hardware needed
cargo clippy --all-targets
cargo fmt
```

The probing layers are written as pure functions over captured file contents (`parse_cpuinfo`, `lookup_many`, `split_subsystem`, `startup_programs`, …), so they're tested against realistic samples rather than whatever hardware the test machine happens to have.

The `include` expansion borrows the same line-level scan the structure detector uses, and is capped on depth, file count and total bytes — a self-referential config terminates instead of looping.

## A note on how this was built

This project started as a personal itch: I wanted a fetch tool that understood niri, and nothing existing quite did. It was built with heavy AI assistance — pair-designed, with every hardware probe verified against real `procfs`/`sysfs` output on actual machines and every claim in this README measured rather than assumed.

That approach got it to a working, tested state quickly, but it also means the code has had exactly one author's worth of review. **It has not been battle-tested across the long tail of hardware, terminals and distros.**

### Call for maintainers

If you're a Rust developer who uses niri, contributions are genuinely wanted — especially:

- **GPU naming for your hardware.** Run nirifetch and check the GPU line. If it's wrong or shows a bare codename, a patch to the codename table with your `pci.ids` entry is a five-minute fix that helps everyone with that chip.
- **Terminal and font detection** for terminals other than kitty. Only kitty's config format is implemented today; the `gsettings`/`fc-match` fallbacks report a *system* font, not necessarily what your terminal actually renders.
- **Non-AMD GPUs.** Intel Arc and NVIDIA have their own naming quirks.
- **AUR packaging.** The `PKGBUILD` is written and waiting, but the AUR's 2026 lockdown means there's no way to publish it yet (see [Arch Linux](#arch-linux)). If you maintain AUR packages, this is a small one to pick up once that lifts.
- **Anything in the [issue tracker](https://github.com/504b3urd/nirifetch/issues).**

Bug reports with the output of `nirifetch` plus your `lspci -nn` are the most useful thing you can send.

## License

MIT — see [LICENSE](LICENSE).
