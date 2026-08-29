# zstats.app

macOS 菜单栏系统监控面板。界面实现自 Claude Design 项目 `Stats Popover v3 shadcn`，指标由 [zstats](https://crates.io/crates/zstats) 嵌入式采集。基于 [gpui](https://github.com/zed-industries/zed) + [gpui-component](https://github.com/longbridge/gpui-component)。

**支持平台：macOS。** 其它平台**当前编译不过**（见「已知问题 / TODO」）：托盘（Linux）、窗口显隐、多屏定位、异常进程扫描、Dock 隐藏都只有 macOS 实现，而其中三个模块被六处无条件 import 引用。

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
| 面板关闭 | 5s |

实测空闲开销：2s = 1.6%、5s = 1.0%、15s = 0.3%。取 5s 是因为托盘标题跟着同一个 tick 走，是面板关闭时唯一还看得见的东西——15s 会让它看起来像卡住了。曾经用整机 CPU ≥ 30% 在隐藏时仍钉住 2s，编译期间能从 1.6% 降到 1.0%（0.6% 的一核），而那时机器已经忙、托盘反而是还在看的东西；进程表本来就是 15s 墙钟，2s 并不会把它采得更勤。隐藏后一律 5s。

节奏读的是 config 里的 `interval` 而不是自己定一个：既然和 CLI 共用 `~/.zstats`，就该共用它的采样率，否则两个进程会对同一条用户写下的设置各行其是。

等待用的是 `recv_timeout` 而不是 `sleep`：点开面板会立刻唤醒采集器采一次，而不是让人看着一个最多差一整个周期的旧数字。

### 与 zstats CLI 共用 `~/.zstats`

配置目录用 `zstats::settings::default_dir()`，与 zstats CLI 共享同一份 config.toml、告警阈值和历史记录。

**前提**：一个系统里只能有一个采集器。如果同时跑 `zstats serve` 守护进程，会双重采集 —— 重复通知、重复写历史。本应用**不做** `is_running()` 检测（那需要开 `client` feature 并引入 tokio），请自行确保不并存。

### 语言与主题偏好存在 `app.toml`，不进 config.toml

Config 页的「界面」卡片可以把语言（跟随系统 / English / 中文）和主题（跟随系统 / 浅色 / 深色）固定下来，写在 `~/.zstats/app.toml`（`prefs.rs`）。不写进共享的 config.toml 是因为 `zstats::settings::save` 只回写它认识的 `[collector]` / `[daemon]` / `[alerts]` 三段——任何额外的键都活不过下一次 `apply_add` 落盘（无论是本应用改阈值还是 CLI 的 `-add`），会被无声丢掉。缺键即跟随系统，所以文件不存在就是默认行为。

强制主题除了换 gpui 主题，还会 pin `NSApp.appearance`（`main.rs` 的 `apply_ns_appearance`）：vibrancy 材质跟的是窗口的 AppKit appearance 而不是我们的 token，不 pin 的话「系统浅色 + 强制深色」会变成深色文字压在浅色 Popover 毛玻璃上——正是背景不透明度注释里说的那种灰雾。切语言则要重建托盘菜单和应用菜单（两者在构建时就快照了翻译文案），面板本身每 tick 整体重绘，重设 locale 即可。

## 界面

八个视图：Overview / Apps / Processes / Hardware / Network / Alerts / History / Config，`src/views/` 一个文件一个（Hardware 由 `disk.rs` + `sensors.rs` 两个卡片栈拼接而成——磁盘、温度、电池同属机器的物理层，各占一个 tab 时后两者常年只有半屏内容）。传感器默认只显示 4 个，按**距各自上限的占比**排序（条子画的就是这个占比，所以列表天然是一段递减的阶梯）而不是按绝对温度——一刀切的 80 °C 线在满载的 Apple Silicon 上会把全部 pACC/eACC 通道染红（34 条红等于零条红），却让一颗离自身 60 °C 上限只差 5 度的电池传感器保持中性。着色同样按占比（≥90% 才红），排序键和着色键是同一个数，于是「贴近上限的永不被折叠」是结构性成立的——截断只吞安静的那些。头部的「显示更多」chip 展开全部（与 Network 的隐藏接口 chip 同一习语）。导航是单行图标 tab（Control Center / Stats 的做法），全名走 tooltip。设计 token 在 `src/theme.rs`，卡片用半透明 grouped fill 叠在原生 vibrancy 上，而不是 shadcn 实心描边。

贯穿全部视图的一条规则：**进度条、柱状图和数字默认中性色（`ink`），只有越过阈值才变品牌红（`accent`）**，由 `theme::fill_for()` / `theme::text_for()` 固化。

第二条规则（`views/mod.rs` 有完整说明）：**macOS 不会在可点击元素上改变鼠标指针**，所以 `cursor_pointer` 不能作为「这里能点」的唯一信号。每个可点元素都必须有*看得见*的反馈——hover 填充、边框提亮、或真正的按钮外观。当点击目标是一整张卡片而不是按钮形状的东西时，hover 也不够，要给显式控件：这就是 Alerts 卡片上那个齿轮按钮的由来。

第三条规则（`theme::tiny_label` 有完整推导）：**≤9.5px 的固定标签走 `theme::tiny_label(base)`**——英文原样返回 `base`，中文提到全对比 `text()`。小字号汉字四面受敌：UI 字族（SF Pro）没有汉字、回退到没有 optical size 轴的 PingFang，笔画密度是拉丁的近十倍，gpui 在 macOS 只做灰度抗锯齿，而面板还是毛玻璃。抬字号试过并退回（一个点乘上 1.618 行盒会动整页节奏），对比度是唯一零布局代价的杠杆。**只限标签、绝不用于控件**：开关的静默灰是状态（`showing ? text() : muted`），排序 chip 的灰是 hover 提亮的起点，套进去等于谎报状态。同一条腿上还有 `font::mono_unless_cjk`：JetBrains Mono 一个汉字都没有，把译文整句塞进等宽面（卷卡片的读写行、History 的峰值行）等于让每个汉字在为数字挑的字号上逐字回退 PingFang——中文下这两个元素交回 UI 面，英文保留等宽对齐；行盒 = 字号 × 行高、与字族无关，所以零布局代价。

第四条规则（zstats 0.5.3 起）：**`display_name` 只管展示，`name` 永远是匹配身份**——这是上游在字段 doc 里划的线，本面板全盘沿用。人读的地方用显示名：Apps 行标题与成员行、Overview 的树、告警卡与横幅、压力告警的占用者（上游原话：「压力告警里人要动手处理的那一半」）、History 行（「横幅说的是 CodeBuddy CN，行里就不能写 Electron」）、退出确认框。机器匹配的地方永远用 `name`：告警线解析（`app_bars`）、模板与覆盖、阈值编辑。Apps 展开区在标题是显示名时补一行「进程名 …」——阈值匹配的那个身份必须在某处可读，否则「告警线」两行会显得对不上号。歧义 pid 标记按**屏幕上显示的字符串**计：两个都叫 Electron 但显示名不同的树不歧义，反之才歧义。筛选框两个身份都匹配（用户可能记得任一个）。

### zstats 告警看不到的三件事（`watch.rs`）

这三个观察器各有自己的时钟和阈值，独立于 gpui（不用 `Context`、不 `notify`、不渲染），因此可以拿一串手工构造的样本直接单测。

- **持续低强度负载**。永远不越过阈值——这正是它的定义——所以没有任何告警规则能命中。判据是对 `ProcessSnapshot::cpu_time_ms`（内核的生命期计数器）做差得到的**积分**，而不是对采样到的百分比取平均：计数器免疫我们自己的自适应节奏、免疫系统睡眠，也免疫单次读数的抖动。它还堵上了百分比版本堵不上的漏洞——一个每 4 分钟跑满 30 秒的进程，每次去看它都在阈值之上，「最近是否超过」的规则会让它的时钟永远走下去，最后报出一个从未发生过的「稳定 10%」；它的积分是 5%，而积分才是现在的判据。**两个门槛是面板自己的偏好**（`watch::SustainedRule`，从 `app.toml` 读：`sustained_hours` 缺省 2、`sustained_divisor` 缺省 3，Interface 卡上两行 chip）：这个观察器按定义在 zstats 的线之下，归面板管，所以放进 app.toml 不碰铁律；暴露的是**时长**和**除数**而不是一个百分比——门槛 = `alert-cpu ÷ 除数`，用户改了 `alert-cpu` 这条线要跟着走，否则两条线会悄悄脱节。规则在每次提问时从 prefs 现组（`state.sustained_rule()`），所以选择器一改下一个 tick 就生效，不重建采集器；grace（5 分钟）不暴露，它是实现细节不是判断。范围外的值读作缺省（10 分钟以下是 burst 探测器，规则本来就管；÷1 就是告警线本身）。
- **异常进程**（僵尸 / 停止）。在 UI 看到进程表之前就已经被排掉了：zstats 选的是 CPU、内存各取一半的 top-N，而僵尸两项都接近零。开发机上实测它们排在第 435、589、591 位。所以 `procscan.rs` 单独走一次 `sysctl(KERN_PROC_ALL)` 扫全表。**不用 libproc**：`proc_listallpids` 只返回当前用户可见的进程且完全不含僵尸，实测只报 169 个而 sysctl 报 666 个，三个异常进程一个都没有（它们属于 root）。
- **接口活跃度**。快照里根本没有：内核的计数器是自开机累计的，说不出字节是*什么时候*动的。所以自己记「本会话内最后一次有流量的时刻」，30 分钟没动的接口收起来。实测 32 个接口里只有 5 个真的在传数据。

僵尸进程**不保证**会被回收——父进程只要不 `wait()`，条目就一直在，直到父进程自己退出。开发机上有两个已经躺了 6 天和 15 天，父进程还活着，这正是值得报出来的那种泄漏。异常状态需要持续 5 分钟才展示：正常退出时子进程 exit 到父进程 reap 之间本来就会短暂产生僵尸。计时从**我们第一次观察到**算起（内核不记录状态转换的时刻），所以永远是个下界，UI 上写作「≥」。

### Overview 的「过去一小时在涨」（`trend.rs`）

第四个同类观察器。CPU% 是速率，速率没有记忆——快照分不清「一直 30%」和「十分钟前还是 2%」，而 zstats 的滚动均值只回看 60 秒。可是打开面板的原因多半是「机器*变*吵了」，此刻的排名对这个问题结构性失明：常年第一的常驻户是正常，从无到有爬起来的那棵树才是新闻，而任何瞬时排名里稳定的 30% 都压过 2%→21% 的爬升者。

存储是每棵应用树 60 个分钟槽、u16 单核百分比（`u16::MAX` 作无数据哨兵），每分钟取**最大值**——「在涨」问的是够到过哪，取均值会让分钟内一个空闲样本把真实的爬升拉回去。约 100 棵树共 ~20 KB。键是 `trend::tree_key`——**显示名优先、进程名兜底**的呈现层身份（zstats 0.5.3）：每个未改名的 Electron 应用向内核报的都是 `Electron`，按进程名做键曾把不相干的程序并成一条曲线。不用 root pid 是因为重启打断、复用还会把两个程序接成一条曲线（和告警卡片用 `SeenAlert::live` 把门是同一个理由）。喂入端（state）和读取端（Overview 的查询与行标题）必须用同一个字符串，所以这个函数只定义一次、定义在 trend 里。

两种「缺数据」严格分开（和 History 不画折线是同一条诚实原则）：**采集器没报告的分钟**（睡眠、启动前）不可知，比较的两侧都不算；**报告了但这棵树不在 groups 里**说明它在采集器 top-N 之下——对 CPU 而言真的安静，按零计，这正是「从无到有」能记满全程涨幅的原因。

**但第二条只对 CPU 成立，内存必须跳过而不是记零**——这是 `rise` 与 `climb` 唯一分道的地方，原因全在「那张表按什么排序」：groups 表就是一张 **CPU 排行榜**（`aggregate_process_groups` 按 cpu 排序后 `truncate(max_processes)`，默认 50），掉出榜外本身就是 CPU 低的证据；内存的排序键仍是 CPU，缺席对 footprint 不含任何信息，记零等于把「这棵树安静了一阵子」伪造成一段与它整个 footprint 等大的涨幅。实测抓到：终端 Ghostty 常驻 ~1.1 GB，随命令忙闲在榜内外进出，横幅报出「这一小时涨了 1.1 GB · 此刻占用 1.1 GB」——两个数字完全相等，正是基线被读成 0 的签名。这台机器 700 棵树、榜单 50 个，进出是常态而非边缘情况。

卡片双模：有树越过 `RISE_FLOOR`（15 个单核百分点，低于它是调度噪声）时按涨幅排、标题换成「在涨」、行尾带 ↑ 涨幅标签（**中性灰**——上涨是新闻不是越线，accent 仍只留给阈值）；安静的一小时退回当前占用 top-5，否则就是在给 ±噪声排名。仅展示：不产生 `AlertEvent`、不发横幅、不改任何着色规则。行本身可点（hover 填充作 affordance，和它要去的 Apps 行同款）：跳到 Apps 页、选中并展开那棵树（`state.reveal_app`——选中而非 toggle，落到已展开的树上不许把它折回去），Apps 列表在那一帧把该行滚进视野（`take_app_reveal` 一次性标记，之后读者自己的滚动说了算；目标被名字过滤器挡住时不滚，无处可去就不假装去了）。

**同一套环再喂一次内存（`state.mem_trend`）**，问的是泄漏：一个进程 footprint 一小时从 300 MB 涨到 1.5 GB、还没碰到 `alert-mem`，规则引擎按定义看不见，而这正是「看见时已经晚了」的那类事。单位是 MB（`trend::MIB`），u16 槽封顶 ~64 GB——就是整机。读取端不是 `rise` 而是 `climb`：**最新五分钟对本小时最早上报的五分钟**，量的是整段爬升，按小时均值做基线会把一条匀速爬升腰斩；另外两条门槛都在 `trend.rs`：至少上报 20 分钟才给结论（footprint 本该慢慢动，五分钟对它不是趋势），以及最新几分钟必须仍在小时最高值的 90% 以上（`CLIMB_HOLD`）——涨过又释放的是结束了的尖峰，不是泄漏。呈现是内存卡底部一条弱化的行（`mem_climb_strip`，`Chrome +1.2 GB · Code +420 MB`），门槛是整机内存的 5%、夹在 256 MB 与 1 GB 之间（`trend::mem_rise_floor`），没人在涨就不占高度；一小时涨过 10%（夹在 1–2 GB，`trend::creep_notify_bytes`）走 `notify::post_memory_creep`，和持续负载同款静默横幅，`creep_notified` 做再武装集合（name → 首次点名时刻的 map），**按钟点清退（`CREEP_REARM` = 一个环长），绝不按数值**——第一版是「climb 跌回门槛以下即再武装」，实测被 GC 锯齿打穿：Chrome 的 footprint 绕高位摆动，climb 每隔几分钟在 1 GB 线上穿越一次，每次穿越都读作新爆发，29 分钟三条横幅；单个 tick 根本分不清「爬升结束」和「采到了锯齿的低谷」。按钟点则到期时横幅描述的那一小时已完整滑出环外，再公告量的是整体晚于上一条横幅的基线——这才是新闻，也才是本意的「一小时说一次」。仍然只展示、不评估：比较的两端都是 zstats 的 `phys_footprint_bytes`。横幅在外期间，Alerts 页有一张与持续负载卡同款的只读卡承接（`state::creeps_active`，卡按首次点名时刻插进时效序）：横幅点击一律落在那一页，落在一张不提爬升者的页面上，通知读作指向了空处。卡上的行随现状活更新，短暂跌破门槛不撤行——点着 20 分钟前的横幅进来的人必须还能找到主语；只有爬升真正结束（离开高位）或小时翻篇才撤。armed 列表同时点名这个观察器（一小时 ≥1 GB），否则那张卡和横幅像凭空出现。

### 「已有 N 没有使用」——慢燃横幅的加权（`active.rs`）

持续负载和内存爬升这两个观察器能说出一棵树在烧 CPU、在涨内存，却说不出**人还在不在用它**。而这正是发现与误报的分界：你正在敲的编辑器在编译是工作，一个上午没碰过的 app 烧同样的 CPU 才是新闻。内核对此没有意见——「前台」是 AppKit 的概念——所以面板问 AppKit，答案挂在两个观察器已经在发的横幅正文后面（`unused_clause`）。

**事件驱动，不轮询**：订阅 `NSWorkspace` 的 `didActivateApplication`，回调里就一次哈希表写入，频率是人切换应用的频率（每分钟几次封顶），查询只发生在横幅成文的那一刻。轮询 frontmost 是严格更差的方案：每 tick 采一次仍会漏掉 tick 之间的切换，却要为一个每小时问两次的问题永远付出每 tick 的代价。公开 API，无 entitlement、无 TCC 弹窗——实测非 bundle 的裸进程也照收（窗口*标题*才需要录屏权限，激活与窗口归属不需要）。**被激活的 app 在 `userInfo[NSWorkspaceApplicationKey]` 里，不在通知的 `object`（那是 NSWorkspace 自己）**——这一条是实测出来的，读错位置时观察者一切正常却永远拿不到 pid。

**激活是「应用」级事件，不是窗口级**——同一个 app 的两个窗口之间切换不触发。这一条决定了两处设计：其一，光靠表会在本功能最该避免的场景上翻车——一个连续用了三小时、期间从没切走过的 app，时间戳还停在三小时前切进来的那一刻，横幅会对着你正在打字的窗口说「已有 3 小时没有使用」；所以**提问时直接问一次当前 frontmost，在前台的永远读作「在用」**（一小时最多两次的调用，代价为零）。其二，永远不可能变成活动应用的东西（后台 agent、裸进程）根本不会进表，这正是它们永远不会被加这句话的原因。

三条边界都承重：门槛 `UNUSED_AFTER` = 1 小时（低于此，读者为看横幅刚切走的那个 app 就会中招，技术上没错，实际是噪音；这也正好是两个观察器各自度量的窗口，于是一句话的两半说的是同一段时间）；**条目随应用退出清除**（`forget`，订阅 `didTerminateApplication`）——不是为了省内存（256 条上限本来就够小），而是因为 macOS 会把小 pid 直接发回去，继承来的旧时间戳会让横幅对一个刚起几分钟的进程宣称「几小时没用」，和告警卡片那道 `SeenAlert::live` 门是同一类错误：pid 只在进程活着时才是身份；**没见过的 pid 是「没有答案」，不是「很久没用」**——本会话没看到过的切换就是不知道，一个十分钟前启动的面板不了解今天上午，横幅不能声称一段读者无法核对的时长。数据只活在本次会话，不落盘。

### MobileAsset 行说的是系统自己的声明（`assetinfo.rs`）

整盘范围把 `/System/Library/AssetsV2` 暴露出来之后，最大的几行长这样：`com_apple_MobileAsset_UAF_Siri_Understanding/purpose_auto` 2.5 GB。看得见体积、读不懂主语，等于没说。

**没有手写"这是什么"的规则表**，因为 `cleanhints` 的取材标准明确禁止：条目只能来自工具自己的文档，而 Apple 不公开文档说明这些名字。凭经验写一张表，等于在系统文件上用 tooltip 猜测删除安全性。

改为**引用资产自带的 `Info.plist`**：`CFBundleIdentifier`（类型）、`__RequiredByOS`（系统是否必需）、`__AssetDefaultGarbageCollectionBehavior`（`NeverCollected` / `Precious` = 系统不自动回收）、`AssetLocale`（语言）。这与 `CACHEDIR.TAG` 是同构的——**拥有者自己声明**——克制也相同：声明换来一句话，绝不换来删除按钮，因为这些内容归 `mobileassetd` 管，手删可能被重新下载或弄坏资产状态；行上那句收尾指向系统设置 → 通用 → 储存空间。

三条边界：**只报一致读数**（一个类型目录下的资产可能互相矛盾，多数决对系统文件不算事实）；**每行最多读 8 份 plist**（实测一个类型目录下 0～57 份不等，超过就只报类型名——从抽样下结论等于替没看过的资产说话）；**没声明的字段就不说**（实测 156 份里只有 8 份声明了回收策略、56 份声明了 `__RequiredByOS`，所以多数行只有类型名，这本来就是诚实的结果）。类型名一律从目录名读出（`com_apple_MobileAsset_` 之后的部分，就是 Apple 自己的标识符把点换成下划线），不需要读盘。行上带一个「系统」pill，因为没有它没人知道去悬停——它和 cache pill 的区别在于：那个说"这行可以清"，这个说"这行不归你删"。plist 读取走 `plutil`（和 updater 读 Info.plist 用 `defaults` 同一姿态：这些文件二进制和 XML 都有，交给系统自己的工具解），发生在扫描结束、表截断到 20 行之后，所以次数由展示行数封顶；结果随缓存一起持久化，重开窗口不必重读。

### 阈值在「进程」「应用」两页也能改

此前调阈值只有一个入口：告警卡片。也就是说**只有在告警已经出现时才调得动**——想说「这个程序允许跑高一点」的时刻，恰好是它刚打扰过你的时刻，事前根本没地方设。而这两页的展开区本来就显示着该对象的两条线（`alert_bar_rows`，回答「这行 300% 为什么不报警」），把它变成可编辑是最短的路径。

放在**两条线下面**而不是 Quit 按钮旁边（用户最初的提法）：一个对象有**两条**阈值（CPU 与内存），共用动作区里的一个按钮说不清它改的是哪条，而贴着数值的一排 chip 可以。chip 的取值、写入路径都与告警卡片完全共用（`alerts::presets` / `configured_value` / `state::apply_alert_override` → CLI 的 `apply_add`），所以这不是第二种设置规则的方式，只是同一种方式的第二个入口；进程页写 `alert-cpu` / `alert-mem`，应用页写 `alert-app-cpu` / `alert-app-mem`，键由调用点传入而不是控件自己猜——200% 对一棵树平平无奇、对单个进程荒谬，弄混就是把进程的线写到树上。chip 点击要 `stop_propagation`：它嵌在负责折叠展开的可点击行里，否则点一下阈值会顺手把详情收起来。

### 「我给谁设过规则」要能回头看（Config 的覆盖卡片）

README 把「能对不同 app 指定不同规则」当卖点，而这些规则此前只能**写**不能**读回**：告警卡片上按对象设一条，配置页只显示条数（`alert-cpu · 12 条覆盖`），既看不到是哪十二个、各是多少，也没有任何撤销入口——唯一的出路是手改 `config.toml`，而这个应用存在的意义之一就是让人不必去改那个文件。用了几个月之后，用户对自己设过什么完全失控。个性化监控如果不可回顾，个性化就成了负债。

卡片列出五张覆盖表里的每一条（进程 CPU / 进程内存 / 应用 CPU / 应用内存 / 卷），**按规则分组、组内按名称**：规则决定了数值的含义（200% 对一棵应用树平平无奇，对单个进程荒谬），而一个对象通常只出现在一条规则下；组内顺序就是 `BTreeMap` 自己的顺序，跨帧稳定，行不会在读到点下去之间移位。每行给出对象、规则的键（`zstats -add` 和 config.toml 里的那个拼法，可直接抄进终端）、值（`0` 显示为「关」——那是该对象主动退出这条规则，不是没设）。

**「恢复默认」而不是垃圾桶图标**：撤掉的只是这一行，该对象回到大家共用的规则上，重新设置在告警卡片上两下即可，所以也**不加确认弹窗**——本应用那两个需要确认的动作移动的是真东西（文件进废纸篓、信号发给进程），而这里改的是一行配置，且那一行就摆在你眼前。删除走 `state::remove_alert_override` → CLI 自己的 `apply_remove`，与写入路径对称，面板和 `zstats -remove` 对「删除意味着什么」不可能产生分歧；`[alerts]` 是唯一原地重载的段，所以不重建采集器、不丢速率基线。一条覆盖都没有时整张卡片不出现——上面的阈值卡片已经说了每条规则都在基值上，空卡片是多余的。

### 告警按 episode 归并

zstats 的告警是 episode 语义：跨越阈值报一次，30 分钟后若仍成立再报一次跟进，然后沉默到值落回并重新武装。所以 `state.rs` 按 `(对象, 度量)` 归并而不是每个事件压一张卡——否则同一个条件会占两张卡，而一个在阈值附近反复进出的进程能独自占满 20 条上限。卡片的 element id 用的是 episode 自己的递增序号，不是队列下标：队列会随着 episode 重新浮上来而重排，用下标会把这张卡的 hover / 展开状态交给顶替它位置的另一条告警。

这份列表会**镜像到按日的文件 `~/.zstats/alerts-YYYY-MM-DD.toml`**（`alertlog.rs`，0600，写临时文件后 rename）：重启后早上烧过的那条还在，而不是一张空表暗示「今天很安静」。当天的文件喂列表，范围与 History 的日界一致；恢复只是记忆，不重发横幅、不重新评估任何条件——恢复回来的 episode 就是同一条件再次触发时归并进去的那一条（`reports` 继续累加，`span` 仍从早上算起）。解析失败的条目单条丢弃，半截文件的代价是它截断的那几条，不是整张表。

**往前的文件是「过去 7 天」只读块的来源**（`alertlog::recent`，`state.alert_history`）。「这周响了几次、都是谁」是监控最基本的回看，而此前跨日即丢。几条规则：文件按本地日命名，保留 30 天（`RETENTION_DAYS`，和 zstats 自己的日记录同一个月，每天第一次写入时扫一遍）；`save` 只把当天的 episode 写进当天的文件，别的日子的 episode 在它落地那天就已经写进了它自己的文件，所以跨午夜的会话不会把昨天混进今天；**✕ 关掉的卡片不从文件里消失**，而是带 `dismissed = true` 写回（`state.dismissed_today`）——「响过并且我看到了」仍然是发生过的事，没有它这周的记录就是假的；加载当天文件时跳过它们，读往日时包含它们并标「已关闭」。读取只在三个时点：启动、进入 Alerts 页、跨日退休时——从不在绘制里做文件 IO。展示是每天一个 shell、每条一行（时刻 · 严重度点 · 对象 · 类别），最多 6 行再折成「当天还有 N 次」，没有按钮、不可展开：pid 已是历史，没有任何可操作的东西；空周要说「就本应用看到的而言」——记录只在它运行时写入。旧的单文件 `alerts.toml` 在没有当天文件时读作当天，第一次保存后删除。

展示按**时效**而不是按类型。本会话报到过的 episode 在上，按最近一次报到排；两张观察器卡（持续负载、在报的内存爬升）插进这一组——各按观察器开始指向它的时刻，不是按各自的门槛——因为它们不是告警，但是此刻还在发生的事。上次会话恢复的记录沉到「今天早些时候」下面，不再压住第一眼。卡面上的差异留下（严重度、✕、阈值；持续负载仍只读），排序键改成「还要不要你管」。

持久化带来两条必须成对存在的规则。其一，**卡片上的动作按钮多了一道门**：除了
`kill(pid, 0)`，还要求这条 episode 在**本次会话**里被报告过（`SeenAlert::live`）。
理由是 pid 回收——早上那条卡片写着「Chrome · 923」，重启后 923 早已归属别的进程，
而 `can_quit` 只回答「我有权限给这个 pid 发信号」，从不回答「它还是不是那个程序」。
恢复的卡片因此是只读记录（带「上次会话」标签），要结束进程去进程页，那里的 pid
来自实时快照。其二，**列表需要一个确认出口**：每张卡右侧的 ✕ 只删记录（条件仍成立
时下次报告会带回来），否则 tab 上那点强调色会整天亮着而用户无法应答；同理，
跨过本地午夜时列表会自行退休非当日的 episode，「当日」才不至于悄悄变成「自本次启动以来」。

阈值可以在卡片上直接改：写入 `~/.zstats/config.toml` 的 per-name override（和 CLI 的 `-add` 同一套键），采集线程下一次循环 `reload_settings()`。

### 告警阈值表可以从 zstats 仓库拉更新（`alerttpl.rs`）

zstats 按平台编译进一张表，给「本来就忙、本来就大」的程序抬高（或归零）告警线——没有它，`kernel_task` 跑到 300% 也会报警。它同时会读 `~/.zstats/template.toml` 顶掉内置表（整体替换，不叠加），而 zstats 自己的注释写明了 HTTP 不该放在采集器里：*"Keeping it a plain file is what makes 'refresh the table on a schedule' a one-line cron job (`curl -o`) instead of an HTTP client inside a local metrics collector"*。本应用正好是那个客户端，于是补上另一半。

抓的是 **zstats 仓库**的 `templates/alerts-<os>.toml`（不是本仓库的 assets），且盯 `main` 而不是与所钉 crate 对应的 tag：格式版本很少动、表的内容很勤动，盯 tag 等于永远和已编译进去的那份一模一样，更新按钮永远找不到更新。代价是远端可能领先出一个格式版本，这由 `VersionMismatch` **单独成一种结果**说清楚——和「下载失败」共用一句话会把人支去查网络，而他要做的是更新应用。

这不违反「zstats 拥有告警」：写这个文件和 Alerts 卡片用 `apply_add` 写 `[alerts]` 是同一件事——把字节放进 zstats 自己的配置，判断仍然全在规则引擎里。三条性质撑着它的安全边界，缺一不可：

- **用户覆盖优先于模板**（用户 `-add` > 模板 > 基础规则），所以更新永远推不翻你在 Alerts 页手调过的阈值。
- **落盘前先校验**：`Template::parse` 查格式版本、拒未知表、拒任何 matcher 认不了的 key，空表也当无效拒掉（它能解析，但会一次性抹掉全部豁免，那比不更新糟得多）。本应用因此不可能写出一个让 `reload_settings` 开始失败的文件。
- **和内置表一致就什么都不写**。远端跟着 `main` 走，两次发布之间它通常和 crate 里编译进去的那份完全相同；照写不误会凭空造出一个覆盖文件，而它会压过*今后每一次* crate 升级带来的新内置表——更新按钮会悄悄把机器钉死在今天的阈值上。比较的是阈值不是字节：上游改一句注释不该导致共享配置目录里发生一次写入。

这张表还有第二个读者：进程页和 Apps 页的**展开区各多两行「告警线」**——这个名字要越过哪条线才会出现在 Alerts 页（`processes::alert_bar_rows` / `apps::app_bars`）。解析走 zstats 自己的 `ActiveThresholds::from_config_with_template`，用的就是上面缓存的那份表，所以面板显示的线和引擎武装的线不可能分叉；「覆盖」标记表示有按名条目命中（用户的或模板的——哪层赢的 zstats 不公开，这里也不越权重实现它的优先级去猜）。这两行是对「为什么它 300% 了还不报警」的回答：因为模板给它的线本来就不在 80。

齿轮圆点也有它的一半：**静默探测**（`silent_check`）骑着 updater 那口两天一次的钟（同一个后台 pass，整个 app 只有一种主动网络节奏），抓取已发布的表**只为比对、绝不落盘**——这张表决定什么条件响铃，定时应用远端内容等于把告警引擎交给远端遥控，所以「应用」永远只在卡片按钮后面。只有「干净、可用、且阈值不同」才点亮：改措辞不算（比对的是阈值指纹不是字节，否则狼来了一次圆点就废了）；格式领先（`VersionMismatch`）不点亮——指着一个按下去会失败的按钮的圆点是折磨，那种情况由 updater 自己的圆点覆盖（格式升级必然伴随发版）。断网保留上次探测的结论（断网对表一无所知）。卡片上出现「这份不要」可按内容指纹忽略（`template-check.toml`，纯展示层，同 updater 的跳过）；按下更新或本地已一致时，offer 随状态自动熄灭——圆点因事实而灭，不靠记账赛跑。

覆盖文件被拒时单独报一种状态（`Source::Broken`），不伪装成「正在用内置表」——那不是实际发生的事：`load_template` 返回错误、`reload_settings` 整体失败、采集器留着旧阈值不动，而 Alerts 页看上去只会像是坏了。「改用内置」按钮只在存在覆盖文件时出现，删掉它并触发重载。

还有一层**自动降噪**（`state.rs` 的 `banner_damped`）：同一条 episode 在一小时内已经弹过两次横幅，后续横幅暂停，直到时间窗滑过那两次投递。它针对的是**反复越线又回落**的主体——zstats 已经在一条 episode *内部*把提醒拉开（压力规则 30m/1h/2h/4h 递增退避），但每次重新越线都会开一条新 episode，每一条都当作新消息送达。按 episode 计数是刻意的：另一个主体越线是另一件事，照常送达。卡片上有「已降噪」标签并附说明——**一条悄悄不再出现的横幅，和一条不再触发的规则，从外面看是一样的**，所以必须说出来；点「恢复」两层一起清除，且都不跨重启（与旁边会持久化的列表不同）。

同一块编辑区里还有**横幅静音**（1 小时 / 3 小时）。它刻意做在投递层而不是规则层：引擎照常评估、告警照常进列表、config.toml 一个字节不动——被压住的只有横幅这一次打扰。临时改阈值再定时改回的方案被否掉了：那会污染与 CLI 共享的配置，应用中途退出还会把「临时」变成永久。静音按 episode（对象 + 度量）生效、到点自动过期、不跨重启持久化——静音的语义是「现在别吵」，重启后已是新的「现在」。

投递层还有一个**总开关**（界面卡的「通知」，`app.toml` 的 `notifications = false`，只在关闭时写入）：横幅全体不发、其余一切照旧——规则照常评估、列表和按日文件照常记录，日志给每条打 `banner="muted"`。语义与静音不同所以持久化也不同：静音是「现在别吵」，总开关是「我只要记录」，跨重启保持。它排在静音和自动降噪**之前**，关闭期间不向两者的时钟里记入从未发生的打扰。可见性双保险：告警页的监视列表多一行「通知 · 已关闭」，页脚那句节律承诺换成「只记录」的实话——静悄悄不再出现的横幅和失效的规则从外面看一样，这条铁律在总开关上同样成立。

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

第三个排序镜头是**内存增长**（`Spender::memory_growth_bytes`）：同一份文件里每分钟的 footprint，末一个入档分钟减首一个——「今天谁的内存从早到晚涨得最多」，就是 trend 那个小时问题的日尺度版，零新采集。带符号：释放了的读负数、沉到下面；只见过一次的没有增量。和峰值一样只算入档分钟，chip 的 tooltip 把这条诚实性说明带着。

累计量用相邻样本做差再取正数步长，两点都重要：进程只在够格的分钟才进文件，会反复进出，对累计计数器做差在任何空档上都保持精确；而计数器**倒退**意味着 pid 被复用，那一步应该贡献 0 而不是一个荒谬的负数。机器睡眠会留下空隙，不做插值。

**「今天」视图的行下画一条时间带**（`history::Band`，48 个半小时格，只画到「现在」），回答排名回答不了的「几点发生的」：所有行共享 0 点起的同一根轴，两行在同一竖列上同深 = 同时在烧——告警页 14:20 那条越线旁边谁的 CPU 也起来了，从此对得上。它顶掉的那根总量占比条本来就在重复排名次序和头部数字。三个刻意的选择：**格取该时段各分钟的最大值**（「烧过什么」问的是够到过哪，同 trend 缓冲）；**深浅是 ink 的四档量化透明度而不是红色渐变**（accent 只表示越线，格编码的是量级——和 meter 用长度编码量级同类）；**空白 = 那半小时文件里没有这行**，绝不画成 0——记录是条件写入的（越阈值或当分钟前五），没记录 ≠ 空闲，这正是不画折线的原因：折线必须给每个 x 编一个值。7 天 / 30 天视图不画带（多天叠到一根 24 小时轴上是没人经历过的叠加态），保留占比条。**悬停整条带读出钟点**（`history::stretches` + `band_tip`）：连续亮格合并成一段一行的列表，`08:30–11:30 · 记录 42 分钟`，320px 里 ~6px 一格只够目测「三点前后」，这一步补上 HH:MM。**每行必须带记录分钟数**，因为半小时分辨率下「每半小时醒一分钟的机器」和「七小时没停过的机器」画出来一模一样——实测：合盖过夜的笔记本 25 次维护唤醒（DarkWake）点亮了 0 点到早上的每一格，而全程只记录到 48 分钟（共 440 分钟）；范围说「什么时候」，分钟数说「其中有多少真的在」，只给范围就是在默许前一种被读成后一种。计数存在格里（`BandCell { peak, minutes }`）而不是另开一个并行数组：两者描述的是同一格，放一起才不会漂移。多行而非用分隔符连成一句：三个范围排成一行会读成一串连续数字（`widgets::wrap_tooltip_lines`——gpui 把一个 div 的文本当单个 run 排版，`\n` 会被渲染成空格，所以是多个子元素）。三条边界都承重：**四档深浅全算**——tooltip 是对画面的解释，图上看得见的最浅段不出现在自己的读数里是自相矛盾，深浅本来就编码在格上；**精度止步半小时**——带只有这个分辨率，比图更精的 tooltip 是在宣称格子画不出的分钟（分钟数不违反这一条：它数的是文件里的行，不是把格子细分）。触到「现在」的段以当前钟点收口（13:41 就写 13:41——当前格的名义终点还在未来，占位词"now"又是唯一放不上钟面的条目，这是全条里仅有的分钟级数字）；段数超过 8 时留**最长的 8 段**按时序展示、其余折叠成计数——爆发型的一天是一堆单格夹着几段真正的连续区，按时序取前 8 会把预算全花在上午的噪声上、藏掉下午两小时的整块，而按长度选之后，被折叠的从不比展示的长。逐格 tooltip 与轴刻度都考虑过：前者是 576 个 6px 的悬停目标外加扫过连环闪，后者回答不了 HH:MM。

### Swap 的着色线量的是物理内存，不是 swap 自己的分配

Overview 内存卡上的 swap 行，越线才变红。这条线**不能**用 `swap_used / swap_total`——虽然 zstats 现成就报了 `swap_used_percent`，正是这个比值。

原因是 macOS 的 swap 按需增长，不存在固定分区：`/System/Volumes/VM/` 下是一组**等大的 1GB 文件**，free 掉到大约一个文件的量时内核就再开一个。于是稳态是 `used/total ≈ (N-1)/N`（N 为文件数），而这个数**随着机器换页越多、越贴近 100%**：

| swapfile 数 | total | 稳态比例 |
|---|---|---|
| 4 | 4G | 75% |
| 5 | 5G | 80% |
| 7 | 7G | 86% |
| 14 | 14G | 93% |

也就是说，任何一台曾经涨到 5 个以上 swapfile 的 Mac，一条 80% 的线就**永久性地过了**。实测在一台 24GB、内存空闲 55%、内核压力正常的机器上，这个口径仍然画成红色——干净的假阳性。这条线原本是照 Linux 的语义定的：那里 swap 是固定分区，用到 80% 确实离 OOM 不远；macOS 上不够就再造一个，80% 不意味着任何事。

所以分母换成物理内存：**swap 用量超过 RAM 的 50% 才着色**。同一台机器的读数从 81% 变成 24%（中性），而这个数在 16GB 笔记本和 128GB 台式机上含义相同。50 是判断不是推导——一半物理内存的页住在磁盘上；Apple Silicon 在远低于此时就会积极换页，那不代表出问题。

这也顺带解决了一处规则违反：原来的代码自己去除 `swap_used / swap_total`，而那正是 zstats 已经上报的 `swap_used_percent`（「不要重算 zstats 已经报过的数字」）。新口径问的是 zstats 不回答的问题，且只用来选颜色，符合 `HOT_*` 常量的豁免。

### 进程内存显示 footprint，读不到才落回 RSS

进程行和排序用的内存数字是 zstats 0.4 起上报的 `phys_footprint_bytes`——macOS 给进程记账的口径（私有脏页 + 压缩页 + GPU/IOKit 分配），即活动监视器「内存」列。只看 RSS 会把 GPU 重度应用整个看错：Metal 缓冲和被压缩的页 RSS 根本看不见，一个 GUI 应用 RSS 读 80 MB、实际吃 300 MB 是常态。`proc_pid_rusage` 读不了别的用户的进程，所以非特权采集对 root 守护进程报 `None`，这时落回 RSS——那是 zstats 对它们仅有的数字。行展开里两个口径并列标注（`processes.rs` 的 `shown_memory`，有测试守 fallback）。**Apps 页走同一条规则**：`ProcessGroupSnapshot::phys_footprint_bytes`（zstats 按成员逐个求和，读不到的成员用其 RSS 顶上）优先，回落 `memory_bytes`，展开处同样并列两个口径——同一个程序出现在两个页面，必须用同一种量。两边各有一个测试，注释互指，任何一侧单独改动都会被另一侧的测试点名。

两处**有意不动**：Apps 页的分组内存仍是 RSS 之和（zstats 的分组快照没有 footprint 字段，成员又不全在 top-N 里，自己加总就是在推导 zstats 没说过的数字）；`alert-mem` 告警在上游仍按 RSS 均值份额评估，告警卡片引用的是事件自带的数字，两边各自一致。

### 卷卡片显示的是卷名，不是挂载点

标题画 `DiskSnapshot::name`（「Macintosh HD」），不再画 `mount_point`（「/」）。两者
都是 zstats 上报的字段，这纯是展示选择：`/` 正确地命名了那个卷，却只对终端里的人成立，
访达和「关于本机」给普通用户看的一直是卷名。挂载点没有消失，它挪到卡片页脚和文件系统
并排（`/ · apfs`），技术身份仍在，只是退到最小号字。系统没给名字时回落挂载点——
标题绝不能是空的。

### 大文件：查索引，不走树（`bigfiles.rs`）

「大文件」是磁盘清理故事的第一步（现居「磁盘空间」窗口，与目录分析同处一窗）：`mdfind` 查 Spotlight 索引（个人目录范围），毫秒级出结果，不做任何文件系统遍历。默认 ≥500 MB，命中不足 5 个自动降到 ≥100 MB——空列表读作「功能坏了」，几个 100 MB 的文件读作「答案」。行尾大小是重新 stat 的物理占用（`st_blocks`，与卷量表同一口径），不是索引里可能过期的逻辑大小。前置 `mdutil -s` 检查：索引被关闭时如实说明，而不是端出一个假的「没有大文件」。

盲区是接受的边界而不是缺陷：点开头的隐藏目录和 `~/Library` 大部分不进索引，海量小文件的目录也没有单个大文件可找——那些属于第二步的目录分析器（后台走树，已落地，见 docs/disk-analysis.md）。删除按钮走 `confirm.rs` 确认后调 `NSFileManager.trashItemAtURL`——Finder 自己的「移到废纸篓」，清空前可恢复，绝不直接 unlink；这与 `terminate.rs` 同一姿态：面板只发出可拒绝、可撤销的请求。**「新增」标记（`~/.zstats/bigfiles.toml`）**：每次查完把这份清单存下来（0600、临时文件后 rename、带 version，与分析器缓存同一套），下一次查完先与它比对，比出来的行挂一枚「新增」小签。比对完才轮转，所以「新增」永远是「距你上次查找」的意思，caption 里写明那是多久以前——不写的话这个词就是一句没人能核对的断言。

诚实性同源于 Δ 基线那条规则：**在基线里查无此路径，不等于新增**。有两种情况会让一个老文件缺席上一次的清单——上次跑的是 500 MB 那道高门槛（门槛只有在命中太少时才降到 100 MB），或者上次命中超过 20 条、这一条被截掉了。所以 `Baseline::is_new` 只在「上次那一趟本来会列出它」时才认定新增：逻辑大小要过上次的门槛，且当上次被截断时，物理大小要不小于上次列出的最小那条。答不上来就什么都不标，有测试逐条守着这三种情形。

查询结果属于「此刻在问什么」的瞬态状态，**新建磁盘空间窗口时**重置——原先挂在面板隐藏上，而打开这个窗口恰好就会让面板失焦收起，那样一开窗就把自己清空了。

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

- **毛玻璃**：设计稿是实心 `#09090b`，这里保留 vibrancy，观感更通透。**白色壁纸曾把整个暗色面板打穿**（55% wash 放 45% 亮度进来，近白正文压在浅灰玻璃上，有实测截图），根因在 gpui：它给 `Blurred` 垫的 `NSVisualEffectView` 子类钉死 `Selection` 材质、并在每次 `updateLayer` 把 layer 背景剥掉（自称 colorless）——剩下**纯 blur**，材质本该有的亮度钳制衬底根本留不住，`setMaterial: Popover` 设上去也会被剥。所以 `use_popover_material`（main.rs，建窗后首帧调用）在 gpui 的 blur 视图**之上**、Metal 内容层**之下**插一个**原生未子类化的** `NSVisualEffectView`（`.popover` 材质、`.behindWindow`、`.active`，autoresize 跟窗，重入时以 `isMemberOfClass` 认出自己直接返回）：popover 材质的亮度钳制正是系统菜单在任何壁纸上都保持暗底的机制——但对纯白只能压到中灰，所以暗色 wash 从 55% 降到 35% 而不是归零：白底下弱化文字仍可读，彩色壁纸的色相则清楚地透进玻璃（实测 20% 在白底会把说明文字洗掉，HUDWindow 材质在新系统上反而更透，都试过）。曾经试过反方向——把卡片涂到 94% 实心——白壁纸是修好了，玻璃也没了，黑底下卡片还和框架撞色；已回退，卡片仍是玻璃上的微提亮（暗 `0xffffff12`、浅 `0xfffffff2`）。那次弯路留下的一件对的东西保住了：卡片描边（`widgets::outline`，零布局 inset shadow，`theme::border()` 选墨色）从浅色专属改为两个主题都画——不欠壁纸任何东西的分隔。
- **导航**：设计稿是 4×2 缩写文字（Over / Sens / Conf）。320px 塞不下全名，缩写也不像 macOS，所以改成单行图标 + tooltip。
- **字体**：系统字体做 UI，JetBrains Mono 做指标数字（等宽数位，跳动时不会左右抖）。设计指定的 Archivo 未采用。
- **Config tab 可写**。开关和间隔走 `apply_add` 后重建 `Monitor`（速率基线会丢，下一次采样的速率是 —）；告警基值走 `reload_settings()`，和 Alerts 卡片同一条路径。
- **Apps 展开列出整棵树的成员**。`ProcessGroupSnapshot` 只有汇总；成员靠对全表走 parent 指针还原。常驻 tick 只物化 `max-processes`，所以点击展开时若 live 表不够，会另采一次不带 CPU 基线的全表（不重建 Monitor、不拉长 `max-processes`）。已在 top-N 里的 pid 继续画 tick 上的 CPU，其余显示 `—`。
- **Apps / Overview 的行标题是「脸」，不是树的 key**（`trend::tree_face`）。根是裸可执行文件（无 bundle、`display_name` 为 `None`：login、sshd-session、tmux、守护进程）的树，按**内核的进程组**命名：job-control shell 给每个敲下的命令一个新 pgid，它 fork 的一切都继承，于是 `login → zsh → cargo → rustc×10` 是 `{login}`、`{zsh}`、`{cargo, rustc…}` 三个组，按组合计 CPU、取最大组（须 ≥ 树的 1/3）的组长——`cargo`：你敲的那个、退出会落到的那个，而且整个构建期间稳定，不论此刻一个还是十个 rustc、尾部是不是 `rustc → cc → ld`。曾经的做法是一张 shell 名单 + 找单个最热进程，两处都错：名单永远不全（nushell、sshd-session、sudo、caffeinate），而并行构建里没有一个 rustc 过得了 1/3，树最热的时候脸反而停在 `login`；也考虑过「沿闲置的单子节点往下钻」，但它在构建头尾抖（单个 rustc 时是 `rustc`，链接时是 `ld`）。组长就是 job，没有名单。`sudo make` 的脸是 `sudo`——组长，诚实且稳定。**门禁是 bundle 而不是名字**：根有 bundle 的是应用，哪怕某个渲染进程自成一组烧满整棵树，行仍然是那个应用。但应用的树被一个**裸 job** 烧着是另一回事——这台机器上的 `login` 都挂在 Zed / Ghostty 下面，终端里的构建曾显示为 "Zed 800%"，看着像 Zed 失控。于是 `tree_face` 返回 `Face { title, job }`：根在 bundle 里时标题不变，若最热 job 的组长**不在根的那个 bundle 里**（所以不是这个应用自己的 helper）、也不是根本身，它作为弱化的尾巴跟在后面——`Zed · cargo`，谁的树、谁在烧，一行说清；Xcode 的 `make` 自己有 bundle，对 Zed 的树仍是外人，照样是尾巴。**"在不在 bundle 里"读的是 argv[0]（`bundle_of`，`ProcessSnapshot::cmd` 的首段），不是 `display_name`**：zstats 对裸可执行文件和"bundle 名恰好等于进程名"（Google Chrome 和它的每个 helper）都给 `None`，那个字段分不开 `login` 和 Chrome——第一版用它做门禁，测试里根 pid 取了 1 又被 `belongs_to` 当 launchd 截断，两个错误互相掩盖着通过了，这是把测试根 pid 改成 100 之后才暴露的；`widgets::truncating_name_tailed` 把标题和尾巴画成两段，标题截断时尾巴仍在（尾巴是新闻，标题读者本来就认识）。尾巴的条件和脸一样：≥ 树的 1/3。过滤按 `Face::text()` 匹配，所以搜 `cargo` 能命中 Zed 那行。pgid 不在 zstats 的快照里，由 `procscan::process_groups`（同一个 `sysctl(KERN_PROC_ALL)`，`e_pgid` 紧挨 `e_ppid`）和成员表在同一次后台采集里一起取、一起落到 `MemberTable::Ready`，所以脸和展开行描述的是同一瞬间；表落地前树保持自己的名字。有一个针对偏移量的自检测试：本进程在表里的 pgid 必须等于 `getpgid(0)`。
- 设计稿里的假菜单栏和右下角说明文字不实现 —— 那是设计稿自己的展示环境。

## 日志（`logger.rs`）

zedis 的 logger 原样适配：stdout + `~/.zstats/logs/zstats-app.log.<日期>` 每日滚动（**文件名日期是 UTC**——tracing-appender 写死 now_utc，UTC+8 下本地 0~8 点的行落在前一天名字的文件里、早上 8 点换文件；行内时间戳是本地的，按内容时间查而不是按文件名）、90 天清理（**仅启动时执行**，按 mtime 判旧；常驻数月不重启就不清，KB/天的量级无所谓）、`RUST_LOG` 控级（默认 INFO）、non-blocking writer 的 guard 由 `main` 持有到进程结束。目录与 CLI 共享，所以**文件名携带写入者**——和 cleanhints 文件名携带平台是同一条理由。

它落盘的核心是**告警类轨迹**：托盘内存恢复计时的两次转换（起步 / 清零，后者带「已跑多久」——刻意打 INFO 而不是 DEBUG：它要回答的「episode 看着已经过去了，菜单栏怎么还是内存」是对着**安装版**问的，而安装版不开 DEBUG；这个问题被问过一次，当时没有任何记录可查，只能靠推断压力值抖动来回答。只记转换，条件持续成立的每个 tick 不记，否则真正有信息的那行会被淹掉）、每条上报的 `AlertEvent` 连同横幅裁决（delivered / snoozed / auto-quieted——一条悄悄没出现的横幅和一条不再触发的规则从外面看一样，卡片上的 pill 答当下，日志答隔天的追问）、每次 quit/SIGTERM 请求、每次移入废纸篓（都在唯一投递点打一行，两个确认门的调用方共享）。此前 app 有约 30 处裸 `eprintln!` 且没装 subscriber——LaunchServices 下 stderr 无人可见，而且 **zstats 库自己发的 tracing 事件（调度器、告警引擎）一直在被丢弃**；现在全部收编，失败路径一律 `tracing::warn!/error!`。

## 系统通知

告警触发时发原生横幅，macOS 走 `NSUserNotification`。点击横幅会打开 popover 并切到 Alerts 页。系统横幅的外观不能自定义。

**macOS 上投递走 `UNUserNotificationCenter`：fire-and-forget + 常驻 delegate。** `addNotificationRequest:` 异步返回；点击由启动时装好的 delegate 在主 run loop 上接收（gpui 本来就在泵它），任何一条横幅的点击都等于「打开 Alerts 页」，所以 delegate 无需携带 per-notification 状态；`willPresent` 回调放行 Banner|List|Sound，让面板打开（应用最前）时横幅照常出现——菜单栏应用的「最前」正是用户在看告警的时刻。fire-and-forget 是目的而不是省事：更早的 notify-rust `wait_for_action` 版本要等用户*处理掉*横幅才返回，一条躺着没人理的横幅能把投递线程停到天亮，第 17 条起静默丢弃。用户的注意力不是可以串行化的资源。

**前任 `NSUserNotification` 在 macOS 26 上死于无声**，这是整次迁移的起因：当年选它是因为裸 `cargo run` 没有 bundle、UN 会直接抛异常，而它到 26.5 变成了彻底的 no-op——`deliverNotification:` 正常返回、什么都不显示、系统连通知设置条目都不建（实测，`osascript` 的横幅作为对照正常弹出）。假装投递的 API 比拒绝投递的更糟，所以它没有作为回退保留：裸 `cargo run` 现在诚实地没有横幅，启动时日志说一次。

**授权与签名身份，两道都要过。** UN 走标准授权：首次请求弹系统自己的「允许通知？」对话框，之后按用户的记录静默放行；拒绝只记日志、绝不重问——用户已经答过系统的问题了。**签名身份是本地构建的暗坑**：arm64 链接器自动打的 ad-hoc 签名带随机 Identifier（`zstats-<hex>`），UN 对它直接拒绝授权（实测 `granted=false`）；`make bundle` 因此收尾 `codesign -s - --force --deep` 重签——codesign 会从 Info.plist 取正规 bundle id 作 Identifier，重签后授权即通过（同样实测）。发布管线的真实签名会覆盖这次 ad-hoc。旧的 `notify_rust::set_application` swizzle 和 `BUNDLE_ID` 常量随 `NSUserNotification` 一起退役——UN 按进程的真实 bundle 归属，无需自报身份；留下的跨文件测试改为守「Cargo.toml 还声明着 identifier」本身。

同一个 id 若有多份 .app 注册（比如 Downloads 里留着旧包），归属会摇摆，横幅可能静默丢失：`lsregister -u` 掉多余的那份即可。实测这台机器上曾同时注册六份（正主之外：`make bundle` 的构建产物——Spotlight 会自动收录它索引到的任何 `.app`——和四条已卸载 DMG 的残留），所以 **`make bundle` 现在收尾自动注销自己的产物**，每次构建都做（Spotlight 之后可能悄悄加回来）。`make dev` 是裸二进制，本来就不产生注册。

**应用内更新流程自己就会制造第二个认领者**：updater 下载 DMG 后交给 `open` 挂载，用户拖完 /Applications，卷宗却没人弹出——镜像里那份 zstats.app 一直挂在 `/Volumes/zstats Installer`，注册鲜活，横幅从此静默丢失（v0.1.13 发布版实测：系统设置里授权齐全、日志 `banner="delivered"`、屏幕上什么都没有；弹出镜像横幅立刻回来）。**所以新版本启动时替上一次更新收尾**：`updater::sweep_installer_mounts`（后台执行器上跑一次）把名字以「zstats Installer」开头、bundle id 是我们、版本不比运行中*新*的卷宗 `hdiutil detach` 掉，再 `lsregister -u` 掉卸载后仍残留的注册记录（实测卸载不清记录）。三道闸都承重：id 不符的卷宗不是我们的、正在从镜像里运行就不能抽走地面、版本更新意味着「下载了还没拖」的进行中安装，拖装窗口得留着。detach 忙则记 warn 放过，下次启动重试。

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

  失焦自动收起（仅 release）走 `orderOut`，重新唤起走 `setFrameOrigin` + `makeKeyAndOrderFront`。`was_active` 标志用来跳过窗口刚创建、还没首次激活时的那一次失活回调，否则窗口会一闪即逝。这两次 AppKit 调用都在 `after_app_borrow` 里（主队列 `dispatch_async`）：托盘点击落在 `cx.update` 里，握着 gpui 的 App `RefCell`，而 `setFrameOrigin` / `orderOut` 会**同步**打 `windowDidMove`，gpui 再 `handle.update` → `try_borrow_mut` 同一格，就是每点一次托盘那条 `ERROR gpui::window: RefCell already borrowed`。只把 AppKit 挪出 `handle.update` 不够，`cx.defer` 也不够（它还在这次 update 的 `flush_effects` 里）。debug 以前没托盘、窗口一直开着，这条路径根本走不到。
- **重绘要按可见性门控**。窗口是移出屏幕而不是销毁的，gpui 并不知道它看不见，会老老实实继续渲染一个没人能看到的面板——实测空闲 CPU 因此从 0.6% 涨到 2.0%。`CollectorPace::is_visible()` 同时管采样节奏和「这次 tick 要不要重绘」。
- **跨 Space**。普通窗口属于它被创建时的那个桌面，从别的桌面唤起会让 macOS 切回去——对一个从菜单栏召唤出来的东西来说很突兀。`NSWindowCollectionBehavior::CanJoinAllSpaces | FullScreenAuxiliary`（后者保证在全屏应用之上唤起时不会先退出全屏）。gpui 的 `WindowKind::PopUp` 自带这个行为，但那是 nonactivating panel，拿不到键盘焦点。
- **托盘点击的 toggle**：点图标会先让窗口失焦（触发自动收起），点击事件随后才到。所以 `TOGGLE_GRACE`（300ms）内如果刚发生过自动收起，这次点击就不再开窗 —— 于是表现为 toggle。`took_recent_auto_hide` 会取走标记，只生效一次。
- **托盘图标**：`assets/icons/cpu.svg` 与 `memory-stick.svg` 在启动时由 `resvg` 光栅化成两张位图缓存在 `TrayHandle` 里。用主体（CPU die / 内存条）而不是趋势箭头：箭头对数据下了断言（「数字在涨」），而主体本身不下断言。

  **图标会换脸，但换脸不是面板自己的判断。** `app.toml` 的 `tray` 偏好四档（`tray.rs` 的 `face_for`）：`cpu` / `memory` 钉死一项；`both` 放**两个** `NSStatusItem`——AppKit 把新建的状态项插在已有项的**左边**（`tray-icon` 不设 `autosaveName`，位置不会被记住），所以后建的 `second` 在左、戴 CPU，常驻的 `primary` 戴内存，左→右就是 CPU · 内存（选择器那档叫「两者」——原本写作「CPU + 内存」，四个 chip 一行放不下 320px）；两个项共用同一套菜单和点击线程，点到哪个就以哪个的 rect 为锚点，切档时建或 drop 第二个项（drop 即从菜单栏移除），启动时若已是 both 则两个一起建，不让第二个晚几秒才长出来；缺省的 Auto 平时是 CPU，只有一个触发条件——`state.rs` 的 `memory_needs_attention()`：本次会话报告过、尚未关掉、且通过 `turns_the_face` 的一条内存类 episode（Memory / AppMemory / Pressure）。**`turns_the_face` 只挡一样东西：warning 档的内核压力。**理由是这一档在这个平台上的含义——内存大户机器的稳态就是 warning（zstats 在压力规则的注释里原话如此，也正因如此让这一档等 5 倍时长才上报），而一张半天都在显示内存的脸已经不是信号了；卡片和横幅照常承载 warning，只有菜单栏等 critical。判据用的是 zstats 自己的 `severity()`，绝不读原始 `pressure_level`；按**种类**而不是笼统按严重度，是因为进程/应用内存 episode 在上游天生就是 Warning（只有压力 ≥ 4 和 CPU runaway 是 Critical），笼统一刀会把内存脸最初的职责——点名正在吃掉机器的那个进程或那棵树——整个删掉。warning 升级到 critical 时，那条上报会立刻翻脸（`record_alert` 保留最新事件）；之后即使回落到 warning 也保持，因为那仍是同一条尚未恢复的 critical episode，退出方式和其它 episode 一样。**切过去**完全是 zstats 规则引擎已经做出的裁决；**切回来**有两条路：关掉卡片立刻回，或者该 episode 按**它自己记录的那条线**（`AlertDetail` 里的 `threshold_bytes` / 压力事件对应 `pressure_level > 1`）连续恢复满 `TRAY_RECOVER`（5 分钟，镜像 zstats 结束压力 episode 的 `PRESSURE_REARM`）后自动回。恢复判定（`memory_event_holds`）是这个面板里**唯一一处拿实时数字对阈值**的地方，边界收得很紧：用的是事件自己带的线而不是新阈值，读的是 zstats 自己的字段，结论只落在托盘的脸上——不开合 episode、不动列表、不发横幅，卡片和页签着色照旧到关闭或跨日为止。之所以需要它：引擎对进程/应用内存 episode 只有 30 分钟一次跟进、之后沉默，「已经恢复」这件事引擎从来不说，没有这个判定，Auto 的脸一旦换成内存就只能等人手动关卡片。subject 掉出 top-N 表按「已恢复」读——掉出内存榜本身就是不再是大户；该 tick 说不了话（无进程表、无压力等级）则时钟原地不动。昨天恢复的 episode 不算——托盘说的是现在。

  **有意不读最新样本上的原始 `pressure_level`。** 它会抖——zstats 自己的注释记录了一个连续的压力条件在 5.5 小时里产出四次「新」warning——所以 zstats 的压力规则要求 warning 持续 5 分钟（critical 1 分钟）才报，回到 normal 也要保持 5 分钟才算 episode 结束。托盘如果绕开这层直接读原始值，就是把 zstats 刚去掉的抖动重新漏到菜单栏上，用一个更粗的裁决覆盖一个更细的。结果是脸和横幅同一时刻切换、从不早于横幅，也就不需要自己的最小切换周期：切过去的时机由 zstats 的持续要求决定，切回来的时机由 5 分钟恢复保持（或用户关卡片）决定——恢复判定也从不读原始压力值抖一下就动：一次跌回把 `recovered_since` 记下，五分钟内任何一个样本重新过线就清零重来，两端都不是 tick 级的量。CPU 是静息脸，所以一条 CPU 告警什么也不改（它关心的数字本来就在那里）；两边都有事时内存赢，因为 macOS 会把内存一路升级（压缩、swap、jetsam），而 CPU 忙只是忙。脸和旁边的数字永远一致：CPU 脸配 `cpu.usage_percent`（整数百分比），内存脸配 `memory.available_bytes`（`format::gb_short`，`8.1G`），都是 zstats 的字段。内存**有意不用 used%**：macOS 的缓存会填满所有空闲内存，健康的机器 used% 也在六十几，脸切过去时旁边一个 "62%" 说明不了为什么切；随机器吃紧真正下降的是可用量，也是概览 hero 和总量配对的那个数（同一个 `format::gb` 取整）。裸露的 `8.1G` 靠状态项的 tooltip 说明是什么（`zstats · 可用 8.1 GB，共 24 GB`），tooltip 跟着标题的变化门一起设。同步点在 `metrics.rs` 每次 `ingest` **之后**（这一 tick 合并进去的 episode 要在同一 tick 生效）以及选择器改动时。

  换图**不能**用 `TrayIcon::set_icon`：macOS 实现里它把 template 写死为 `false`，而我们的位图已被抹成纯黑只靠 alpha——换进去在深色菜单栏上就是一块黑。要用 `set_icon_with_as_template(icon, true)`，这正是 crate 为此准备的入口。一条 live `AlertEvent` 在 Alerts 页不在眼前时（面板藏着，或开着但停在别的 tab），主状态项换一张多了一颗角点的 **同一张 template**（多出来的是 alpha，跟字形同一套菜单栏墨色，深色栏上就是白点）；切到 Alerts，或窗口打开时已经停在 Alerts，点灭。不关 template，不改颜色。这是「你还没打开过那份列表」的显示，不是第二条阈值——引擎已经判了；从文件恢复的卡片（`live = false`）不算新的；已经看过的 episode 再跟进一次，人走开之后点会再亮，和横幅同一条节奏。Auto / Both 当时可能戴着内存条，所以两张脸都备了带点的 template；Both 的第二项不加，一条新闻不必两个点。

  `tray-icon` **不支持 SVG**，只接受原始 RGBA（内部再编码成 PNG 交给 `NSImage`）。两个要点：macOS 会把图标缩放到 **18pt 高**，所以按 2x（36px）出图才不会在 Retina 上发虚；注册为 template image 后**只有 alpha 通道有效**，颜色由系统按明暗模式重新上色，因此渲染后把 RGB 抹成黑色。glyph 只占画布 78%——lucide 画到 24×24 viewBox 的边缘，1.0 的话图标会有整整 18pt 高，压过旁边约 12pt 的标题文字，系统图标都是自带留白的。另外 lucide 的 `stroke="currentColor"` 是 CSS 上下文关键字，usvg 解析不了，加载前需替换成具体颜色。有单测校验光栅化结果的覆盖率——解析失败会得到一张全透明位图，不报任何错，只表现为图标消失。
- **托盘交互**：左键单击 toggle 窗口，右键弹出菜单（Show Window / Quit）。实现上是 `with_menu_on_left_click(false)` 关掉左键弹菜单，再监听 `TrayIconEvent::Click`；`MenuEvent` 和 `TrayIconEvent` 各用一个阻塞线程，汇入同一个 `smol::channel`。托盘标题显示当前那张脸的百分比（整机 CPU% 或内存 used%），取整到个位（菜单栏很挤，小数会让它每次采样都抖），并且标题和图标都是「和上次相同就不重设」（设标题会让菜单栏重新布局，换图标还要重建 `NSImage`）。
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

**面板之外还有两个普通窗口**：设置（`open_settings_window`）与磁盘空间
（`open_storage_window`，大文件 + 目录分析）。两者**开在同一个尺寸**
（`AUX_WINDOW_SIZE`，507×620，可缩放）——两个只差几十像素的尺寸读起来像疏忽，
不像决定；最小尺寸也共用一个，且必须小于它，否则窗口一开就被下限撑宽。它们和面板是两种东西——有真正的
标题栏与红绿灯、不参与显隐复用、关掉就是关掉（存着的 handle 下次 update 失败，再点即
新建）。会独立成窗都是同一个理由：**popover 一失焦就自动收起**，而在里面做的事经不起
这个——改配置是一段会话，走查是分钟级的一次性查询（而且刻意设计成收起面板也继续跑）。
320px 也装不下三张排名表：每条路径都成了省略号。两个窗口都用 `key_context` 各自绑
Esc/⌘W（`CloseWindow` 一个 action、两个上下文），键不会漏进面板——面板没有 Esc、
靠失焦收起是它自己的形态。窗口视图只持有 focus handle 与滚动位置，数据仍读同一个全局
store：磁盘窗口的重绘订阅**不走** `CollectorPace::is_visible()` 门控，那道门是给「移出
屏幕但还活着」的面板准备的，普通窗口要么开着可见、要么已经不在。

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

### 非 macOS 平台编译不过

`main.rs` 把 `procscan` / `terminate` / `window_ext` 声明为 `#[cfg(target_os = "macos")]`，但有**六处无条件 import** 它们——`metrics.rs`、`state.rs`、`watch.rs`、`views/processes.rs`、`views/alerts.rs`、`placement.rs`。在 Windows / Linux 上这是六个 `E0432 unresolved import`。此前本节写的是「能编译但未验证」，那是错的：**从未有任何一次非 macOS 构建成功过**。

`main.rs` 顶部有一条 `#[cfg(not(target_os = "macos"))] compile_error!`，把这件事在最早的位置说清楚——否则另一个目标上得到的是六个散落的 unresolved import，看不出原因。**门禁刻意没有铺到那六个文件里**：一个能编译但没有托盘、没有窗口显隐、没有异常进程扫描、废纸篓按钮永久失败的构建，比一个直接停下来的构建更糟。铺门禁属于移植工作本身——那时每个被门禁的功能要么有真实现、要么有诚实的空态，而不是为一次尚未决定的移植预留脚手架。

因此 `main.rs` 与 `metrics.rs` 里保留的那批 `#[cfg(not(target_os = "macos"))]` 分支（窗口用销毁代替隐藏、`cx.displays()` 查屏、无毛玻璃）连编译都没经历过，更谈不上跑。保留它们是为了不堵死后续移植，但不应当理解为「支持」，也不应当理解为「现成的」。

移植还有三条硬约束值得先知道：Linux 上 `tray-icon` **根本不发射点击事件**（其自身文档写明），所以拿不到锚定弹窗所需的图标屏幕矩形；Wayland 不允许客户端设置绝对窗口位置；`gpui_linux` 的 `activate` / `hide` 是静默 no-op。三条叠加意味着「点菜单栏图标弹出锚定面板」这个形态在 Linux 上无法还原。
