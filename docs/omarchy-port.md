# 在 Omarchy 上跑起来：分阶段移植计划

这份文档和 `cross-platform.md` 不同，它**不是假设**：目标是让面板在 Omarchy（Arch + Hyprland + Wayland）上真正可用，而不是让编译器闭嘴。

`design.md` 的「非 macOS 平台编译不过」一节曾列出三条硬约束，其中**一条已经过时**，另一条有正规解法：

- ~~Linux 上 `tray-icon` 不发射点击事件~~ — `tray-icon` 0.25 新增 ksni 后端（StatusNotifierItem，走 D-Bus，不需要 GTK 事件循环），在 `activate` / `secondary_activate` 上发 `TrayIconEvent::Click`。本仓库已经在 0.25.1 上。
- **Wayland 不允许客户端设置绝对位置** — 仍然成立，但正规机制是 wlr-layer-shell，而 gpui 已经实现：`WindowKind::LayerShell(LayerShellOptions)`，可设 layer、anchor（`TOP | RIGHT`）、exclusive zone、margin、键盘交互模式和 namespace，`gpui-pre-linux` 里是真的 `zwlr_layer_shell_v1`。面板锚在右上角由合成器摆放，不需要坐标。
- **SNI 不提供图标屏幕矩形** — 这条是新的、也是真正的降级：`rect` 为空，所以「锚在图标正下方」在 Linux 上无法还原。`placement.rs` 那套锚定数学在这里不再需要。

## 目标与不做什么

**目标**：Omarchy 上一个能用的面板——六个页的数字、zstats 的告警、桌面横幅、可靠的唤起方式、能对进程动手。

**不做**：X11、GNOME / KDE 适配、以及依赖 macOS 专有系统服务的功能（下面逐项点名）。

**两条不变的铁律**：zstats 拥有数字与告警，面板只负责呈现；**macOS 上的行为不得倒退**——每个新分支在 macOS 上要么不可达，要么逐像素等价。

## 构建与验证矩阵：x86_64 是主目标，aarch64 也要能跑

**源码层面两个架构没有差别。** 全仓库唯一按架构分叉的地方是更新器挑 DMG 文件名（`env::consts::ARCH`），而那条路在 Linux 上本来就要换掉。所以「支持两个架构」的成本不在代码，在**哪里构建、在哪里验证**。

| 环节 | x86_64 | aarch64 |
|---|---|---|
| 本地编译循环 | 这台 arm64 Mac 上的 Apple `container` **做不到**（它跑 arm64 虚拟机，不带模拟），需要 OrbStack / Docker Desktop 的 Rosetta 方案 | Apple `container` 原生、最快 |
| CI 编译与产物 | `ubuntu-latest` | `ubuntu-24.04-arm`，公开仓库免费 |
| 效果验证 | 你的 Omarchy 机器，逐条走验收 | **只要能构建、能启动**（决定于 2026-09-17）。观感与交互不在 arm64 上验收。注意 CI 只能证明「构建通过、测试通过」——启动需要一个合成器，跑 runner 上没有，所以「能启动」要等真有一台 arm64 Linux 桌面时才算验证过，在那之前如实记为未验证 |

**结论**：本地只保留 arm64 容器做快速编译试错，x86_64 的编译与产物交给 CI——那里本来就是 x86_64，比在 Mac 上模拟便宜得多。**阶段零的验收因此是「两个架构在 CI 上都能编译、测试通过」**，而不是本地两个都能出二进制。

容器还有一条硬边界要记住：里面没有合成器、没有显示、没有会话总线，**它只能回答「编不编得过」**。从阶段一开始的每一条验收都必须在真机上做。

## 需要先确认的环境事实

1. **Omarchy 版本：4.0（Quattro）**，已确认。bar、通知、OSD、锁屏全部由一个 Quickshell 单进程 shell 提供，Waybar 和 mako 都不在了。**托盘宿主因此是 Quickshell**，它是否实现 StatusNotifier 宿主仍未确认，这是阶段 3 最大的未知；通知的 action 支持同理，影响阶段 5 的横幅点击。
2. 因此**面板不把托盘当作唯一入口**：阶段 2 先做键位唤起，阶段 3 的托盘是增益而不是前提。
3. 通知守护进程是否支持 action（mako 支持），决定横幅点击能不能落到告警页。

## 阶段 0：Linux 目标能编译、能启动（**已完成**，2026-09-17）

