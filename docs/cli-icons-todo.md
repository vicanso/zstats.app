# CLI 与无 Bundle 进程图标 · TODO

记录日期：2026-10-31

## 背景与现状

目前 `zstats` 的程序图标（`src/appicon.rs`）只认真实的 macOS `.app` bundle：

1. 通过 `proc_pidpath(pid)` 拿到进程可执行文件的绝对路径；
2. 查找路径上最外层带 `Contents/Info.plist` 且声明了 `CFBundleIconFile` 或 `CFBundleIconName` 的 `.app` 目录；
3. 调用 `NSWorkspace.iconForFile:` 栅格化为 64×64 PNG，按 bundle 缓存整个会话；
4. 凡是不在 `.app` 内的进程（或未声明自有图标的系统代理），一律回退为首字母方块（`widgets::monogram`）。

这套规则对 GUI 应用及其 helper（如 `Google Chrome Helper` 归属到 Chrome 图标）很准确，但有两个明显空白：

- **macOS 上的命令行工具与开发工具链全是首字母方块**：`gitui`、`lazygit`、`cargo`、`rustc`、`btop`、`nvim`、`node`、`bun`、`python`、`docker`、`claude`、`tmux` 等裸二进制没有 `.app` 包裹，在 Processes、Network 流量卡、History，以及 Apps 页被 `trend::tree_face` 识别出前台作业的裸终端树（如 `login` 树显示为 `cargo` 或 `gitui`）上，全部只能显示单个字母。
- **Linux 平台没有任何图标**：`appicon::cached` 在非 macOS 下恒返回 `None`（`src/appicon.rs`），所有行都是首字母方块。

## 参考：Rex 的做法

观察 `/Applications/Rex Beta.app` 可以看到它是如何给 `gitui` 等命令行程序显示图标的：

1. **内置 CLI 图标资产包**：在 `SuperBlockIcons_SuperBlockIcons.bundle/Contents/Resources/Assets.car` 中打包了 141 个 `block-glyph-*` 图标（涵盖常见 CLI、编程语言、云厂商与 AI 工具）。
2. **命令名 → 图标的归类映射表**：
   - **直接同名映射**：`btop`、`htop`、`nvim`、`hx`、`tmux`、`fish`、`bash`、`brew`、`claude`、`codex`、`gemini`、`docker`、`go`、`bun` 等直接对应各自的图标。
   - **生态归并映射**：把细分 CLI 工具映射到所属技术栈的图标上，避免为成百上千个子命令单独画图：
     - Git 生态：`git`、`gitui`、`lazygit`、`tig`、`gh`、`delta`、`jj` → Git / GitHub 图标
     - Rust 生态：`cargo`、`rustc`、`rustup`、`clippy-driver`、`rust-analyzer` → Rust 图标
     - JS / TS 生态：`node`、`npm`、`npx`、`pnpm`、`yarn`、`bun`、`bunx`、`deno` → Node / Bun / Deno 图标
     - Python 生态：`python`、`python3`、`pip`、`uv`、`uvx`、`pytest`、`ruff` → Python 图标
     - 编译与构建：`go`、`gopls`、`clang`、`clang++`、`cc`、`ld`、`make`、`cmake`、`ninja`、`bazel` → 对应语言/工具链图标
     - 容器与云：`docker`、`kubectl`、`helm`、`terraform`、`wrangler`、`cloudflared` → Docker / K8s / Cloudflare 图标

## 后续 `zstats` 可考虑的方案

### 1. 查找优先级

保持「真实 `.app` 优先、CLI 图标映射兜底、首字母方块垫底」的三级回退：

1. **`.app` bundle 图标（macOS）**：现有 `appicon::icon_for_pid` / `icon_for_record` 逻辑不变。只要进程跑在声明了图标的 `.app` 内，永远用该 `.app` 的真实图标。
2. **内置 CLI / 工具链图标映射（macOS & Linux 通用）**：当进程不在 `.app` 内时，按匹配名查找内置映射表：
   - Processes / Network / History：按进程可执行文件名（`name`）匹配；
   - Overview / Apps：若树根无 `.app` 图标，按 `trend::tree_face` 选出的前台作业名（`face.title`，如终端里跑的 `cargo`、`gitui`）或树根名匹配。
3. **首字母方块（`widgets::monogram`）**：未命中映射表的长尾进程仍显示首字母方块，不使用系统的通用可执行文件图标。

### 2. 资源形态与体积控制

- **复用现有 SVG 渲染链路，不引入大体积位图包**：
  `zstats` 已经用 `rust-embed` + `data:image/svg+xml` + NSH `tint_color` 渲染内置图标（`src/nsh/widgets.rs` 的 `icon_uri`）。挑 40–60 个高频开发工具与 CLI 生态的单色 SVG 放进 `assets/icons/cli/`，总体积只需几十 KB。
- **表驱动映射**：
  用一张轻量的静态表或内嵌配置（把 150–200 个常见可执行文件名映射到这 40–60 个图标上），维护成本低，且 macOS 与 Linux 两端直接共享。

### 3. 视觉呈现

- `.app` 图标是 `22×22` 的完整应用图标；如果把单色 CLI SVG 直接裸画成 `22×22` 会显得过大且与旁边的应用图标风格割裂。
- 应当沿用 `widgets::monogram` 的容器几何（`22×22`、`rounded(6.)`、`palette::track` 背景色），把原来的 11pt 首字母替换成居中的 `13–14pt` SVG 图标（着 `palette::text_muted`）。这样行高、对齐与视觉重量完全不变，只是把原本需要逐个辨认的字母换成了眼熟的工具符号。
