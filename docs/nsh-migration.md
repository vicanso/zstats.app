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
- **1c 状态（**已完成**，2026-10-24）。** 分两步。**1c-1**：gpui 的界面句柄（每页的滚动句柄、行区域的滚动句柄、两个搜索输入框、完整列表的 `ListState`、三个窗口句柄）搬出状态，放进 gpui 前端的 `gpui_ui.rs`——一个主线程上的 thread-local，因为视图是只接收状态的纯函数，手里没有 `App`；视图签名一个没改。状态里留下两个前端都要知道的：搜索开没开、写了什么（`open_/close_/set_*_filter_text`），完整列表显示的是哪一组行（`FullScanData::rows`，gpui 的列表只在它变了时回到顶部），窗口位置改用 `geom::Rect`。**1c-2**：状态方法从 `Context<Self>` 改接 `&mut dyn Effects`（`state/effects.rs`：`changed`、`background`、`sync_tray`、`fit_panel`、`storage_window_open`）；gpui 前端为 `Context<ZStatsAppState>` 实现它，所以视图里几百处 `state.foo(.., cx)` 一处没改。19 处 `cx.spawn` 改成后台线程加 `Reply`：16 处一次性任务，4 处边做边汇报（磁盘分析三处、重复文件一处），下载更新改成一个任务里依次汇报进度、失败、开始安装、装完。采集节奏交给状态自己持有，不再读 gpui 全局。状态模块整体共享，只开 `nsh` 也能编译；一条编译期断言保证 `ZStatsAppState` 是 `Send`。
- **1d 采集、托盘、通知（**已完成**，2026-10-24）。** `metrics::spawn` 起采集线程，返回 `Collector`（采集节奏、tick 的 channel、启动时读到的 `config.toml`）；每个 tick 对状态做的事——`ingest`、三道横幅闸门和它们的日志、几类观察者的提示、托盘、面板高度——收进共享的 `ZStatsAppState::on_tick`，异常进程的扫描收进 `start_abnormal_scan`（后台线程加 `Reply`）。gpui 的 `metrics::start` 只剩接线。`notify::start` 不再接 `App`，返回横幅点击的 channel，点了打开什么由前端决定。`tray.rs` 整体共享：托盘句柄从 gpui 的 `Global` 改成主线程上的 thread-local，`sync`、`anchor`、`rebuild_menu` 不再要 `cx`；点击和菜单通过 `tray::start(deliver)` 交来的处理函数变成 `TrayAction` 交给前端。事件改用 tray-icon 的 `set_event_handler`（一次性设定，第一次事件之前必须设好），原来两条阻塞在接收端上转发事件的线程没了。Linux 上等托盘宿主出现的那条线程不能碰主线程上的托盘，改成发 `TrayAction::HostAppeared` 请前端在主线程重建，重试节奏照旧（0 / 2 / 10 秒），靠一个原子标志知道建没建成。NSH 宿主删掉了自己手写的托盘，改用共享的：同样的脸、菜单和点击处理，菜单里的“磁盘空间”在 NSH 里暂时只记一行日志（阶段 4）。

**验收**：gpui 版行为不变（测试全过，关键页面截图与改动前一致）；`nsh` 构建能用上完整的核心。

**1d 结果**：默认构建 438 个测试、只开 `nsh` 的构建 344 个（托盘的测试现在不带 gpui 也在跑）；`make lint` 两种配置都过。`tray.rs` 只在 Linux 上编译的部分（等托盘宿主的线程、`host_appeared`、墨色），用对调条件编译的办法在 Mac 上过了 clippy 和测试（zbus 临时加进副本的 macOS 依赖）；直接对 Linux 目标交叉检查不可行，`ring` 要 Linux 的 C 编译器。运行时两个前端都实测过，点击都是进程内把鼠标事件交给状态项自己的窗口，不动用户光标——NSH 的 probe 为此加了 `tray` 命令。顺带发现：接着外接显示器时，每块屏的菜单栏上都有一个状态项窗口，只有一个是真的（命中 `TaoTrayTarget`），其余是系统画的副本（`NSStatusItemReplicantImageView`），点副本什么也不会发生。NSH：第一次点击隐藏面板，第二次把它挂在图标正下方（状态项 x=1357、宽 34，面板 x=1195），并成为 key 窗口。gpui（临时钩子、隔离 HOME、`config.toml` 里 `disk = 1.0`）：第一个 tick 就报了启动盘告警（日志来源变成 `zstats::state`，横幅判定 delivered），托盘自动换成磁盘脸，标题 “88G”；两次托盘点击分别隐藏、显示面板，没有 `RefCell already borrowed`。