实测结果：macOS 上 `cargo fmt --check` 干净、`make lint` 无告警、281 个测试全过且行为无变化；Linux aarch64 在 Apple `container` 的虚拟机里 `cargo build --all-targets` 链接通过、268 个测试全过。x86_64 等 CI 的两个 job 回答——那正是本阶段加它们的原因。「能启动」在 arm64 上仍未验证，缺一台 arm64 Linux 桌面，如实记着。

**做什么**

- 撤掉 `main.rs` 顶部的 `compile_error!`，把门禁真正铺到那六处无条件 import：`metrics.rs`、`state.rs`、`watch.rs`、`views/processes.rs`、`views/alerts.rs`、`placement.rs`。
- 按模块给出诚实空态，不留永远失败的控件：`spaceinfo`（tmutil）、`assetinfo`（MobileAsset）、`updater::install`（DMG）在 Linux 上不渲染入口，而不是渲染一个点了没反应的按钮。
- `Cargo.toml`：`libc` 从 macOS 段移到公共依赖（`terminate`、`volflag`、`procscan` 三处都要）；`ureq::Proxy` 那条 `cfg(not(target_os = "linux"))` 门禁删掉，它把 Linux 的代理整个切没了。`tray-icon` 的 ksni 特性留到阶段 3——在 `tray.rs` 能在 Linux 编译之前打开它没有意义。
- `procscan` 拆成 `procscan/{mod,macos,linux}.rs`：类型与契约在 `mod.rs`，两个后端各自实现 `scan` / `comm` / `process_groups`。Linux 侧读 `/proc/<pid>/stat`，**不是占位**——僵尸、停止、进程组、启动时间都在那一行里。
- `terminate` 不拆文件，只把 AppKit 那一档门禁掉：Linux 上 `method_for` 恒为 `Term`、`can_quit_app` 恒假，应用页因此不长退出按钮，SIGTERM 那一半原样保留。

**验收**：CI 上 `ubuntu-latest` 与 `ubuntu-24.04-arm` 两个 job 都能 `cargo build` 并跑通 `cargo test`；macOS 上 `make lint` 无告警、测试全绿、行为无变化。`test.yml` 这时就该加上这两个 job，Linux 能不能编译从此每次推送都有答案。

**风险**：门禁会碰很多文件，容易顺手改到 macOS 路径。每一处都应当是「加一个 Linux 分支」，不是「改一个共用分支」。

**Linux 上会留下十来条 dead-code 告警，这是有意的**：`active` 的前台应用表、`autostart` 的状态名、`format::gb_short`、`CollectorPace::hidden`、`SeenAlert::recovered_for` 等等，每一条都精确对应一个「Linux 还没有路径」的功能，后面的阶段接通谁，谁的告警就消失。用 cfg 逐个盖住只会在阶段 3 和阶段 5 全部推翻重来，而且会把这份现成的缺口清单藏起来。CI 的 Linux job 因此不开 `--deny=warnings`。

**本地容器循环的三个坑，记下来免得重踩**：容器里 `cargo` 拉 crates.io 会超时（那台虚拟机网络很慢），所以在宿主机 `cargo vendor` 之后用 `--offline` 加 `source.crates-io.replace-with` 让容器离线编译；默认内存不够，`naga` 会被 OOM 杀掉，要 `-m 12g`；gpui 的 x11 后端在**链接**阶段才会暴露缺 `libxkbcommon-x11` 和 `libxcb`，编译全过之后才报错。

**动手之后发现的两件事，都记在这里免得后人重走**：

1. **仓库比文档说的更接近可移植。** `active`、`autostart`、`spaceinfo`、`bigfiles`、`notify`、`views/disk` 早就写好了非 macOS 分支，托盘、Dock、窗口的调用点也全部门禁过了。那条 `compile_error!` 挡住了所有人，所以没人知道这些分支已经铺好——这正是它该被换掉而不是留着的理由。
2. **有两个「按了没反应」的控件**，都是之前从没编译过的分支留下的：进程页的退出按钮在非 macOS 上是个空函数 `fn kill(_pid: u32) {}`，而 `can_term` 在 Linux 上返回真，所以按钮照画、点了什么都不发生。`terminate::request_term` 本来就是纯 POSIX 的，直接接通即可。这类东西只有真编译、真读一遍分支才会暴露。

另外把两个测试从「机器断言」改成「契约断言」：`fullscan` 原本断言「任何真实机器都有 50 个以上进程」，容器里只有 5 个。上限有没有生效应该看「返回数是否等于总数」，或者和一个刻意设了上限的对照组比，而不是看机器有多大。

## 阶段 1：窗口形态——layer-shell 锚在右上角（**代码完成**，待真机验收）

**做什么**

