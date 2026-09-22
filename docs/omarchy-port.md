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

1. **Omarchy 版本：4.0（Quattro）**，已确认。bar、通知、OSD、锁屏全部由一个 Quickshell 单进程 shell 提供，Waybar 和 mako 都不在了。**托盘宿主因此是 Quickshell，并且确实能显示托盘项**（2026-09-18：先用 `IsStatusNotifierHostRegistered` → `b true` 判断，随后实机看到了图标）。那条 `busctl` 探针**比当初说的要弱**：ksni 自己的注释指出两个主流 watcher 实现都把这个属性硬编码成 true 且从不真正处理 `RegisterStatusNotifierHost`（`ksni-0.3.6/src/service.rs:106`），所以它证明的是「有 watcher」而不是「有宿主在画」。真正的证据是图标出现了。阶段 3 因此成立。通知的 action 支持仍未确认，影响阶段 5 的横幅点击。
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

- **`exclusive_zone` 不设**就等于设 0，而 0 的语义是「不占位，但请把我挪开、别压住别人的占位区」——所以面板会自动落在 bar 下面，不需要我们去算 bar 有多高。
- **两条边的 margin 要的是相反的东西，所以不能共用一个常量**（实机反馈，2026-09-18）：右边 8px 在 358pt 宽的面板旁边读不出来，看着就是贴着屏幕；顶部那 8px 则是多余的，面板应当像 macOS 的 popover 挂在菜单栏下沿一样挂在 bar 下沿。现在两边都是 10(`PANEL_MARGIN_RIGHT` / `PANEL_MARGIN_TOP`)。**顶部曾经是 0,而那个 0 是在一个有布局 bug 的构建上量出来的**:gpui-component 的 CSD 外框当时把 surface 每边撑大 20px,内容第一行落在 bar 下方 20px 处、看着缝太大,0 正是让*那个*看起来对的值。inset 修掉之后 0 就是字面意义的贴死,又太紧。教训不在数字上——**在一个带布局 bug 的构建上调几何常量,调出来的是那个 bug**。
- **面板尺寸不能拿合成器的答复回喂**（实机测出来的，2026-09-18）。`ZStatsApp::render` 每帧把 `window.bounds()` 镜像进 store，`open_main_window` 下次又拿它当请求的尺寸——macOS 上这是必需的（窗口可缩放，每次点托盘都重建，尺寸只活在那里），Wayland 上是个没有不动点的回环。实测 `hyprctl layers` 给出 `1352 26 598 893`：设计值 358×653，两轴都正好大了 240，右边缘落在 1950 而屏幕只有 1920——**这也是当初「右边贴着屏幕没有缝」的真正原因，margin 一直都在，只是面板画出去了**。修法是 Linux 上只继承 origin、尺寸恒取 `DEFAULT_WINDOW_SIZE`：layer surface 用户根本没法缩放，没有尺寸值得记。每帧那个镜像保留不动，它还带着 `scale_factor`，只是 Linux 上不再有人消费它的尺寸。
- **gpui-component 的 CSD 外框必须在 layer surface 上关掉**（实机测出来的，2026-09-18，两条症状其实是一个 bug）。`WindowBorder::render` 每帧调 `set_client_inset(20px)`（`gpui-component` 的 `SHADOW_SIZE`，Linux 上是 20，其它平台 0），而 gpui 的 Wayland 后端在 `compute_outer_size` 里把这个 inset **加回**它报告的尺寸和提交的 buffer。于是 surface 画得比合成器锚定的框每边大 20：右边缘越过屏幕（读起来是「margin 没了」），而 bar 和第一行内容之间多出一条 20px 的阴影带（读起来是「顶部空隙太大」）。`Root::bordered(false)` 是 gpui-component 自己文档里为这种 surface 准备的开关——layer surface 由合成器摆放、没有标题栏、拉不动，那个外框没有任何东西可装饰。设置窗口和磁盘空间窗口是真正的 toplevel，外框照旧。
  - 顺带记一条死路：`WindowOptions::window_decorations = Server` **不管用**。layer surface 上没有 xdg-decoration 可协商，gpui 的 `request_decorations` 会回落成 `Client` 并打一行 log。
