# 从 gpui 迁到 NSH：分阶段计划

2026-10-24 决定把界面层从 gpui（gpui-pre + gpui-kit）换成 [NSH](https://nsh.dev) 的原生后端：macOS 上是 AppKit，Linux 上是 GTK 4。可行性原型在独立仓库 `~/github/nsh-panel-spike`，它的 `FINDINGS.md` 是这份计划的依据：托盘、非激活面板、毛玻璃、输入框焦点、弹出菜单都在 AppKit 宿主加 NSH `AppView` 上跑通了；修掉 canvas 的一个默认设置后，面板显示时 CPU 0.55%、内存 26 MB，gpui 版是 1.20%、62 MB。

## 已经定下的事（2026-10-24）

- **渐进替换。** 先把业务状态和逻辑从 gpui 里剥出来，gpui 版照常发布；NSH 版作为第二个前端逐页移植，功能对齐后再切换默认、删掉 gpui。每一步都能编译、能测，main 不会长期停滞。
- **macOS 和 Linux 一起迁。** Linux 宿主是 GTK 4 窗口加 `gtk4-layer-shell`。这台 Mac 上编译不了 GTK，Linux 部分靠 CI 编译、在 Omarchy 真机上验收；和 `omarchy-port.md` 一样，没在真机上看过的就记为“未验证”。
- **最低系统版本升到 macOS 14。** NSH 的要求。从切换到 NSH 的那个版本开始生效，在那之前的 gpui 版本仍支持 macOS 12。

## 两个前端怎么并存

**一个 crate，两个可叠加的 feature**：`gpui`（默认）和 `nsh`。

- 与界面无关的模块两边共享，只编译一次；`i18n!` 仍在 crate 根上只初始化一次。
- 依赖 gpui 的模块（`views/`、`state/`、`window_ext.rs` 等）只在 `gpui` 打开时编译。原来 `main.rs` 里的 gpui 宿主搬进 `src/gpui_app.rs`，其他模块引用的根级函数（`crate::open_settings_window` 等）在 crate 根上重新导出，路径不变。
- NSH 前端在 `src/nsh/`。
- 两个都打开时（`make lint` 的 `--all-features`），用环境变量 `ZSTATS_UI=nsh` 在运行时选 NSH，默认仍是 gpui，方便迁移期间对照。发布构建只用默认 feature。
- 只开 `nsh` 时，共享模块里只有 gpui 前端调用的函数会报 dead code，迁移期间在 crate 根上对这种构建放宽这一条 lint。

## NSH 的结构约束

- **每个 `AppView` 一个引擎线程。** 面板、设置窗口、磁盘空间窗口会在三个不同的线程上运行界面代码，所以共享状态必须是线程安全的（`Send`），每个窗口订阅它的变化。gpui 版的状态是主线程上的 `Entity`，这是阶段 1 要解决的核心问题。
- **窗口和应用归自己管。** AppKit 一侧（托盘、`NSPanel`、显示/隐藏、定位、Dock 策略）在主线程上直接写，不再需要 `dock.rs` 的 swizzle、`after_app_borrow`、事后补 style mask 这些绕路。
- **原型里发现的 NSH 0.2.0 问题**，迁移时都要带上绕法，同时向上游提：
  1. canvas 默认异步绘制，每次重绘临时占用约 97 MB GPU 内存 → 显示后把 canvas 图层改回同步绘制。
  2. `AppView` 容器层总是不透明，挡住毛玻璃 → 显示时清掉容器层底色。
  3. 非激活面板会自动成为 key 并截走按键 → `setBecomesKeyOnlyIfNeeded(true)`。
  4. Rust 侧没有原生 `NSMenu` 弹出 → 用 `nsh_base::menu`，或者自己用 objc2 弹 `NSMenu`。
  5. Tooltip 不按窗口宽度折行 → 自己写带最大宽度的元素型 tooltip（相当于 `widgets::wrap_tooltip`）。

## 阶段 0：双前端骨架（**已完成**，2026-10-24）

- `Cargo.toml` 加 `gpui` / `nsh` 两个 feature，gpui 系依赖改为可选。
- `main.rs` 拆成共享部分和 `gpui_app.rs`；依赖 gpui 的模块按 feature 编译；`assets.rs`、`metrics.rs` 里依赖 gpui 的几段单独隔开。
- `src/nsh/`：原型的 AppKit 宿主（托盘、面板、自动隐藏、定位）加一个占位页面。Linux 的 NSH 宿主在阶段 2，这一阶段 Linux 上 `nsh` 构建只是一个说明尚未实现的入口。
- `make nsh` 运行 NSH 前端；CI 加 `nsh` feature 的编译和测试。

**验收**：默认构建行为不变，测试、`make lint` 全过；`cargo run --no-default-features --features nsh` 在 macOS 上能启动，菜单栏有图标、点开有面板。

**结果**：
- 默认构建 434 个测试、只开 `nsh` 的构建 251 个测试全过；`make lint` 对“两个前端都开”和“只开 NSH”两种配置都过；CI 固定的 Rust 1.98.1 上也能编译。
- 只开 `nsh` 的 debug 构建在隔离的 HOME 下启动：菜单栏有图标，面板显示占位页且没有抢焦点，日志写进隔离目录；用 probe 点 Quit，引擎线程把动作交回主线程后进程正常退出。
- 顺带发现：`objc2-app-kit` 的 `libc` feature（`NSRunningApplication` 的 pid 接口要用）一直没在 `Cargo.toml` 里声明，是 gpui 的依赖树碰巧打开的。只开 `nsh` 构建时才暴露出来，已补上。
- 提前做了一小块 1d：托盘图标的 SVG 光栅化和告警小圆点移到共享的 `glyph.rs`，墨色由调用方传入；`tray.rs` 和 NSH 宿主都用它。
- 未做：gpui 版改动后没有实际启动过。debug 构建启动时会 `cx.activate(true)` 抢焦点，这次没跑；改动只是把代码原样搬进 `gpui_app.rs` 并加了 feature 门，靠编译和测试保证。Linux 上 `nsh` 构建只是一句提示，等阶段 2。

## 阶段 1：抽出与界面无关的核心

- **1a 纯计算移出视图（**已完成**，2026-10-24）。** 跨页面共用的图表几何（`overview.rs` 的分桶、坐标轴规则、参考线与刻度的时机、柱槽、单调三次曲线）连同 13 个测试移到共享的 `chart.rs`。**范围调整**：各页自己的纯逻辑（`processes::fit_rows`、`overview::notice` 等）不在这一步搬，而是在阶段 4 移植那一页时一起搬——现在就把每页都拆一遍，等于提前改动全部 gpui 页面，风险大而没有收益。
- **1b 颜色与几何（**已完成**，2026-10-24）。** 共享的 `color.rs`（自有 `Rgba`，字段和 `rgb`/`rgba` 构造函数与 gpui、NSH 的同名同义）和 `palette.rs`（配色方案的选择、全部 token 的取值、Omarchy 的派生，以及原来写在 `theme.rs` 里的设计理由）；`theme.rs` 留在 gpui 前端，变成一层转换（函数名与返回类型不变，视图零改动），外加为 gpui-kit 生成的主题 JSON。共享的 `geom.rs`（点、尺寸、矩形，坐标约定与 gpui 相同，`contains` 的边界规则也一样）；`placement.rs` 改用它并整体共享，屏幕列表和兜底值改为参数（`below_tray`），gpui 的取法留在原地的 `bounds_below_tray`；`TrayAnchor` 随之搬进 `placement.rs`，`state` 重新导出。`omarchy.rs` 随颜色类型一起共享。NSH 宿主已改用 `placement::below_tray` 和 `palette`，删掉了原型时期的单屏定位和手写色值。
- **1c 状态。** `ZStatsAppState` 去掉 gpui：方法不再接 `Context<Self>`，改接一个副作用接口（通知变化、派发后台任务、窗口操作）；输入框实体、滚动句柄、窗口句柄移到 gpui 前端自己的结构里；状态变成 `Send`，可以放进 `Arc<Mutex<_>>` 给多个线程用。
- **1d 采集、托盘、通知。** `metrics.rs` 把 tick 交给与前端无关的 channel；`tray.rs` 里决定托盘外观的逻辑（`face_for`）与 gpui 分开；`notify.rs` 的横幅点击改成回调。

**验收**：gpui 版行为不变（测试全过，关键页面截图与改动前一致）；`nsh` 构建能用上完整的核心。

**1a / 1b 结果**：默认构建 438 个测试（多出的是颜色、几何类型自身的测试，其中两条拿 gpui 自己的构造函数和 `Bounds` 逐一比对，保证换类型后颜色和位置一分不差），只开 `nsh` 的构建 288 个（图表、配色、Omarchy、`placement` 的测试现在不带 gpui 也在跑）。`placement.rs` 只在 Linux 上编译的那段 gpui 分支，用对调条件编译的办法在 Mac 上编译过。NSH 前端实测：没有图标位置时放在主屏右上角；用 probe 的 `anchor` 命令模拟一次托盘点击，面板落在算法给出的位置（这台机器当时接着外接显示器）；改用 `palette` 后截图与之前一致。

## 阶段 2：NSH 宿主

- **macOS**：托盘（复用 `tray.rs` 的外观逻辑）、面板（挂在托盘下、自动隐藏、Pin、拖动改大小、高度随 Overview 形态和提示行变化）、设置窗口、磁盘空间窗口、外观跟随、横幅点击落到告警页、键盘快捷键。
- **Linux**：GTK 4 面板加 `gtk4-layer-shell` 锚在右上角，ksni 托盘，`ipc.rs` 的单实例和 `--toggle`。CI 装 GTK 4 开发包编译；Omarchy 真机验收。
- **多窗口共享状态**：`Arc<Mutex<_>>` 加每个引擎一个订阅任务。

## 阶段 3：界面基础件

主题（Golden Dark / Golden Light / Classic 两套 / Omarchy）、字体（界面字体的等宽数字、等宽字体）、`card`、`big_number`、带最大宽度的 tooltip、`MenuTrigger`、选项 chip、`list_shell`、`expand_mark`、应用图标（`appicon`）、确认对话框。规则照 `.claude/skills/ui-polish`。

## 阶段 4：逐页移植

Overview（短、长两种形态）→ Apps → Processes → Hardware → Network → Alerts → History → 设置窗口（5 页）→ 磁盘空间窗口（目录图、重复文件）。

每页的验收：和 gpui 版并排截图对照；所有按实测写死的高度重新量（539 / 828 / 775、`APP_ROW`、`fit_rows`、`NOTICE_HEIGHT`），因为文字排版换成了 CoreText / Pango。

## 阶段 5：切换与收尾

- `nsh` 设为默认 feature；`osx_minimum_system_version` 改为 14.0；打包、签名、发布流程跟着改；Linux 安装脚本声明 GTK 4 依赖。
- 更新 `CLAUDE.md`、`README.md` / `README-zh.md`、`docs/design.md`、`.claude/skills/ui-polish`。
- 删掉 gpui 前端、`patches/gpui-pre-linux`、`dock.rs` 和其他只为 gpui 存在的绕路代码。

## 进度

| 阶段 | 状态 |
|---|---|
| 0 双前端骨架 | 完成（2026-10-24） |
| 1 核心 | 1a、1b 完成；1c、1d 未开始 |
| 2 宿主 | 未开始 |
| 3 基础件 | 未开始 |
| 4 逐页 | 未开始 |
| 5 切换 | 未开始 |