- 面板窗口在 Linux 上用 `WindowKind::LayerShell`：`namespace = "zstats"`、`layer = Overlay`、`anchor = TOP | RIGHT`、`margin` 留出与屏幕边和 bar 的间距、`keyboard_interactivity = OnDemand`（进程页的名称过滤框要能输入）、不要 exclusive zone（面板是浮层，不该挤占工作区）。
- 显隐：macOS 的 `orderOut` 保留窗口实例，Linux 走销毁重建（`window_ext` 的 Linux 对侧），`CollectorPace` 的可见性开关接到这条路径上，重绘门禁照旧。
- 毛玻璃：三层里中间那层（`NSVisualEffectView` 的材质 clamp）由合成器接管——Hyprland 的 `layerrule = blur, zstats` 匹配的正是上面那个 namespace；顶层的 wash 保留，底层不需要。

**动手时发现的两件事**

- **`exclusive_zone` 不设**就等于设 0，而 0 的语义是「不占位，但请把我挪开、别压住别人的占位区」——所以面板会自动落在 bar 下面，不需要我们去算 bar 有多高。margin 只留 8px 的边距。
- **快捷键原来在 Linux 上根本到不了面板。** gpui 把 `cmd-` 解析成*平台*修饰键，在 Linux 上是 Super，而 Omarchy 用 Super+数字切工作区，按键全被合成器吃掉。改用 gpui 自己的 `secondary-`（macOS 是 ⌘，其它平台是 Ctrl），页签提示里的符号也跟着按平台取。

**验收**（需要在 Omarchy 上做，容器里没有合成器）：

1. `cargo build` 之前先装系统依赖：`pkgconf`、`libxkbcommon`、`libxkbcommon-x11`、`libxcb`、`fontconfig`、`freetype2`，运行还需要 `vulkan-icd-loader` 加显卡驱动。
2. **用 `cargo run`（debug）验收**：release 构建会在失焦时自动收起，而托盘（阶段 3）和 `--toggle`（阶段 2）都还没有，收起之后没有任何入口能把它叫回来。debug 构建不自动收起，正好适合这一阶段。
3. 面板应当贴在右上角、在 bar 下面、不被遮住也不遮住 bar。
4. `hyprctl layers` 里能看到 namespace 为 `zstats` 的 overlay 层。
5. Ctrl+1 到 Ctrl+7 切页签，进程页的过滤框能打字。
6. 想看毛玻璃：`hyprland.conf` 加一行 `layerrule = blur, zstats`，再把设置里的面板不透明度调低。默认是不透明的，所以不加这条规则也能正常读数。
7. 反复开合十次，看 RSS 是否持续上涨（macOS 上销毁重建曾经每次泄漏约 1 MB，Linux 这边要实测）。

**这一阶段结束时 Linux 上还不能算「可用」**：release 构建失焦即收起，而把它叫回来的两条路（阶段 2 的键位、阶段 3 的托盘）都还没有。所以阶段 1 和阶段 2 之间不要打包任何东西给人用。

## 阶段 2：唤起入口，不依赖托盘

**做什么**

- 单实例 + `--toggle`：第二次启动不新起进程，而是通过 `$XDG_RUNTIME_DIR` 下的 unix socket 把「开合面板」发给已经在跑的那个。
- Hyprland 侧一行 `bind = SUPER, M, exec, zstats --toggle`，写进 README 的 Linux 段。

**验收**：绑定的键位能开合面板；连续执行 `zstats --toggle` 不会产生第二个进程；面板关着时采集仍在跑（托盘常驻的那套节奏在这里同样适用）。

## 阶段 3：托盘（有宿主才有）

**做什么**

- `tray-icon` 的 ksni 后端：图标、菜单、左键 `activate` 触发 toggle。**`rect` 为空**，所以点击只负责开合，不参与定位——定位已经由阶段 1 的 anchor 决定。
- **一个必须承认的降级**：SNI 没有「菜单栏文字」这种东西，Waybar / Quickshell 的托盘只画图标。macOS 上图标旁边那个 CPU% 或可用内存数字在 Linux 上没有对应物。两个选项：接受只有图标（脸的切换仍然有意义：CPU / 内存 / 磁盘三种图形），或者面板把当前数字写进一个状态文件，由 bar 的自定义模块去读——后者是 bar 的配置，不是本应用的功能。
- 托盘不可用时（宿主不存在）不报错、不重试，只在日志里说一次，唤起靠阶段 2。

**验收**：托盘里出现图标；左键开合面板；右键出菜单（菜单由宿主渲染）；杀掉 bar 再拉起，图标能回来。

## 阶段 4：动作与观察器

**做什么**