- **顶部的 margin 不是从屏幕顶边量的**，是从 bar 占位之后剩下的可用区顶边量的——合成器先挪，margin 后加。所以 bar 若「占的比画的多」，`PANEL_MARGIN_TOP = 0` 也仍然会留一条缝，那条缝不归我们管。真要贴死，只能 `exclusive_zone: Some(px(-1.))` 整个退出占位避让，再用 margin 把 bar 的高度自己减回来——而 bar 有多高没有任何协议会告诉我们，那就成了一个要用户填的配置项，不是一个能测出来的数。先不做。
- **快捷键原来在 Linux 上根本到不了面板。** gpui 把 `cmd-` 解析成*平台*修饰键，在 Linux 上是 Super，而 Omarchy 用 Super+数字切工作区，按键全被合成器吃掉。改用 gpui 自己的 `secondary-`（macOS 是 ⌘，其它平台是 Ctrl），页签提示里的符号也跟着按平台取。

**验收**（需要在 Omarchy 上做，容器里没有合成器）：

1. `cargo build` 之前先装系统依赖：`pkgconf`、`libxkbcommon`、`libxkbcommon-x11`、`libxcb`、`fontconfig`、`freetype2`，运行还需要 `vulkan-icd-loader` 加显卡驱动。
2. **光标进过面板再离开才收起**（debug 和 release 都是）。从托盘往下移、还没碰到面板时不能收：面板一 map 就把键盘抢走，光标还在 bar 和面板之间的空隙上时 Hyprland 会把键盘焦点还回去，于是 `wl_keyboard.leave` 比光标先到。这次 leave 在光标从未进入时忽略。真正的收起是 `wl_pointer.leave`（进过一次之后），点击仍落在底下的窗口上，不会被吃掉。钉住时不收。
3. 面板应当贴在右上角、在 bar 下面、不被遮住也不遮住 bar。
4. `hyprctl layers` 里能看到 namespace 为 `zstats` 的 overlay 层。
5. Ctrl+1 到 Ctrl+7 切页签，进程页的过滤框能打字。
6. 想看毛玻璃：`hyprland.conf` 加一行 `layerrule = blur, zstats`，再把设置里的面板不透明度调低。默认是不透明的，所以不加这条规则也能正常读数。
7. 反复开合十次，看 RSS 是否持续上涨（macOS 上销毁重建曾经每次泄漏约 1 MB，Linux 这边要实测）。

**这一阶段结束时 Linux 上还不能算「可用」**：release 构建失焦即收起，而把它叫回来的两条路（阶段 2 的键位、阶段 3 的托盘）都还没有。所以阶段 1 和阶段 2 之间不要打包任何东西给人用。

## 阶段 2：唤起入口，不依赖托盘（**代码完成**，待真机验收）

**做什么**

- 单实例 + `--toggle`：第二次启动不新起进程，而是通过 `$XDG_RUNTIME_DIR` 下的 unix socket 把「开合面板」发给已经在跑的那个。
- Hyprland 侧一行 `bind = SUPER, M, exec, zstats --toggle`。

**做完之后的样子**（2026-09-18，`src/ipc.rs`，整模块 `#[cfg(target_os = "linux")]`）：