**1c 结果**：默认构建 438 个测试、只开 `nsh` 的构建 340 个（状态模块的测试现在不带 gpui 也在跑）。运行时用带测试钩子的 gpui 版（临时 worktree、隔离 HOME、不抢焦点）逐项验证：搜索框打开后模拟键盘输入，经输入框的订阅写回状态；完整列表的后台扫描落地并按搜索过滤（806 个进程留下 6 个），截图里输入框和虚拟列表都在；重置清掉搜索和列表；硬件页的空间探测、网络页的监听（裸二进制按预期是“需要安装版”）和流量、历史读取、静默更新检查都走新的后台路径完成；磁盘分析（6 个目录）和重复文件（1 组）两种边做边汇报的任务都正常收尾；切到告警页同步托盘无误。

**1a / 1b 结果**：默认构建 438 个测试（多出的是颜色、几何类型自身的测试，其中两条拿 gpui 自己的构造函数和 `Bounds` 逐一比对，保证换类型后颜色和位置一分不差），只开 `nsh` 的构建 288 个（图表、配色、Omarchy、`placement` 的测试现在不带 gpui 也在跑）。`placement.rs` 只在 Linux 上编译的那段 gpui 分支，用对调条件编译的办法在 Mac 上编译过。NSH 前端实测：没有图标位置时放在主屏右上角；用 probe 的 `anchor` 命令模拟一次托盘点击，面板落在算法给出的位置（这台机器当时接着外接显示器）；改用 `palette` 后截图与之前一致。

## 阶段 2：NSH 宿主

- **2a 共享状态接进 NSH（**已完成**，2026-10-24）。** `nsh/store.rs`：一个 `ZStatsAppState` 放在静态的 `Mutex` 里，所有线程都经 `store::read` / `store::update` 取用。`update` 期间状态方法要的副作用（`Effects`）先记下来，放锁之后再做：通知订阅者、在主线程换托盘（`tray::look` 持锁时算好，`tray::wear` 交给主线程——`tray.rs` 为此把 `sync` 拆成两半）、面板高度跟随。后台任务的改动在任务自己的线程上持锁应用，顺序即发送顺序。`std` 的锁不可重入，从 `update` / `read` 里面再取一次会死锁，这里改成 panic 并说明原因（有测试）。引擎这边：根组件里一个任务等 `store::subscribe` 的 channel（容量 1、不等待发送，所以一串变化只唤醒一次），面板显示时把一个 revision signal 加一，凡是读状态的闭包都先读它（`panel.rs` 的 `Live`）——相当于 gpui 的“每个 tick 整个面板重绘”，只是重算的范围缩小到变了的那些；面板隐藏时什么都不重算，显示时补发一次。宿主照 gpui 版的启动顺序：恢复今天的告警和上次的标签页、托盘、横幅点击（落到告警页）、`autostart::refresh`、`active::start`、采集线程（tick 在专门的线程上持锁走 `on_tick`）、异常进程扫描、清理安装镜像。显示和隐藏补上了和 gpui 版一样的记账：采集节奏、所在标签页的进入工作、看过告警、重置临时视图，以及点托盘时“刚自动隐藏就别再打开”的宽限（`TOGGLE_GRACE` 移进 `state`，两个前端共用）。占位页上加了几行实时数字（CPU、内存、告警数、当前标签页、第几次打开），用来看数据流。
- **2b 面板行为（macOS）**：高度随 Overview 的形态和提示行变化（`fit_panel`）、拖动改大小并记住、Pin、外观跟随。
- **2c 设置窗口、磁盘空间窗口的外壳和键盘快捷键。**
- **2d Linux**：GTK 4 面板加 `gtk4-layer-shell` 锚在右上角，ksni 托盘，`ipc.rs` 的单实例和 `--toggle`。可以先用 `make linux-check`（Apple 的 `container`，arm64）在本机编译，镜像要加 GTK 4 开发包；CI 同样要装；Omarchy 真机验收。

**2a 结果**：默认构建 438 个测试、只开 `nsh` 的构建 346 个。只开 `nsh` 的 debug 构建在隔离 HOME（`config.toml` 里 `disk = 1.0`）下实测：第一个 tick 就报了启动盘告警（横幅判定 delivered），托盘换成磁盘脸并带上 “88G”——这一步是持锁算好、交回主线程换的；面板上的 CPU 和内存随 tick 变化（两张间隔 4 秒的截图 14.8% → 20.9%）；probe 点托盘隐藏、再点显示（挂在图标下、成为 key），显示后内存那一行立即刷新、“第几次打开”从 0 变 1（隐藏时重置了临时视图）；probe 的新命令 `banner` 模拟横幅点击，面板切到告警页。日志里没有 ERROR / WARN。

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
| 1 核心 | 完成（2026-10-24） |
| 2 宿主 | 2a 完成；2b、2c、2d 未开始 |
| 3 基础件 | 未开始 |
| 4 逐页 | 未开始 |
| 5 切换 | 未开始 |