- `procscan`：`sysctl(KERN_PROC_ALL)` 换成读 `/proc/*/stat` 的进程状态（zombie / stopped），接口和字段保持不变，上层不需要知道换了实现。
- `terminate`：只保留 `request_term`（SIGTERM），删掉应用层那一档——Linux 上没有 `NSRunningApplication` 那种「可拒绝的 ⌘Q」。告警卡上的退出按钮因此在 Linux 上只对进程生效，文案要跟着改。
- `volflag`：`statfs` 的 `MNT_RDONLY` 换成 `statvfs` 的 `ST_RDONLY`；只读额外卷的告警跳过规则保持不变。

**验收**：进程页能列出异常进程；对自己的进程点退出真的收到 SIGTERM；挂一个只读镜像，磁盘告警仍被跳过且日志写 `banner="skipped"`。

## 阶段 5：系统集成

**做什么**

- **通知**：`notify-rust` 在 Linux 走 D-Bus，本来就在依赖里。要做的是 action 回调——点击横幅打开面板并切到告警页。守护进程不支持 action 时退化为「只显示，不可点」，并且**不要假装可点**。
- **防休眠**：IOKit 的电源断言换成 logind 的 `Inhibit`（`what=idle`）。**这里有一个必须实测的未知**：hypridle 是否尊重 idle inhibitor。不尊重的话，这个开关在 Omarchy 上就是假的，那就应当在 Linux 上直接隐藏它，而不是留一个不起作用的开关。
- **开机自启**：`SMAppService` 换成 `~/.config/autostart/zstats.desktop`。
- **更新器**：Linux 上保留检查、去掉安装——DMG 与 Gatekeeper 都不适用。按钮改成「去发布页」，或者交给包管理之后整块隐藏。

**验收**：触发一次真实告警，横幅出现并且点击落到告警页；开关打开后 `systemd-inhibit --list` 能看到我们；重启后面板自启；更新检查能报出新版本且不提供原地安装。

## 阶段 6：能力驱动的界面与收尾

**做什么**

- 读 `zstats::Capabilities`，不要猜平台：Linux 上 `memory_pressure` 和 `cpu_perf_levels` 都是假。概览的压力卡与 P/E 核卡片说「本平台不报告」而不是显示 `—`；托盘 Auto 的内存脸去掉内核压力那条触发，只留进程与应用两类。这正是 `cross-platform.md` 第一节的做法，照搬即可。
- 砍掉的功能给出诚实空态：可清除空间、系统资源包说明、DMG 安装。
- 打包：两个架构各出一个 tar.gz（二进制、`.desktop`、图标），接进 `publish.yml` 现有的发布与 `SHA256SUMS` 流程，Gitee 镜像同样带上；Arch 侧可以再给一个 PKGBUILD。两份 README 各补一段 Linux 安装与 Hyprland 配置片段（`layerrule` 与 `bind` 两行）。
- 更新器在 Linux 上按架构挑的是 tar.gz 而不是 DMG，两个架构的命名要和 macOS 那套保持同一个形状。

**验收**：在 Omarchy 上完整走一遍——开面板、翻六个页、触发一次告警、收一次横幅、退出一个进程、清一次可再生缓存目录、切换主题与语言、重启后自启并保持上次标签页。

## 工作量与顺序

阶段 0 和 1 是地基，必须按顺序；2 之后可以并行。粗略估计：0 与 1 各占大头，2 和 4 各一天量级，3 取决于托盘宿主的未知，5 取决于 hypridle 的实测结果，6 是收尾。

**建议的第一个可验证里程碑是阶段 1 结束**：那时 Omarchy 上已经有一个贴在右上角、能看数字的面板，后面每个阶段都是在一个活着的东西上加功能，而不是在等一次大爆炸式的移植完成。

## 已知未知（需要实测才能回答）

| 问题 | 影响 | 怎么问 |
|---|---|---|
| Omarchy 4 的 Quickshell 是否提供 SNI 宿主 | 阶段 3 是否成立 | 装一个已知带托盘图标的应用，看 bar 上有没有 |
| gpui 的 layer-shell 在 Hyprland 上的实际表现 | 阶段 1 | 最小示例：一个锚在右上的 overlay 窗口 |
| 销毁重建的内存行为 | 阶段 1 的显隐策略 | 开合一百次，看 RSS |
| 通知守护进程是否支持 action | 阶段 5 的横幅点击 | `notify-send` 带 action 试一次 |
| hypridle 是否尊重 idle inhibitor | 防休眠开关在 Linux 上是否该存在 | `systemd-inhibit --what=idle sleep 600` 后等待息屏 |