- **握手顺序是先 bind 再 connect，不是反过来。** 「先 connect，失败就 unlink 再 bind」有一个窗口期，两个同时起的进程都会判定 socket 是陈留的，于是后一个把前一个的活 socket 删掉。现在只有在 connect 被拒（证明没人应答）之后才 unlink。
- **`$XDG_RUNTIME_DIR` 没有时不退回 `/tmp`**，只记一行日志、退化成「没有单实例」。`/tmp` 是别的用户可以抢先 bind 的路径，一把陌生人能持有的单实例锁比没有锁更糟——那等于把你的每一次按键交给先到的人。
- **裸启动和 `--toggle` 是两件事。** 裸启动在没有实例时**不开面板**（那是登录自启的路径，release 常驻托盘的行为不能倒退），有实例时转发 `show`；`--toggle` 在没有实例时**要开面板**——键按下去只是悄悄起了个后台进程，读起来就是这个键坏了。
- **未知参数是硬错误**（退出码 2），不是忽略。`hyprland.conf` 里拼错一个 flag，否则会变成「每按一次就多起一个面板」，而症状（第一次之后键就不灵了）指向的地方离病因十万八千里。
- 连上了但写失败**仍算送达**：既然连上了，实例就在，为了一次按键再起一个常驻采集器正是这个模块存在的意义所在要拒绝的交易。

**关于 README**：计划原本写「写进 README 的 Linux 段」，**没做，是有意的**。README 现在的第一行是「macOS only · 已签名公证」，而 Linux 侧一个包都还没有（那是阶段 6）。在用户看的页面上写一行 Hyprland 键位绑定、却没有任何东西可安装，是在宣传不存在的东西。那一段跟着阶段 6 的打包一起进 README，两份 README 同步。

**验收**（需要在 Omarchy 上做）：

1. `zstats --help` 打出用法；`zstats --frobnicate` 拒绝并且 `echo $?` 是 2。
2. `bind = SUPER, M, exec, zstats --toggle` 写进 `hyprland.conf`，按键能开、再按能合。
3. 面板没开的时候按第一次，面板应当**出现**（而不是悄悄起个进程）。
4. 连按十次 `zstats --toggle`，`pgrep -c zstats` 始终是 1。
5. `ls -l $XDG_RUNTIME_DIR/zstats-app.sock` 能看到它；`kill -9` 掉进程之后 socket 文件还在，再启动应当正常接管（日志里有 `clearing a socket left by a previous run`）。
6. 面板关着时采集仍在跑（托盘常驻的那套节奏在这里同样适用）——托盘的数字继续动就是证据。

## 阶段 3：托盘（**代码完成**，待真机验收）

**做什么**

- `tray-icon` 的 ksni 后端：图标、菜单、左键 `activate` 触发 toggle。**`rect` 为空**，所以点击只负责开合，不参与定位——定位已经由阶段 1 的 anchor 决定。
- **一个必须承认的降级**：SNI 没有「菜单栏文字」这种东西，`set_title` 写的 `Title` 属性 Quickshell 不渲染，托盘上只有图标。macOS 上图标旁边那个 CPU% / 剩余内存数字在 Linux 上没有对应物，只活在悬停提示里。

  **「把数字画进图标本身」试过了，真机上输了，已经撤掉**（2026-09-18）。做法是把图形和数字合成一张宽位图（`ttf-parser` 取仓库自带 JetBrains Mono 的字形轮廓，等宽所以不需要 shaping，固定 4 格所以宽度恒定），动手前还渲染过预览图确认 26px bar 高度下可读。**赌的是宽高比，赌输了**：Quickshell 把整张宽位图塞进托盘槽位的宽度，于是图形被按比例缩得很小——数字是读到了，图标废了。协议里没有任何办法事先问宿主会怎么缩放，所以这件事只能这样试一次。代码全部删除（`traytext.rs`、`ttf-parser` 依赖、`Faces` 改回缓存成品 `Icon`），**不要再试第二次**；真想要数字，剩下的只有状态文件 + bar 自定义模块那条路，而那是 bar 的配置不是本应用的功能。
- 托盘不可用时（宿主不存在）不报错、不重试，只在日志里说一次，唤起靠阶段 2。

**读过 tray-icon 0.25.1 的 ksni 后端之后，动手前先记下来的四件事**（`src/platform_impl/ksni/mod.rs`）：

