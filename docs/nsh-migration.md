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
- **2b 面板尺寸（macOS，**已完成**，2026-10-24）。** 面板的大小照 gpui 版的规则走，用的是同一批共享答案：Overview 的形态和提示行（`placement::window_size`；提示行的判定从 `views/overview.rs` 挪进共享的 `notice.rs`——它决定面板高度，两个前端的宿主都要问；`notify::subject_title` 与原来视图里的 `subject_name` 逐字等价，直接复用）、拖动过的尺寸（`prefs::panel_width` / `panel_height`）、提示行出现或消失时按 `placement::notice_step` 增减高度并保持上边缘（`set_size_keeping_top` 从 `window_ext.rs` 挪进共享的 `appkit.rs`）、短形态的最小高度。窗口一创建就是可拖动改大小的（gpui 版要在第一帧之后补 style mask）；拖动中、拖动结束分别是面板委托自己的 `windowDidResize:`、`windowDidEndLiveResize:`：拖动中跨过长形态的高度就切形态，拖动结束记下宽度和当前形态的高度（不含提示行）。面板每次移动或改大小都把位置和缩放写回状态，页面靠它算列表能放几行。显示时按当前形态应有的尺寸重新定位。Pin 早已遵守（`prefs::pinned`），Pin 的按钮和 ⌘P 跟着底栏和快捷键走（2c / 阶段 4）；外观跟随归到阶段 3 的主题。
- **2c 两个普通窗口和快捷键（**已完成**，2026-10-24）。** 设置窗口和磁盘空间窗口在 NSH 宿主里各是一个 `NSWindow` 加自己的 `AppView`（`nsh/windows.rs`），根组件是占位页（`nsh/settings.rs`、`nsh/storage.rs`），各窗口根组件共用的部分——宿主通道 `HostAction`、跟随状态的 `Live`（订阅条件改成参数：面板是“在屏幕上”，普通窗口是“开着就算”）、几样 Golden Dark 的取值——从 `panel.rs` 抽到 `nsh/root.rs`。沿用 gpui 版的约定：再开一次是把开着的那个提到前面，关闭就真关掉，下次重建（NSH 重建 `AppView` 的内存原型量过，几次之后就封顶）；关闭时窗口和委托推迟到主队列的下一轮才释放，不能在它自己的 `windowWillClose:` 里放掉最后一个引用。设置窗口打开时、重新成为 key 时都 `autostart::refresh`；磁盘空间窗口打开时 `reset_storage_views`、铺满可见区域（缩放按钮回到 720×480），开着的时候告诉状态（`storage_window_open`）。三个窗口尺寸常量从 `gpui_app.rs` 挪进 `placement.rs`。快捷键用 NSH 的 `on_key`：面板 `mod+1`…`mod+7` 切页、`mod+p` Pin；磁盘空间窗口 `mod+1` / `mod+2` 切页、`mod+r` 重跑；两个普通窗口 Escape 关闭。`mod` 在 macOS 是 ⌘、别处是 Ctrl，对应 gpui 版的 `secondary-`。宿主自己装了主菜单：菜单栏应用看不到菜单栏，但 key equivalent 照样生效——⌘Q、⌘W（`performClose:`），以及编辑菜单，没有它文本框里的 ⌘C / ⌘V / ⌘Z 都不起作用。托盘菜单的“磁盘空间”和占位面板上新加的两个按钮通往这两个窗口。
- **2d Linux 宿主（GTK 4，**已写完、只编译过**，2026-10-24）。** `nsh/linux.rs`：GTK 主循环上的面板、两个普通窗口和几条入口（托盘、键位绑定走的 socket、横幅点击）。做法和理由都照 gpui 版的 Linux 路径，用的是同一批共享部分：面板是 overlay 层的 gtk4-layer-shell 表面，锚在右上角，边距和命名空间沿用（`PANEL_MARGIN_*` 挪进 `placement.rs`，命名空间仍是 `zstats`，Hyprland 的 `layerrule` 靠它匹配）；合成器没有 layer shell 时退回普通窗口。键盘焦点离开、指针离开都会让面板自动隐藏，宽限照旧（`REVEAL_GRACE`、`POINTER_LEAVE_GRACE` 挪进 `state`）。和 gpui 版不同的是隐藏后不销毁，GTK 可以把 layer 表面取消映射再映射回来。layer 表面不能拖动，所以面板用设计尺寸，只跟着 Overview 形态和提示行变。`ipc.rs` 改成共享模块（和 `tray::start` 一样通过回调交出调用），ksni 托盘、socket、横幅点击都经 `glib::idle_add_once` 回到主循环。依赖：`nsh-platform-linux`（不带 JS）和 `gtk4-layer-shell` 0.8.1，与 NSH 共用一份 `gtk4` 0.11。各窗口的根组件（`panel.rs`、`settings.rs`、`storage.rs`、`root.rs`）不再限于 macOS。CI 的 Linux job 装上 `libgtk-4-dev`、`libgtk4-layer-shell-dev`。`make linux-check` 的容器换到 Debian trixie（`nsh-platform-linux` 要求 GTK 4.14，bookworm 只有 4.8），加了 GTK 4 开发包和 clippy，并对 NSH 构建跑 `--deny=warnings`。

