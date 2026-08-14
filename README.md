# zstats.app

macOS 菜单栏系统监控面板。界面实现自 Claude Design 项目 `Stats Popover v3 shadcn`，指标由 [zstats](https://crates.io/crates/zstats) 嵌入式采集。基于 [gpui](https://github.com/zed-industries/zed) + [gpui-component](https://github.com/longbridge/gpui-component)。

**支持平台：macOS。** Linux 和 Windows 能编译，但托盘（Linux）、窗口显隐、多屏定位、异常进程扫描、Dock 隐藏都只有 macOS 实现，其余平台的分支从未被验证过。详见「已知问题 / TODO」。

## 源码结构

| 文件 | 职责 |
|---|---|
| `main.rs` | 启动、窗口生命周期、显隐与定位的调度 |
| `placement.rs` | 窗口尺寸与「挂在托盘图标下方」的纯几何（可单测） |
| `window_ext.rs` | 通过 AppKit 直接显示 / 隐藏 / 移动 `NSWindow` |
| `dock.rs` | 两条独立路径的 Dock 图标抑制 |
| `metrics.rs` | 常驻采集线程、自适应节奏、异常进程扫描调度 |
| `procscan.rs` | `sysctl(KERN_PROC_ALL)` 扫僵尸 / 停止进程 |
| `watch.rs` | 三个 zstats 告警看不到的观察器：持续负载、异常进程、接口活跃度 |
| `fullscan.rs` | 一次性全量进程扫描，只在点「全部」时执行 |
| `history.rs` | 读当天历史文件，按累计 CPU 时间排名 |
| `state.rs` | 全局状态：采集结果、告警 episode、窗口几何、UI 选择态 |
| `views/` | 八个视图 + 共用控件 + 设计 token（`theme.rs`） |
| `format.rs` | 所有数字 → 字符串的规则，纯函数、有单测 |
| `notify.rs` | 系统横幅：单线程有界队列 |

## 指标采集

`src/metrics.rs` 起一个**常驻后台线程**跑 `zstats::Monitor::tick()`，经 `smol::channel` 把 `Tick` 交给主线程写进全局 store。

- **为什么常驻**：`Monitor` 内部为 disk / net / 每进程 IO 累积「上一次采样」的基线，重建就丢。窗口是随开随关的 popover，采集器跟着窗口走的话每次开窗速率都显示 `—`。
- **首次采样的速率类指标必然是 `None`**（需要前一个样本做差），UI 统一显示 `—`。这是 zstats 的契约，不是故障。
- 依赖只开 `frontend` feature：拿到告警规则引擎、滚动均值和 settings 模型，且**不引入 tokio**。

### 自适应节奏

| 状态 | 间隔 |
|---|---|
| 面板打开 | config.toml 的 `[daemon] interval`（默认 2s） |
| 面板关闭、整机 CPU ≥ 30% | 同上 |
| 面板关闭且空闲 | 5s |

实测空闲开销：2s = 1.6%、5s = 1.0%、15s = 0.3%。取 5s 是因为托盘标题跟着同一个 tick 走，是面板关闭时唯一还看得见的东西——15s 会让它看起来像卡住了。

节奏读的是 config 里的 `interval` 而不是自己定一个：既然和 CLI 共用 `~/.zstats`，就该共用它的采样率，否则两个进程会对同一条用户写下的设置各行其是。

等待用的是 `recv_timeout` 而不是 `sleep`：点开面板会立刻唤醒采集器采一次，而不是让人看着一个最多差一整个周期的旧数字。

`BUSY_CPU_PERCENT` 只能对**持续**负载起作用——CPU 百分比是两次刷新之间的均值，空闲节奏下 3 秒的尖峰会被摊平到 5 秒里、可能永远够不到阈值。编译和转码抓得住，短尖峰抓不住。

### 与 zstats CLI 共用 `~/.zstats`

配置目录用 `zstats::settings::default_dir()`，与 zstats CLI 共享同一份 config.toml、告警阈值和历史记录。

**前提**：一个系统里只能有一个采集器。如果同时跑 `zstats serve` 守护进程，会双重采集 —— 重复通知、重复写历史。本应用**不做** `is_running()` 检测（那需要开 `client` feature 并引入 tokio），请自行确保不并存。

### 语言与主题偏好存在 `app.toml`，不进 config.toml

Config 页的「界面」卡片可以把语言（跟随系统 / English / 中文）和主题（跟随系统 / 浅色 / 深色）固定下来，写在 `~/.zstats/app.toml`（`prefs.rs`）。不写进共享的 config.toml 是因为 `zstats::settings::save` 只回写它认识的 `[collector]` / `[daemon]` / `[alerts]` 三段——任何额外的键都活不过下一次 `apply_add` 落盘（无论是本应用改阈值还是 CLI 的 `-add`），会被无声丢掉。缺键即跟随系统，所以文件不存在就是默认行为。

强制主题除了换 gpui 主题，还会 pin `NSApp.appearance`（`main.rs` 的 `apply_ns_appearance`）：vibrancy 材质跟的是窗口的 AppKit appearance 而不是我们的 token，不 pin 的话「系统浅色 + 强制深色」会变成深色文字压在浅色 Popover 毛玻璃上——正是背景不透明度注释里说的那种灰雾。切语言则要重建托盘菜单和应用菜单（两者在构建时就快照了翻译文案），面板本身每 tick 整体重绘，重设 locale 即可。

## 界面

八个视图：Overview / Processes / Apps / Hardware / Network / Alerts / History / Config，`src/views/` 一个文件一个（Hardware 由 `disk.rs` + `sensors.rs` 两个卡片栈拼接而成——磁盘、温度、电池同属机器的物理层，各占一个 tab 时后两者常年只有半屏内容）。传感器默认只显示最热的 3 个，头部的「显示更多」chip 展开全部（与 Network 的隐藏接口 chip 同一习语）；超过 80 °C 的传感器永远不会被折叠掉——截断只吞安静的那些。导航是单行图标 tab（Control Center / Stats 的做法），全名走 tooltip。设计 token 在 `src/theme.rs`，卡片用半透明 grouped fill 叠在原生 vibrancy 上，而不是 shadcn 实心描边。

贯穿全部视图的一条规则：**进度条、柱状图和数字默认中性色（`ink`），只有越过阈值才变品牌红（`accent`）**，由 `theme::fill_for()` / `theme::text_for()` 固化。

第二条规则（`views/mod.rs` 有完整说明）：**macOS 不会在可点击元素上改变鼠标指针**，所以 `cursor_pointer` 不能作为「这里能点」的唯一信号。每个可点元素都必须有*看得见*的反馈——hover 填充、边框提亮、或真正的按钮外观。当点击目标是一整张卡片而不是按钮形状的东西时，hover 也不够，要给显式控件：这就是 Alerts 卡片上那个齿轮按钮的由来。

### zstats 告警看不到的三件事（`watch.rs`）

这三个观察器各有自己的时钟和阈值，独立于 gpui（不用 `Context`、不 `notify`、不渲染），因此可以拿一串手工构造的样本直接单测。

- **持续低强度负载**。永远不越过阈值——这正是它的定义——所以没有任何告警规则能命中。判据是对 `ProcessSnapshot::cpu_time_ms`（内核的生命期计数器）做差得到的**积分**，而不是对采样到的百分比取平均：计数器免疫我们自己的自适应节奏、免疫系统睡眠，也免疫单次读数的抖动。它还堵上了百分比版本堵不上的漏洞——一个每 4 分钟跑满 30 秒的进程，每次去看它都在阈值之上，「最近是否超过」的规则会让它的时钟永远走下去，最后报出一个从未发生过的「稳定 10%」；它的积分是 5%，而积分才是现在的判据。
- **异常进程**（僵尸 / 停止）。在 UI 看到进程表之前就已经被排掉了：zstats 选的是 CPU、内存各取一半的 top-N，而僵尸两项都接近零。开发机上实测它们排在第 435、589、591 位。所以 `procscan.rs` 单独走一次 `sysctl(KERN_PROC_ALL)` 扫全表。**不用 libproc**：`proc_listallpids` 只返回当前用户可见的进程且完全不含僵尸，实测只报 169 个而 sysctl 报 666 个，三个异常进程一个都没有（它们属于 root）。
- **接口活跃度**。快照里根本没有：内核的计数器是自开机累计的，说不出字节是*什么时候*动的。所以自己记「本会话内最后一次有流量的时刻」，30 分钟没动的接口收起来。实测 32 个接口里只有 5 个真的在传数据。

僵尸进程**不保证**会被回收——父进程只要不 `wait()`，条目就一直在，直到父进程自己退出。开发机上有两个已经躺了 6 天和 15 天，父进程还活着，这正是值得报出来的那种泄漏。异常状态需要持续 5 分钟才展示：正常退出时子进程 exit 到父进程 reap 之间本来就会短暂产生僵尸。计时从**我们第一次观察到**算起（内核不记录状态转换的时刻），所以永远是个下界，UI 上写作「≥」。

### 告警按 episode 归并

zstats 的告警是 episode 语义：跨越阈值报一次，30 分钟后若仍成立再报一次跟进，然后沉默到值落回并重新武装。所以 `state.rs` 按 `(对象, 度量)` 归并而不是每个事件压一张卡——否则同一个条件会占两张卡，而一个在阈值附近反复进出的进程能独自占满 20 条上限。卡片的 element id 用的是 episode 自己的递增序号，不是队列下标：队列会随着 episode 重新浮上来而重排，用下标会把这张卡的 hover / 展开状态交给顶替它位置的另一条告警。

阈值可以在卡片上直接改：写入 `~/.zstats/config.toml` 的 per-name override（和 CLI 的 `-add` 同一套键），采集线程下一次循环 `reload_settings()`。

### 内存告警卡片上的「退出」按钮（`terminate.rs`）

内存告警是唯一带「动手」按钮的告警：要释放内存只能请走占用者，而 CPU 尖峰大多自己会过去，从告警卡片上驱逐反而武断。这是整个应用**唯一对系统做动作**的地方，边界刻意收得很窄：

- **只响应点击，永不自动**。无人值守的 kill 随时可能带走没保存的文档，所以姿态始终是「通知 + 提供动作」，触发权在用户手里，点了还要过 `confirm.rs` 确认框。
- **判断权仍在 zstats**。按钮消费的是规则引擎发出的 `AlertEvent`，本应用不自己判定「谁该被清理」——和「zstats 拥有告警和数字」是同一条规则。
- **请求可以被拒绝**。LaunchServices 认识的应用走 `NSRunningApplication.terminate()`（等价 ⌘Q，应用可以弹保存对话框甚至拒绝）；裸进程发 SIGTERM（可被捕获清理）。确认框会按点击时判定的层级写明将要发送哪种。**没有 SIGKILL**——它不可拒绝，等于一颗数据丢失按钮；顽固到无视 SIGTERM 的进程该交给活动监视器。
- **发不出去的按钮不画**。渲染前用 `kill(pid, 0)` 做权限探测，root 进程的告警卡片上根本不会出现一个只能失败的控件。

不做市面「一键释放内存」那种气球法清理：人为制造压力尖峰逼内核丢缓存换来的 free 数字，是本应用拒绝自己发明的那类误导性指标。

### History：今天什么烧了 CPU

唯一按**量**而不是按速率排序的视图。其他视图回答的都是「现在有多忙」，而这在结构上就看不见那个从不显得忙、却在一天里花掉最多的进程。

数据来自 zstats 每分钟写一行的 `<config-dir>/data/YYYY-MM-DD.jsonl`（保留 30 天）。两种情况会被记录：越过基础阈值，或者当分钟 CPU 时间排进前五。后者是唯一能看见「没有任何阈值抓得住」的进程的判据，且**只记录不告警**——一个低门槛长窗口的*告警*会对每个正经的常驻守护进程都开火。

累计量用相邻样本做差再取正数步长，两点都重要：进程只在够格的分钟才进文件，会反复进出，对累计计数器做差在任何空档上都保持精确；而计数器**倒退**意味着 pid 被复用，那一步应该贡献 0 而不是一个荒谬的负数。机器睡眠会留下空隙，不做插值。

### 进程内存显示 footprint，读不到才落回 RSS

进程行和排序用的内存数字是 zstats 0.4 起上报的 `phys_footprint_bytes`——macOS 给进程记账的口径（私有脏页 + 压缩页 + GPU/IOKit 分配），即活动监视器「内存」列。只看 RSS 会把 GPU 重度应用整个看错：Metal 缓冲和被压缩的页 RSS 根本看不见，一个 GUI 应用 RSS 读 80 MB、实际吃 300 MB 是常态。`proc_pid_rusage` 读不了别的用户的进程，所以非特权采集对 root 守护进程报 `None`，这时落回 RSS——那是 zstats 对它们仅有的数字。行展开里两个口径并列标注（`processes.rs` 的 `shown_memory`，有测试守 fallback）。

两处**有意不动**：Apps 页的分组内存仍是 RSS 之和（zstats 的分组快照没有 footprint 字段，成员又不全在 top-N 里，自己加总就是在推导 zstats 没说过的数字）；`alert-mem` 告警在上游仍按 RSS 均值份额评估，告警卡片引用的是事件自带的数字，两边各自一致。

### 全部进程：点了才扫（`fullscan.rs`）

进程页默认是采集器的 `max-processes`（默认 50），而且不是纯 CPU 前 50 —— 预算和「按内存排」的榜单对半分，两种排序才都有意义。代价是它回答不了「X 到底在不在跑」，所以表头有一个「全部」。

**点击之前什么都不做**，点击之后也不是改配置，而是在后台执行器上临时起一个 `LocalCollector`（`max_processes = usize::MAX`，其余通道全关），采两次，把结果单独放进 `FullScan`。两个原因：

- `Monitor` 的 `CollectorConfig` 在构造时就烤死了，`reload_settings()` 只重读 `[alerts]`，抬高上限意味着重建 monitor —— 丢掉全部速率基线，并把主列表排序用的 60 秒滚动窗口清零。
- 上限不只是显示条数。逐进程的告警规则和历史文件都在物化出来的集合上判断，永久抬高会让从来不被考虑的进程开始触发告警、让每分钟写入的 JSONL 变长。**界面上的一个问题不该改变什么会告警。**

采两次是因为 sysinfo 的每进程 CPU% 要靠自己的两次采样做差，只采一次会得到一张全零的表，看起来像整机空闲；中间等 `SETTLE`（300ms）。有单测守着这个契约。

实测 694 个进程的机器上，物化全部而不是 50 个，`collect()` 本身在噪声内没有差别（20.2ms vs 20.4ms）—— 全表本来就要遍历一次才能排名，上限只决定给多少条建字符串。多出来的是一对 sysinfo 刷新，以及滚动窗口从 90µs 涨到 1.06ms。

行和主列表共用同一个构建函数（`process_row`），包括点击展开和 kill —— 全量列表的意义就是找到不在 top-50 的进程，找到之后自然要看 cmd、要能结束它。列表用 `gpui::list` 虚拟化（不是 `uniform_list`：后者假设所有行等高，容不下展开），面板每个 tick 整体重绘，690 行如果全量构建，就是为了屏幕上的十几行每两秒重建一次。`ListState` 随扫描结果存进 state —— 每帧重建会把滚动位置弹回顶部 —— 并在新扫描落地时整个换掉，高度缓存和滚动从头开始。

**这份数据的 CPU% 口径和其他视图不同**，卡片上用一行说明写死了这件事：它是扫描那 300 毫秒窗口内的值，不是 60 秒滚动均值。同一个进程在两个列表里合理地读出不同的数字，一个没有解释的第二数字比没有更糟。

### 与设计稿有意的偏差

- **毛玻璃**：设计稿是实心 `#09090b`，这里保留 vibrancy，观感更通透。
- **导航**：设计稿是 4×2 缩写文字（Over / Sens / Conf）。320px 塞不下全名，缩写也不像 macOS，所以改成单行图标 + tooltip。
- **字体**：系统字体做 UI，JetBrains Mono 做指标数字（等宽数位，跳动时不会左右抖）。设计指定的 Archivo 未采用。
- **Config tab 可写**。开关和间隔走 `apply_add` 后重建 `Monitor`（速率基线会丢，下一次采样的速率是 —）；告警基值走 `reload_settings()`，和 Alerts 卡片同一条路径。
- **Apps 展开显示聚合详情而非成员进程列表**。`ProcessGroupSnapshot` 只给出整棵树的汇总，不返回成员清单。
- 设计稿里的假菜单栏和右下角说明文字不实现 —— 那是设计稿自己的展示环境。

## 系统通知

告警触发时发原生横幅，macOS 走 `NSUserNotification`。点击横幅会打开 popover 并切到 Alerts 页。系统横幅的外观不能自定义。

**macOS 上投递是自己的一层薄 `NSUserNotificationCenter` 封装：fire-and-forget + 常驻 delegate。** `deliverNotification:` 是异步 XPC 调用，立即返回；点击由启动时装好的 delegate 在主 run loop 上接收（gpui 本来就在泵它），任何一条横幅的点击都等于「打开 Alerts 页」，所以 delegate 无需携带 per-notification 状态。fire-and-forget 是目的而不是省事：此前走 notify-rust 的 `wait_for_action`（一条常驻投递线程 + 深度 16 的队列），它要等到用户*处理掉*横幅才返回——一条躺在通知中心没人理的横幅会把线程停多久取决于用户什么时候去理，后续横幅在队列里排队，第 17 条起静默丢弃。用户的注意力不是可以串行化的资源；投递与注意力解耦后，队列和停摆这一类问题整个不存在了。非 macOS 平台保留原来的 notify-rust 队列路径（XDG 横幅会自动过期，那里的等待有界）。

`NSUserNotification` 自 10.14 起标记废弃，但它是裸 `cargo run` 进程唯一能用的横幅 API——替代品 `UNUserNotificationCenter` 对没有真实 bundle 的进程直接抛异常，而这个应用一半的生命都在调试器下不带 bundle 运行。

**必须在启动时调用 `notify_rust::set_application(BUNDLE_ID)`。** 它在调用时就把 `NSBundle.bundleIdentifier` swizzle 成我们的 id（进程级，自己的投递路径同样受益），macOS 据此归属横幅。字面量必须和 `Cargo.toml` 里 `[package.metadata.bundle] identifier` 保持一致，有单测守这个跨文件约束。裸 `cargo run` 时这个 id 解析到*已安装*的 zstats.app——这就是调试构建能发横幅的原因，也是没装 .app 时横幅静默不出现的原因。同一个 id 若有多份 .app 注册（比如 Downloads 里留着旧包），归属会摇摆，横幅可能静默丢失：`lsregister -u` 掉多余的那份即可。

## 开发

```bash
make dev      # cargo run
make debug    # RUST_LOG=debug cargo run
make check    # 快速类型检查
make lint     # clippy --deny=warnings
make test     # cargo test --workspace
make release  # cargo build --release
make bundle   # .app（需要 cargo install cargo-bundle）
```

debug 构建启动时直接开窗，失焦也不收起，方便对着 IDE 看；release 构建只有托盘。

## 托盘 popover 模型

应用是菜单栏 popover 形态：**启动时没有窗口**，只有托盘图标。

- `cx.set_quit_mode(QuitMode::Explicit)` —— gpui 默认在非 macOS 平台关掉最后一个窗口就退出进程，零窗口的启动状态会直接退出，所以必须显式改成「只有 `cx.quit()` 才退出」。
- **窗口只创建一次，之后靠显隐复用**（`window_ext.rs`）。gpui 把窗口建模成「创建或销毁」：既没有隐藏单个窗口的 API（`PlatformWindow` 没有可见性控制），也没有移动窗口的 API（只有 `resize`）。而这两件事这里都要——popover 要出现在托盘图标当时所在的位置，而且开关很频繁。

  按 gpui 的模型来（每次开关都销毁重建）**每个循环泄漏约 1 MB**：开关 12 次进程涨了 11.5 MB，其中一半来自一个没有任何视图的空窗口。所以这里越过 gpui 直接驱动 `NSWindow`，窗口只建一次。

  失焦自动收起（仅 release）走 `orderOut`，重新唤起走 `setFrameOrigin` + `makeKeyAndOrderFront`。`was_active` 标志用来跳过窗口刚创建、还没首次激活时的那一次失活回调，否则窗口会一闪即逝。
- **重绘要按可见性门控**。窗口是移出屏幕而不是销毁的，gpui 并不知道它看不见，会老老实实继续渲染一个没人能看到的面板——实测空闲 CPU 因此从 0.6% 涨到 2.0%。`CollectorPace::is_visible()` 同时管采样节奏和「这次 tick 要不要重绘」。
- **跨 Space**。普通窗口属于它被创建时的那个桌面，从别的桌面唤起会让 macOS 切回去——对一个从菜单栏召唤出来的东西来说很突兀。`NSWindowCollectionBehavior::CanJoinAllSpaces | FullScreenAuxiliary`（后者保证在全屏应用之上唤起时不会先退出全屏）。gpui 的 `WindowKind::PopUp` 自带这个行为，但那是 nonactivating panel，拿不到键盘焦点。
- **托盘点击的 toggle**：点图标会先让窗口失焦（触发自动收起），点击事件随后才到。所以 `TOGGLE_GRACE`（300ms）内如果刚发生过自动收起，这次点击就不再开窗 —— 于是表现为 toggle。`took_recent_auto_hide` 会取走标记，只生效一次。
- **托盘图标**：`assets/icons/cpu.svg` 在启动时由 `resvg` 光栅化。用 CPU 而不是趋势箭头：箭头对数据下了断言（「数字在涨」），而主体本身不下断言。

  `tray-icon` **不支持 SVG**，只接受原始 RGBA（内部再编码成 PNG 交给 `NSImage`）。两个要点：macOS 会把图标缩放到 **18pt 高**，所以按 2x（36px）出图才不会在 Retina 上发虚；注册为 template image 后**只有 alpha 通道有效**，颜色由系统按明暗模式重新上色，因此渲染后把 RGB 抹成黑色。glyph 只占画布 78%——lucide 画到 24×24 viewBox 的边缘，1.0 的话图标会有整整 18pt 高，压过旁边约 12pt 的标题文字，系统图标都是自带留白的。另外 lucide 的 `stroke="currentColor"` 是 CSS 上下文关键字，usvg 解析不了，加载前需替换成具体颜色。有单测校验光栅化结果的覆盖率——解析失败会得到一张全透明位图，不报任何错，只表现为图标消失。
- **托盘交互**：左键单击 toggle 窗口，右键弹出菜单（Show Window / Quit）。实现上是 `with_menu_on_left_click(false)` 关掉左键弹菜单，再监听 `TrayIconEvent::Click`；`MenuEvent` 和 `TrayIconEvent` 各用一个阻塞线程，汇入同一个 `smol::channel`。托盘标题显示整机 CPU%，取整到个位（菜单栏很挤，小数会让它每次采样都抖），并且和上次相同就不重设（设标题会让菜单栏重新布局）。
- **无标题栏**：macOS 上 `WindowOptions.titlebar` 留 `None`，而且**必须显式写出来**——`WindowOptions::default().titlebar` 是 `Some(..)`，字段留空会装回一个默认的（不透明、带红绿灯的）标题栏。

  留 `None` 时 gpui 用 `Titled | FullSizeContentView` 的 style mask，且**不含** `Closable`/`Miniaturizable`/`Resizable`（所以没有红绿灯、也不可缩放），同时照样会设 `titlebarAppearsTransparent` + `titleHidden`（`gpui_macos/src/window.rs:815,977`）。得到的仍是普通 titled window，系统圆角和阴影都在，也能正常拿键盘焦点。

  注意 `with_app_identity()` 里补 titlebar 的分支必须跳过 macOS，否则会把红绿灯又装回去。
- **退出按钮**：面板 footer 右侧。accessory app 没有应用菜单栏、没有 Dock 图标可右键、窗口也没有关闭按钮，所以退出必须有个看得见的入口（托盘右键菜单的 Quit 仍在）。
- **窗口定位**（`placement.rs`）：`TrayIconEvent::Click` 带的图标矩形是物理像素，换算成逻辑坐标后，窗口以图标为中心水平居中、下方留 6px，再夹进该显示器的 `visible_bounds()`（已排除菜单栏和 Dock）—— 只有居中会越界时才贴边。纯几何部分是 `anchored_origin()`，单元测试覆盖了居中 / 贴左 / 贴右 / 窗口超高四种情况。`ZSTATS_DEBUG_POSITION=1` 会打印整条换算链，多屏定位出问题时可以定位到具体哪一步。
- **毛玻璃**：`WindowBackgroundAppearance::Blurred`，gpui 在 macOS 上用 `NSVisualEffectView` 实现。仅 macOS 启用：其他平台 `Blurred` 文档标注「not always supported」，退化后是纯透明，会直接看到桌面。

  想看到模糊，上面盖的每一层都必须让路，缺一层就是「完全没有透明效果」：

  1. `Root::render` 会铺一层不透明的 `theme.tokens.background`（`gpui-component/crates/ui/src/root.rs:566`）。它的 `refine_style` 排在那句 `bg` 之后，所以 `root.bg(transparent_black())` 能覆盖掉。
  2. 根视图自己的着色层浓度，深浅色分开定值。深色 `0.55`：主题背景是 `l ≈ 0.04` 的近黑，叠在本来就暗的 material 上，太浓会把模糊压成纯黑。浅色 `0.80`：比深色浓，因为浅色面板是深色文字——material 透得越多，桌面细节越会跟着穿上来跟字打架。0.8 是实测下来既看得出模糊、文字又不受干扰的点。
  3. material：gpui 硬编码 `NSVisualEffectMaterial::Selection`（选中高亮用的），`use_popover_material()` 在窗口首帧把它改成 `Popover`，也就是 AppKit 给菜单栏面板用的那个。
- **无 Dock 图标**（`src/dock.rs`）：有**两个**独立来源会把图标放进 Dock，各治一个，少一个就会闪。

  1. **LaunchServices 在进程启动时注册** → Info.plist 的 `LSUIElement = true`，由 `make bundle` 用 PlistBuddy 写入（cargo-bundle 没有这个字段）。裸二进制没有 Info.plist，所以 `cargo run` 这一段治不了。
  2. **gpui 自己调 `setActivationPolicy(Regular)`** → `suppress_regular_policy()` 在 `main()` 开头 swizzle `-[NSApplication setActivationPolicy:]`，把 `Regular` 那次调用吞掉，其余原样转发。

  为什么必须 swizzle 而不能「事后改回来」：gpui 那次调用是发给 Dock 的 IPC，Dock 收到就开始播图标动画，我们在 `run` 回调里微秒后改回 `Accessory` 也拦不住已经起来的动画。实测加了 `LSUIElement` 的 `.app` 依然会闪，就是这个原因。

  `hide_dock_icon()` 仍然保留：`cargo run` 的二进制是 LaunchServices 直接设成 `Regular` 的，没走 `setActivationPolicy:`，swizzle 碰不到，只能在回调里改。所以开发时依然会闪约 50ms（进程启动到回调执行），打包后不闪。

  验证：`lsappinfo info -only ApplicationType <asn>` 返回 `UIElement`。

  swizzle 是运行时改别人的行为，风险要认：gpui 若改用其他 API 设 policy，这段会静默失效（表现是图标又开始闪，不会崩）。**上游给 `Application` 加一个 activation policy 选项就能删掉它** —— 目前 zed 仓库没有相关 issue。

  代价：accessory app 没有应用菜单栏，`cx.set_menus` 的菜单不再显示。退出只剩托盘菜单的 Quit，或窗口有焦点时的 ⌘Q（keymap 绑定仍有效）。`set_menus` 保留着，改回 `Regular` 就会恢复。
- **scale factor**：换算需要菜单栏所在屏幕的 scale factor，而 gpui 的 `PlatformDisplay` 不暴露它。macOS 走 AppKit 直接读 `NSScreen::screens()[0].backingScaleFactor()`（`screens()[0]` 恒为含菜单栏那块屏，`mainScreen` 则跟着 key window 走）；其他平台回退到主窗口每帧镜像进 `ZStatsAppState` 的值。

窗口虽然不再被销毁重建，**任何需要跨「隐藏 → 唤起」存活的状态仍然必须放进 `src/state.rs`**：gpui 会丢弃当帧没有绘制的元素状态。窗口尺寸、当前 tab、每个 tab 的滚动位置都是这样保存的——滚动位置尤其如此，只给每个 tab 一个不同的 element id 是不够的，因为任一时刻只有活跃 tab 被绘制，句柄必须由 store 持有。

## 性能

release 构建、托盘常驻不开窗、cputime 差值 ÷ 墙钟实测：

| 指标 | 值 |
|---|---|
| 闲置 CPU | 0.48% |
| physical footprint | 15.8 MB |
| RSS | 75 MB（绝大部分是共享的框架页） |
| 线程 | 6 |
| 二进制 | 11 MB |

三分钟内存曲线在 t=60s 后走平（正好是 60s rolling 窗口填满的时刻），无泄漏。

测 CPU **不要用 `ps -o %cpu`**：那是个衰减均值，会被进程启动阶段污染，得出的数字毫无意义。用两次 cputime 采样做差除以墙钟。

## 已知问题 / TODO

### Linux 暂不支持托盘

`src/tray.rs` 整体被 `#[cfg(not(target_os = "linux"))]` 关掉，`tray-icon` 依赖也只声明在 `[target.'cfg(not(target_os = "linux"))'.dependencies]` 下。

原因：`tray-icon` 在 Linux 上通过 libappindicator / GTK 实现，菜单事件依赖一个 GTK main loop，而 gpui 在 Linux 上跑的是自己的 X11 / Wayland 事件循环，两者无法直接共存。

后续可选方案（未验证）：独立线程跑 GTK main loop 只驱动托盘，通过 channel 与 gpui 主线程通信；或改用 StatusNotifierItem（D-Bus）的纯 Rust 实现绕开 GTK；或在 Linux 上放弃托盘退化成普通窗口应用。

### 多显示器：gpui 的 display bounds 不可用

`gpui_macos` 的 `PlatformDisplay::bounds()` 拿到 `CGDisplayBounds`（全局坐标）后把 `origin` 丢掉设成 `Default::default()`，于是**所有显示器都报告为 `(0, 0)`**，多屏下彼此无法区分——它自己的注释还写着「0 is the top left of the primary display」。

后果是按位置查找屏幕永远命中第一块，面板被钉死在主显示器上。所以 macOS 走 `window_ext::visible_bounds_containing()`，直接遍历 `NSScreen`。实测两块 4K 屏：gpui 报 `(0,0 1920x1080)` ×2，AppKit 报主屏 `(71,30 1849x1050)`、副屏 `(1920,30 1920x1050)`。

其他平台仍用 `cx.displays()`。

### 多显示器不同 DPI 时托盘定位会偏

托盘报的是物理像素，换算成逻辑坐标需要 scale factor，而选哪块屏的 scale 又得先知道图标在哪块屏——互为前提。现在统一用 `screens()[0]`（菜单栏所在屏）的倍率，所以**多屏同 DPI 时正确**，混合 DPI（比如内置 Retina + 外接 1x）时托盘在副屏的定位会偏。彻底修需要按屏试算再回选。

### 非 macOS 分支未经验证

`main.rs` 与 `metrics.rs` 里保留着一批 `#[cfg(not(target_os = "macos"))]` 分支（窗口用销毁代替隐藏、`cx.displays()` 查屏、无毛玻璃）。它们能编译，但从未在真实环境跑过，也没有测试覆盖。保留是为了不堵死后续移植，但不应当理解为「支持」。