1. **feature 要 `default-features = false, features = ["ksni"]`**。默认那套是 `libappindicator` + `muda-gtk3`，正是当初把 Linux 门禁掉的那个 GTK 主循环——ksni 和它是互斥的两条路，不是叠加。
2. **`secondary_activate` 发的是 `MouseButton::Middle`，不是右键。** 右键在 SNI 里不经过我们，宿主直接渲染 `set_menu` 给的菜单。所以 `tray.rs` 里按 `MouseButton::Right` 分支的逻辑在 Linux 上永远不会触发，要么接到中键上，要么承认这条路只走菜单。
3. **`rect()` 硬编码返回 `None`**（不是「有时没有」），`emit_click` 也是 `Rect::default()`。`placement.rs` 那套锚定数学在这里彻底没有输入。
4. **`set_title` 是存在的**，写进 SNI 的 `Title` 属性，同时进 tooltip 的 title。所以 CPU% 这个数字至少能在悬停时看到；**图标旁边显不显示是宿主的选择**，Waybar 不显示，Quickshell 未知——接通之后一看便知，不必现在假设。

**还有一个 macOS 上不存在的新问题：Linux 没有 template 图标这个概念。**

本仓库的托盘位图是「压平成黑色、只有 alpha 有意义」的蒙版（`tray.rs` 的 `rasterise_icon` / `stamp_hot_dot`），macOS 靠 `set_icon_with_as_template(…, true)` 让菜单栏自己上墨，白底黑字、黑底白字都不用我们管。SNI 没有这一层：`set_icon` 推过去的 ARGB 像素就是最终像素，**一个黑色字形在 Omarchy 的深色 bar 上等于看不见**，而 SNI 也不报告 bar 的明暗，没法照抄 macOS 那套「跟着菜单栏走」。

所以阶段 3 要多做一件 macOS 不需要做的事：**给字形上色**。最省事且诚实的做法是跟随应用自己的主题偏好（`prefs` 的 Auto / Dark / Light）——Auto 在 Linux 上没有系统信号可跟，落到浅色墨（深色 bar 是 Omarchy 的默认）。未读告警那个角标同理：macOS 上它是「同一张 template 上多一点 alpha」，Linux 上得是实打实的一种颜色。

**做完之后的样子**（2026-09-18）：`tray.rs` 的 `cfg` 门禁整体撤掉，`metrics.rs` / `state/mod.rs` / `state/alerts.rs` / `main.rs` 五处调用点的门禁一并撤掉；`toggle_main_window` 的锚点参数改成 `Option<TrayAnchor>`，SNI 那条路传 `None`（伪造一个 0,0 的矩形会把面板扔到屏幕角落，而不是让合成器摆放）；新增 `tray::ink()` 与 `TrayHandle::ensure_ink`，主题一动就整套重新光栅化。macOS 侧 `make lint` 干净、281 个测试全过、行为无变化。

**实机跑起来之后发现的两件事**（2026-09-18，Omarchy 4，release 构建）：

- **Omarchy 的托盘默认是收起的**，要点一下 bar 上的展开箭头才看得见图标。「一开始没显示」多半就是这个，不是注册失败——ksni 监听 watcher 的 `NameOwnerChanged` 并重新注册（`service.rs` 的 `service_loop`），`watcher_offline` 默认返回 true 保持服务活着，所以 bar 重启图标会自己回来，注册竞态不会留下一个永远空的托盘。
- **图标旁边没有数字**，见验收第 6 条。

**验收**（需要在 Omarchy 上做）：

