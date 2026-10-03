# nirifetch

[English](README.md) | **简体中文**

一个专为 [Niri](https://github.com/YaLTeR/niri) Wayland 混成器打造的轻量级 fetch 工具。

多数 fetch 工具都把混成器当成可有可无的一环。nirifetch 直接通过 niri 的 IPC socket 与它对话，所以它报告的窗口、输出和版本号就是 niri 自己认定的那一个 —— 而不是从环境变量里拼凑出来的猜测。

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

*（示意样例，为了适配本页宽度整体右移了两列 —— 你的实际输出取决于你自己的会话。注意那个中文窗口标题：排版时它按双宽字符计算。用 `--short` 看精简版，或 `--fields …` 自选字段。）*

## 特性

- **快，而且并行。** 每个探测 —— niri IPC、硬件、字体、配置校验 —— 都互相独立并各跑一条线程，因此墙钟开销是「最慢的一项」而不是「各项之和」。端到端仍远低于十分之一秒，其中 nirifetch 自身计算的开销大致为 0。硬件信息直接取自 `procfs` 与 `sysfs`；不会去调用 `lspci`、`lscpu` 之类的命令。
- **不止混成器。** 除了 niri 会话本身，它还报告当前工作区与窗口数、当前键盘布局，以及 `niri validate` 是否接受你的配置（合法时一个安静的 `✓`，非法时一个 `✗` 加首条错误）。经典字段也一并补齐 —— 内核、运行时长、负载、Shell、内存、磁盘、已装包数、电池 —— 全部取自 procfs/sysfs；能读到终端真实配色时也会显示出来。
- **配置架构检测。** 扫描 `config.kdl` 中的 `include` 语句，报告 `Modular (N included files)` 或 `Monolithic (Single)`。备份文件和编辑器残留（`config.kdl.bak`、`.#binds.kdl`）会被过滤掉，免得一次随手 `cp` 就翻转判定结果。
- **状态栏检测。** 报告 niri 启动的状态栏或外壳 —— Waybar、Noctalia Shell、Dank Material Shell、Eww、Ironbar、AGS —— 方式是读取配置里的 `spawn-at-startup` 指令，**包括它 `include` 的每一个文件**。对于从 systemd 用户单元启动状态栏的用户，则退回到扫描 `/proc`。引用到状态栏的快捷键绑定（`spawn-sh "pkill waybar"`）绝不会被误认成状态栏本身。
- **自选排版。** `--fields os,cpu,gpu` 精确只显示你要的字段，`--short` 打印精简子集，`--no-logo` / `--ascii` / `--logo-file <path>` 把左列交给你掌控。
- **窄屏安全。** 每个值都按显示单元格宽度度量，并按优先级逐级降级后才考虑截断。任何一行都不会折行或破坏列对齐 —— 已在 46 至 220 列区间验证。
- **真实的 Nerd Font 宽度计算。** 图标码点是拿真实字体表量过的，所以无论开不开图标，标签和值都能对齐。中日韩标题按双宽字符计算。
- **无重量级依赖。** 只有 `serde`、`serde_json` 和 `colored`。没有异步运行时，没有 `sysinfo`，也没有 `lspci` 解析库。
- **永不 panic。** 每个探测各自独立失败并退化成 `Unknown`。在 niri 会话之外运行，它会打印一条友好的错误并以 `1` 退出。管道下游提前退出（`nirifetch | head -1`）同样不会 panic —— 所有输出都走能容忍 `EPIPE` 的写入路径。

## 环境要求

- Linux，且运行着 Niri 会话
- 一款 [Nerd Font](https://www.nerdfonts.com/) 用于显示图标（可选 —— 设 `NIRIFETCH_ICONS=0` 可关闭）
- `hwdata` 用于获得准确的显卡名（可选 —— 见[显卡命名](#显卡命名)）

## 安装

### 从源码构建

```sh
git clone https://github.com/504b3urd/nirifetch
cd nirifetch
cargo install --path .
```

需要 Rust 1.85 或更新版本（edition 2024）。

### Arch Linux

目前尚无 `nirifetch` 包，而且现在也没法往 AUR 发布。AUR 因一波恶意上传自 2026 年起一直处于封禁状态：新账号注册在 2026 年 6 月 15 日至 7 月 13 日期间被完全关闭，无主包认领至今仍然停摆，注册也仍被严格的反机器人措施限制。请改用本地构建安装 —— 下面这份配方就是要发布的那一份：

```sh
git clone https://github.com/504b3urd/nirifetch
cd nirifetch/packaging
makepkg -si          # -s 自动装缺失的 makedepends，-i 构建完直接装
```

`check()` 会跑完整测试套件，升级与重新构建的流程见 [`packaging/README.md`](packaging/README.md)。当然，在 Arch 上直接 `cargo install --path .` 也可以 —— `cargo` 包提供了工具链。

## 用法

```sh
nirifetch                    # 打印 fetch
nirifetch --json             # 同样的数据，JSON 格式，便于脚本处理
nirifetch --short            # 精简字段子集
nirifetch --fields os,cpu,gpu  # 只显示这些字段，保持给定顺序
nirifetch --no-logo          # 隐藏左列
nirifetch --logo-file art.txt  # 使用自定义 Logo
nirifetch --help             # 列出全部字段与环境变量
nirifetch -v                 # 打印版本号
```

想把它放进 shell 的欢迎信息？可以从 `~/.config/fish/config.fish`、`~/.zshrc` 或 niri 的 `spawn-at-startup` 里调用它 —— 它快到开销可以忽略不计。

### JSON 输出

`--json` 打印的正是排版所依据的同一个数据模型，所以两者不可能对不上：

```sh
nirifetch --json | jq -r '.bar'
# Waybar
```

缺失的值是 `null`，显卡列表缺失时是 `[]`，因此 `jq` 不需要做存在性判断。键名就是上面那些字段，外加一个 `home`（用于在终端排版里把配置路径缩写成 `~`）。

无法识别的参数属于用法错误（退出码 `2`），而不是被静默忽略 —— `nirifetch --jsno` 不该看起来像是生效了。

### 环境变量

| 变量 | 作用 |
| --- | --- |
| `NIRIFETCH_ICONS=0` | 关闭 Nerd Font 图标 |
| `NO_COLOR=1` | 关闭颜色（遵循 [no-color](https://no-color.org/) 约定） |
| `CLICOLOR_FORCE=1` | 即使 stdout 不是 TTY 也强制输出颜色 |
| `NIRI_SOCKET` | niri IPC socket 的路径 |

## 工作原理

| 模块 | 职责 |
| --- | --- |
| `src/main.rs` | 命令行解析、环境检测与流程编排（并行探测） |
| `src/ipc.rs` | 通过 `niri msg --json` 获取 niri 动态状态，以及 `niri validate` |
| `src/config.rs` | `config.kdl` 的路径、大小、行数、架构检测与 `include` 展开 |
| `src/bar.rs` | 状态栏 / 外壳：先查配置里的 `spawn-at-startup` 指令，再查 `/proc` |
| `src/hardware.rs` | CPU 来自 `/proc/cpuinfo`，显卡来自 `/sys/class/drm` + `pci.ids` |
| `src/system.rs` | 内核、运行时长、负载、Shell、内存、磁盘、包、电池 |
| `src/font.rs` | 终端来自进程链，字体与调色板来自终端自身的配置 |
| `src/sys.rs` | 共用底层原语：带超时的子进程、限长读文件 |
| `src/ui.rs` | Logo、分栏排版、字段选择、配色、调色板、JSON 渲染与帮助文案 |

每一次子进程调用都包在超时里，所以无响应的混成器或卡死的 D-Bus 守护进程都拖不住这个程序。

### 显卡命名

显卡名解析自 `pci.ids` —— 也就是 `lspci` 读的那个数据文件 —— 分三步，越靠前越可信：

1. **方括号名。** `pci.ids` 约定把商用名写在方括号里（`Rembrandt [Radeon 680M]`）。它的 1146 条 AMD 设备条目中有 818 条如此。直接采信。
2. **子系统名**，但仅当它确实像个显卡名时。笔记本上的子系统条目通常是**整机型号** —— 某个子系统 ID 可能解析出 `Z50-75` 这样的东西，那是一台笔记本，不是图形芯片。把这种主板名当成显卡显示比不显示还糟，所以一律拒绝。
3. **代号别名。** 较新的 APU 在上游只有代号（`HawkPoint1`、`Phoenix1`、`Raphael`），会被映射成商用名。

> **第 3 步是推断，不是查表。** 同一个代号往往横跨多个 SKU：`HawkPoint1` 视具体型号既可能是 Radeon 780M 也可能是 Radeon 760M，而 PCI ID 区分不了它们。表中为每个代号选取了出货量最大的名字，所以低配 SKU 可能读起来偏高。非常欢迎带着更可靠来源的补丁。

未安装 `hwdata` 时，第 1、2 步不可用，名字退化为代号。

## 开发

```sh
cargo test        # 193 个测试，不需要网络或特殊硬件
cargo clippy --all-targets
cargo fmt
```

各探测层都写成了针对「捕获到的文件内容」的纯函数（`parse_cpuinfo`、`lookup_many`、`split_subsystem`、`startup_programs` …），所以它们是拿真实感样例来测的，而不是拿测试机恰好装了什么硬件来测。

`include` 展开复用了架构检测器的同一套行级扫描，并在深度、文件数和总字节数上都设了上限 —— 一个自我引用的配置会终止而不是陷入循环。

## 关于本项目是如何构建的

这个项目源于一个个人痛点：我想要一个真正懂 niri 的 fetch 工具，而现存的东西都不太符合。它是在大量 AI 辅助下构建的 —— 结对设计，每一个硬件探测都在真实机器的 `procfs`/`sysfs` 输出上验证过，本文档里的每一项断言也都是实测而非假设。

这条路让它很快达到了可用且有测试覆盖的状态，但也意味着这些代码只经过了「一位作者」程度的审查。**它尚未在长尾的硬件、终端和发行版组合上经过实战检验。**

### 诚征维护者

如果你是使用 niri 的 Rust 开发者，我们真心欢迎贡献 —— 尤其是：

- **为你的硬件补全显卡命名。** 跑一下 nirifetch 看看显卡那一行。如果它是错的，或者只显示一个光秃秃的代号，那么带上你的 `pci.ids` 条目给代号表提个补丁 —— 五分钟的修复，能帮到所有用这颗芯片的人。
- **kitty 之外终端的终端与字体检测。** 目前只实现了 kitty 的配置格式；`gsettings`/`fc-match` 回退拿到的是*系统*字体，未必是你的终端实际渲染的那个。
- **非 AMD 显卡。** Intel Arc 和 NVIDIA 各有各的命名怪癖。
- **AUR 打包。** `PKGBUILD` 已经写好待用，但 AUR 自 2026 年起的封禁让发布暂时无从谈起（见 [Arch Linux](#arch-linux)）。如果你是 AUR 打包维护者，等它解封后这是个很好接手的小包。
- **任何其他问题，欢迎提到 [issue tracker](https://github.com/504b3urd/nirifetch/issues)。**

最有用的反馈是附上 `nirifetch` 的输出以及你的 `lspci -nn`。

## 许可证

MIT —— 见 [LICENSE](LICENSE)。