**2d 结果**：只编译过，没有在任何合成器上跑过。`make linux-check`（arm64 Debian trixie 容器）里：默认构建 432 个测试全过（只剩原有的 5 条 Linux 死代码警告）；只开 `nsh` 的构建 `clippy --deny=warnings` 通过，345 个测试全过。GTK 宿主就是在这里第一次在 Linux 上编译通过的。macOS 两种配置仍是 438 / 347。顺带修了三处只在容器里才暴露的问题：镜像和 vendor 目录只在缺失时才建，导致一直用旧的 bookworm 镜像（GTK 4.8）；`updater` 的“不可写目录”测试以 root 运行时不成立，现在跳过；`fullscan` 的两条测试在只有 shell、cargo 和测试本身三个进程的容器里分不清“截断”和“本来就少”，现在测试期间多挂两个 `sleep` 子进程。**未验证**：layer 表面的显示和定位、两种自动隐藏、`set_default_size` 对已映射的 layer 表面是否生效（提示行出现时面板该长高 46）、托盘和 `--toggle`——都要在 Omarchy 上看。

**2c 结果**：默认构建 438 个测试、只开 `nsh` 的构建 347 个，release 构建无警告。只开 `nsh` 的 debug 构建在隔离 HOME 下用 probe 实测：`key mod+3` 切到 Processes、`key mod+p` 打开 Pin（都写进了 `app.toml`）——probe 新加的 `key` 把按键投进应用的事件队列，因为 NSH 的快捷键是在本地事件监视器里匹配的，直接 `sendEvent:` 给窗口它看不到；设置窗口 507×620（加标题栏 652）居中打开，Escape 关掉；磁盘空间窗口打开即铺满可见区域（1845×1050），`mod+2` 切到 Duplicates（截图可见）；点面板上的 Settings 按钮，设置窗口重建出来（新窗口号），再点一次只是提到前面；开着窗口退出，日志里没有 ERROR / WARN。**一个只能手动验证的地方**：macOS 14 起应用激活是协商式的，只有回应用户自己的输入时 `activate()` 才会被允许。probe 合成的事件不算，所以实测中新窗口打开了却不是 key 窗口（前台还是终端），probe 的 `key` 因此可以指定目标窗口；⌘W 走菜单要求应用在前台，同样没能自动验证。真人点击面板或托盘菜单是真实输入，预期能激活，需要手动确认一次。