1. 托盘里出现图标（**记得先展开 Omarchy 收起的托盘区**）；`busctl --user list | grep -i StatusNotifier` 能看到我们注册的 item。
2. 左键开合面板；右键出菜单（Show Window / Quit，由宿主渲染）。
3. **深浅两种 bar 下图标都看得见**——尤其把界面主题切成浅色再看一次，那是 `ink()` 唯一会猜错的情况。
4. 主题在设置里来回切，图标颜色**当场**跟着变（不是等下一个 tick）。
5. 切到「两者」模式，两个图标都在；切回去，第二个消失。
6. ~~`set_title` 的数字 Quickshell 是否显示在图标旁边~~ **已回答：不显示**（2026-09-18 实机）。把数字画进图标的方案也试过并撤掉了，理由见上——托盘就是一个图标，数字在 tooltip 里。
7. 杀掉 bar 再拉起，图标能回来。
8. 触发一次告警，未读角标出现在图标上；打开告警页后消失。

## 阶段 4：动作与观察器

**做什么**

- `procscan`：`sysctl(KERN_PROC_ALL)` 换成读 `/proc/*/stat` 的进程状态（zombie / stopped），接口和字段保持不变，上层不需要知道换了实现。
- `terminate`：只保留 `request_term`（SIGTERM），删掉应用层那一档——Linux 上没有 `NSRunningApplication` 那种「可拒绝的 ⌘Q」。告警卡上的退出按钮因此在 Linux 上只对进程生效，文案要跟着改。
- `volflag`：`statfs` 的 `MNT_RDONLY` 换成 `statvfs` 的 `ST_RDONLY`；只读额外卷的告警跳过规则保持不变。

**验收**：进程页能列出异常进程；对自己的进程点退出真的收到 SIGTERM；挂一个只读镜像，磁盘告警仍被跳过且日志写 `banner="skipped"`。

## 阶段 5：系统集成

**做什么**

- ~~**通知**~~ **已完成**（2026-09-21，`notify.rs`）。原以为只差 action 回调，逐行对完 notify-rust 4.18 的源码后发现是**三个半缺口**，都属于「编得过、从没跑过」：

  1. **副标题整条丢失。** notify-rust 的原话是 `subtitle` *"Only useful on macOS. Not part of the XDG specification."*，而三种横幅都往那儿放了实质内容。现在 `xdg_body` 把它折进 body，顺序和 macOS 的 title→subtitle→body 一致，空的一半不留空行。
  2. **点击很可能根本不触发。** `wait_for_action` 等的是 `ActionInvoked`，而按规范守护进程只对**应用声明过的** action 发这个信号——我们从没调过 `.action()`。现在启动后第一条横幅时懒查一次 `get_capabilities()`，宿主advertise `actions` 才声明 `default` 动作，否则只显示、日志里说一次，**不假装可点**（这正是本条原本要求的降级）。
  3. **静音横幅照样会响。** `sound_name("")` 设的是 `Hint::SoundName("")`，不是规范的静音方式。改成 `Hint::SuppressSound(true)`。方向和 macOS 相反是 XDG 本身决定的：macOS 默认静、要响才加声音；XDG 默认由守护进程决定、要静才得明说。
  4. （半个）**同一 episode 的跟进会堆叠而不是替换。** `Banner::id` 在 Linux 上没人读——正是当初那批 dead-code 告警之一。XDG 的对应物是 `replaces_id`（`u32`），所以 `xdg_id` 用手写的 FNV-1a 把字符串 id 哈希过去；**不用 `DefaultHasher`**，std 明确不保证它跨版本稳定，而稳定正是这件事的全部意义。永不为 0——规范里 0 是「不替换任何东西」。

  **验证方式**：`notify-rust` 的 XDG 后端默认走 zbus（纯 Rust，`default = ["z"]`），所以可以在 macOS 上用一个 scratch crate `cargo check --target x86_64-unknown-linux-gnu` **真编译**这条路——从真实源码里抽出函数、只替换 i18n 和 `Banner` 两处外部依赖，`--deny=warnings` 干净。FNV 常量另外用 Python 独立算过。`xdg_body` / `xdg_id` 两个纯函数在仓库里有非 macOS 门禁的单测，Linux CI 会跑。

  **一处查完发现是虚惊**：`wait_for_action` 的闭包收的是 `&NotificationResponse` 还是 `&str`，曾怀疑会让横幅自然消失也误开告警页。`xdg/mod.rs:99` 保留了 `F: FnOnce(&str)` 的兼容签名并把 `Closed(_)` 映射成 `"__closed"`，原代码是对的。