**2b 结果**：默认构建 438 个测试、只开 `nsh` 的构建 347 个。只开 `nsh` 的 debug 构建在隔离 HOME（`disk = 1.0`）下用 probe 实测，每一步都和 gpui 版的预期一致：第一个 tick 报出启动盘告警后，短形态多出提示行，面板从 539 变成 585，上边缘不动（505+539 = 459+585）；`banner` 切到告警页、告警看过之后，下一个 tick 面板缩回 539，并挂到图标下；`detail 1` 切长形态是 828；probe 新加的 `drag 400 900`（改尺寸后发出 `NSWindowDidEndLiveResizeNotification`，和真人松手时一样）之后，`app.toml` 里写进了 `panel_width = 400`、`panel_height_detail = 900`；`detail 0` 切回短形态是 400×539（宽度沿用）；截图里 NSH 的内容跟着窗口变宽。**未验证**：真人拖动——拖动中的最小高度限制、拖动中跨线切形态，需要用手拖一次。

**2a 结果**：默认构建 438 个测试、只开 `nsh` 的构建 346 个。只开 `nsh` 的 debug 构建在隔离 HOME（`config.toml` 里 `disk = 1.0`）下实测：第一个 tick 就报了启动盘告警（横幅判定 delivered），托盘换成磁盘脸并带上 “88G”——这一步是持锁算好、交回主线程换的；面板上的 CPU 和内存随 tick 变化（两张间隔 4 秒的截图 14.8% → 20.9%）；probe 点托盘隐藏、再点显示（挂在图标下、成为 key），显示后内存那一行立即刷新、“第几次打开”从 0 变 1（隐藏时重置了临时视图）；probe 的新命令 `banner` 模拟横幅点击，面板切到告警页。日志里没有 ERROR / WARN。

## 阶段 3：界面基础件

主题（Golden Dark / Golden Light / Classic 两套 / Omarchy）、字体（界面字体的等宽数字、等宽字体）、`card`、`big_number`、带最大宽度的 tooltip、`MenuTrigger`、选项 chip、`list_shell`、`expand_mark`、应用图标（`appicon`）、确认对话框。规则照 `.claude/skills/ui-polish`。分四步：

- **3a 主题（**已完成**，2026-10-24）。** 主题偏好在 NSH 前端生效，四套配色都能画。gpui 版在外观变化时整套重刷 token（`apply_appearance` 再 `Theme::change`）；NSH 不用我们去问：交给它的颜色可以带亮、暗两份（`AdaptiveColor`），引擎按当前外观自己挑。所以 `palette.rs` 加了 `in_mode`——在当前线程上临时钉住当前配色系列（经典或 Golden）的亮或暗那一半，读出 token 后就松开，别的线程不受影响；`nsh/theme.rs` 的 `color` 用它把一个 token 读成亮暗两份。只有换配色系列（经典、Golden、Omarchy，是设置页的事）才需要重新加载各窗口的根组件。根组件都经 `root::make` 创建，带上偏好要求的外观（Light / Dark / Omarchy 调色板自己的，其余跟随系统）。NSH 解析出来的亮暗由面板根组件的一个 effect 报回宿主（`HostAction::Appearance`），宿主据此更新 palette 的全局状态（引擎之外也有人读它：Linux 托盘的墨色、透明度默认值）和窗口在第一帧之前的底色。窗口底色成了 token（`palette::ground`，与 gpui 用的主题文件同值，Golden 两种模式有测试对照 `themes/golden.json`）。共享出来的：`theme_in_force` 进了 `prefs.rs`，钉住 AppKit 外观（`pin_appearance`）进了 `appkit.rs`。
- **3b 基础件（**已完成**，2026-10-24）。** `nsh/widgets.rs`：gpui 版 `views/widgets.rs`、`MenuTrigger`、选项 chip 的 NSH 写法，尺寸、墨色、规则都照原样（原件上的注释解释了为什么，不重复）。`card` / `list_shell` / `card_header`、`big_number` / `big_figure` / `big_figure_with`（只在空格处切开 `format` 已经拼好的字符串，不重算）、`outline_pill`、`meter`、`option_chip`、`menu_trigger`（下拉菜单，当前项打勾）、`wrap_tooltip`（两段式、220 宽换行）、`expand_mark`、`icon`。和 gpui 版的区别只在画法：颜色都是亮暗两份；卡片的细线是 `outline`，不占布局（gpui 那边要用零模糊的内阴影才能做到）；图标是打包进来的 SVG，转成 `data:` URI 交给 NSH 渲染，再按墨色着色——NSH 没有图标集，gpui-kit 的 lucide 图标这里没有，页面用到哪个就把哪个放进 `assets/icons`（这次先放了 `chevron-down` / `chevron-right`，进了 `CustomIconName`，嵌入测试也覆盖）；等宽字体 JetBrains Mono 由 `root::make` 随每个根组件注册（`Root::font`），文件和 gpui 用的同一份。菜单和 tooltip 的底色是新的不透明 token：`palette::popover` / `popover_hover`，取 gpui 主题文件里的 `popover.background` / `accent.background`，Golden 两种模式有测试对照 `golden.json`；卡片那种半透明的提亮色会让下面的字透上来。占位面板改成了这些基础件的样张，数据都接实时状态。
- **3c 毛玻璃**：经典两套靠毛玻璃加一层半透明底色才对（亮色的卡片是白的，要放在灰色的玻璃上），NSH 的容器层默认不透明（原型的问题 2），要垫一层 `.popover` 材质并清掉容器底色；透明度偏好（`prefs::opacity`）随之生效。
- **3d 应用图标和确认对话框。**

**3b 结果**：默认构建 440 个测试、只开 `nsh` 的构建 349 个。只开 `nsh` 的 debug 构建（Golden Dark）用 probe 逐项实测：样张截图里各件都对——20 号的数字和 13 号的单位、香槟色进度条随 tick 走、胶囊、带 SVG 箭头的菜单按钮、展开箭头、选中和未选中的 chip、JetBrains Mono 的版本号；点展开箭头，箭头朝下、细节行出现；点 chip，选中态移过去；打开菜单，弹层不透明、当前项打勾，选一项后按钮文字跟着变、菜单关闭；悬停版本号，两段式 tooltip 在 220 宽处换行、第二段变暗。实测中修了两处：菜单弹层原本用卡片的半透明提亮色，下面的字透了上来（于是有了 `popover` token）；`wrap_tooltip` 起初返回 `nsh_base` 的 `Tooltip` 组件，而 NSH 给元素挂 tooltip 时会再包一层提示浮层，浮层套浮层，里面那层永远不显示——改为只返回气泡本身。**测试方式上的一个教训**：probe 点面板会让它成为 key 窗口，之后用户在前台应用里按的键会被面板截走（一次测试中用户在终端按的 ⌘2 把面板切到了 Apps 页）；probe 的 `click` 现在点完会把 key 交还。`make linux-check`：434 / 347 个测试全过，NSH 构建的 clippy 通过。

**3a 结果**：默认构建 440 个测试、只开 `nsh` 的构建 349 个（多出的是 palette 的两条：Golden 的底色等于主题文件、`in_mode` 只在调用期间、只在本线程生效）。只开 `nsh` 的 debug 构建在隔离 HOME 下按四种偏好各截一张：跟随系统（本机暗色）是经典暗色、底 `#0a0a0a`；`golden` 是 `#1c1c1e` 底、`#252528` 卡片；`omarchy`（debug 构建在 Mac 上用 Tokyo Night 兜底）是蓝灰调；`light` 是白底，卡片几乎看不出来——预期之中，经典亮色要等 3c 的毛玻璃。系统外观在运行中翻转时的换色没有实测（不能替你改系统外观）。`make linux-check`：默认构建 434 个、只开 `nsh` 的构建 347 个测试全过，NSH 构建的 clippy 通过。

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
| 2 宿主 | 2a、2b、2c 完成；2d 已写完、只编译过，等 Omarchy 真机 |
| 3 基础件 | 3a、3b 完成；3c、3d 未开始 |
| 4 逐页 | 未开始 |
| 5 切换 | 未开始 |