- **防休眠**：IOKit 的电源断言换成 logind 的 `Inhibit`（`what=idle`）。**这里有一个必须实测的未知**：hypridle 是否尊重 idle inhibitor。不尊重的话，这个开关在 Omarchy 上就是假的，那就应当在 Linux 上直接隐藏它，而不是留一个不起作用的开关。
- **开机自启**：`SMAppService` 换成 `~/.config/autostart/zstats.desktop`。**能用,但机制不是 Hyprland**（2026-09-22 实测）：Hyprland 自己不读 XDG autostart，是 systemd 的 `systemd-xdg-autostart-generator` 把那个目录下的 `.desktop` 生成成 `app-*@autostart.service` 挂到 `xdg-desktop-autostart.target` 下。所以这是**会话的属性而不是合成器的**，换一套 session 就可能没有；装之前先 `systemctl --user status xdg-desktop-autostart.target` 确认它 active，没有就退回 hyprland.conf 的 `exec-once`。好处是这条路生成出来的本来就是一个真 unit，`systemctl --user status` 能查、能停，不必再手写一个。
  三处条目内容上的讲究，都不是惯例而是这个程序的要求：`Exec` **不带参数**（裸启动在没有实例时刻意不开面板——那正是登录路径，`--toggle` 会开，不能用）；`Exec` 用**绝对路径**（generator 生成的 unit 不继承 shell 的 `PATH`）；`StartupNotify=false`（面板是 layer surface，不产生普通 toplevel 去满足启动通知，设 true 会让光标转圈到超时）。
  改完要 `systemctl --user daemon-reload` 才会被 generator 看见，否则得等下次登录。
- **更新器**：Linux 上保留检查、去掉安装——DMG 与 Gatekeeper 都不适用。按钮改成「去发布页」，或者交给包管理之后整块隐藏。

**通知这一项的验收**（需要在 Omarchy 上做，Quickshell 支不支持 `actions` 是最后一个未知）：

1. 触发一次真实告警，横幅出现，**两行文字都在**（macOS 上是副标题的那行现在是 body 的第一行）。
2. 日志里有一行 `notification server capabilities`，看 `takes` 是 true 还是 false。
3. `takes=true` 时点横幅应当打开面板并切到告警页；`takes=false` 时点了没反应是**正确行为**，不是 bug。
4. 让同一个 episode 再报一次(或等一次跟进)，通知中心里应当是**替换**而不是两条并排。
5. 持续负载/内存爬升那两种慢燃横幅应当**无声**,阈值告警有声。

**其余各项的验收**：开关打开后 `systemd-inhibit --list` 能看到我们；重启后面板自启；更新检查能报出新版本且不提供原地安装。

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
| ~~Omarchy 4 的 Quickshell 是否提供 SNI 宿主~~ **已回答：有**（2026-09-18） | 阶段 3 成立 | 实机看到图标。`busctl` 那条探针只能证明有 watcher——两个主流实现都把 `IsStatusNotifierHostRegistered` 硬编码成 true |
| gpui 的 layer-shell 在 Hyprland 上的实际表现 | 阶段 1 | 最小示例：一个锚在右上的 overlay 窗口 |
| 销毁重建的内存行为 | 阶段 1 的显隐策略 | 开合一百次，看 RSS |
| 通知守护进程是否支持 action | 阶段 5 的横幅点击 | `notify-send` 带 action 试一次 |
| hypridle 是否尊重 idle inhibitor | 防休眠开关在 Linux 上是否该存在 | `systemd-inhibit --what=idle sleep 600` 后等待息屏 |
