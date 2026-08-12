# zstats.app

macOS 菜单栏系统监控面板。界面实现自 Claude Design 项目 `Stats Popover v3 shadcn`，指标由 [zstats](https://crates.io/crates/zstats) 嵌入式采集。基于 [gpui](https://github.com/zed-industries/zed) + [gpui-component](https://github.com/longbridge/gpui-component)。

## 指标采集

`src/metrics.rs` 起一个**常驻后台线程**跑 `zstats::Monitor::tick()`，1s 一次，经 `smol::channel` 把 `Tick` 交给主线程写进全局 store。

- **为什么常驻**：`Monitor` 内部为 disk / net / 每进程 IO 累积「上一次采样」的基线，重建就丢。窗口是随开随关的 popover，采集器跟着窗口走的话每次开窗速率都显示 `—`。
- **`tick()` 每次都完整采集**，但各子系统的节流在 `LocalCollector` 内部按 config.toml 的 interval 走，所以 1s 的调用频率不等于每秒遍历进程表。
- **首次采样的速率类指标必然是 `None`**（需要前一个样本做差），UI 统一显示 `—`。这是 zstats 的契约，不是故障。
- 依赖只开 `frontend` feature：拿到告警规则引擎、滚动均值和 settings 模型，且**不引入 tokio**。

### 与 zstats CLI 共用 `~/.zstats`

配置目录用 `zstats::settings::default_dir()`，与 zstats CLI 共享同一份 config.toml、告警阈值和历史记录。

**前提**：一个系统里只能有一个采集器。如果同时跑 `zstats serve` 守护进程，会双重采集 —— 重复通知、重复写历史。本应用**不做** `is_running()` 检测（那需要开 `client` feature 并引入 tokio），请自行确保不并存。

## 界面

8 个视图：Overview / Processes / Apps / Disk / Network / Sensors / Alerts / Config，`src/views/` 一个文件一个。导航是单行图标 tab（Control Center / Stats 的做法），全名走 tooltip。设计 token 在 `src/theme.rs`，卡片用半透明 grouped fill 叠在原生 vibrancy 上，而不是 shadcn 实心描边。

贯穿全部视图的一条规则：**进度条、柱状图和数字默认中性色（`ink`），只有越过阈值才变品牌红（`accent`）**，由 `theme::fill_for()` / `theme::text_for()` 固化。

与设计稿有意的偏差：

- **毛玻璃**：设计稿是实心 `#09090b`，这里保留 vibrancy，观感更通透。卡片和 tab 槽用半透明 fill，让 material 透出来。
- **导航**：设计稿是 4×2 缩写文字（Over / Sens / Conf）。285px 塞不下 8 个全名，缩写也不像 macOS，所以改成单行图标 + tooltip。
- **字体**：系统字体替代设计指定的 Archivo。
- **Config tab 只读**。`reload_settings()` 只对 `[alerts]` 生效，而 `[collector]` 开关必须重建 `Monitor`（速率基线会丢），所以不做成可写。改配置走 zstats CLI 或直接编辑 config.toml。
- **Apps 展开显示聚合详情而非成员进程列表**。`ProcessGroupSnapshot` 只给出整棵树的汇总，不返回成员清单。
- 设计稿里的假菜单栏和右下角说明文字不实现 —— 那是设计稿自己的展示环境。

## 开发

```bash
make dev      # cargo run
make debug    # RUST_LOG=debug cargo run
make check    # 快速类型检查
make lint     # clippy --deny=warnings
make release  # cargo build --release
```

## 托盘 popover 模型

应用是菜单栏 popover 形态：**启动时没有窗口**，只有托盘图标；窗口按需创建、失焦即销毁。

- `cx.set_quit_mode(QuitMode::Explicit)` —— gpui 默认在非 macOS 平台关掉最后一个窗口就退出进程（`QuitMode::Default`），零窗口的启动状态会直接退出，所以必须显式改成"只有 `cx.quit()` 才退出"。
- **失焦自动收起**（仅 release）：`Context::observe_window_activation` 里发现窗口失活就 `remove_window()`。gpui 没有隐藏单个窗口的 API，收起只能是真关闭。`was_active` 标志用来跳过窗口刚创建、还没首次激活时的那一次失活回调，否则窗口会一闪即逝。`cargo run` / debug 构建失焦不关，方便对着 IDE 看。
- **托盘点击的 toggle**：点图标会先让窗口失焦（触发自动收起），点击事件随后才到。所以 `TOGGLE_GRACE`（300ms）内如果刚发生过自动收起，这次点击就不再开窗 —— 于是表现为 toggle。`took_recent_auto_hide` 会取走标记，只生效一次。
- **托盘交互**：左键单击直接显示窗口，右键弹出菜单（Show Window / Quit）。实现上是 `with_menu_on_left_click(false)` 关掉左键弹菜单，再监听 `TrayIconEvent::Click`；`MenuEvent` 和 `TrayIconEvent` 各用一个阻塞线程，汇入同一个 `smol::channel`。
- **无标题栏**：macOS 上 `WindowOptions.titlebar` 留 `None`。gpui 此时用 `Titled | FullSizeContentView` 的 style mask，且**不含** `Closable`/`Miniaturizable`/`Resizable`（所以没有 traffic light、也不可缩放），同时照样会设 `titlebarAppearsTransparent` + `titleHidden`（`gpui_macos/src/window.rs:815,977`）。

  没用 `WindowKind::PopUp`：那个在 macOS 上是 `NonactivatingPanel` + `NSPopUpWindowLevel`，窗口拿不到正常的激活焦点，会破坏失焦自动收起。留 `None` 得到的仍是普通 titled window，系统圆角和阴影都在。

  注意 `with_app_identity()` 里补 titlebar 的分支必须跳过 macOS，否则会把 traffic light 又装回去。

- **退出按钮**：面板 footer 右侧。accessory app 没有应用菜单栏、没有 Dock 图标可右键、窗口也没有关闭按钮，所以退出必须有个看得见的入口（托盘右键菜单的 Quit 仍在）。
- **窗口定位**：`TrayIconEvent::Click` 带的图标矩形是物理像素，换算成逻辑坐标后，窗口以图标为中心水平居中、下方留 6px，再夹进该显示器的 `visible_bounds()`（已排除菜单栏和 Dock）—— 只有居中会越界时才贴边。纯几何部分是 `anchored_origin()`，单元测试覆盖了居中 / 贴左 / 贴右 / 窗口超高四种情况。

  gpui **无法移动已存在的窗口**（`PlatformWindow` 只有 `resize`，没有 set position），所以位置只能在 `open_window` 时定。`show_main_window` 发现窗口不在目标位置时会 `remove_window` 再重建（判断带 1px 容差，避免合成器亚像素抖动导致每次都重建）。
- **毛玻璃**：`WindowOptions::window_background = WindowBackgroundAppearance::Blurred`，gpui 在 macOS 上用 `NSVisualEffectView` 实现，是系统原生 vibrancy。仅 macOS 启用：其他平台 `Blurred` 文档标注"not always supported"，退化后是纯透明，会直接看到桌面。

  想看到模糊，上面盖的每一层都必须让路，缺一层就是"完全没有透明效果"：

  1. `Root::render` 会铺一层不透明的 `theme.tokens.background`（`gpui-component/crates/ui/src/root.rs:566`）。它的 `refine_style` 排在那句 `bg` 之后，所以 `root.bg(transparent_black())` 能覆盖掉。
  2. 根视图自己的着色层要**很淡**。`BACKGROUND_OPACITY = 0.18` —— 主题背景是 `l ≈ 0.04` 的近黑色，叠在本来就暗的 material 上，稍微浓一点（试过 0.55）就把模糊压成纯黑，看起来和不透明一模一样。AppKit 自己的暗色面板也是只上很淡的一层色，主要靠 material。
  3. material：gpui 硬编码 `NSVisualEffectMaterial::Selection`（选中高亮用的），`use_popover_material()` 在窗口首帧把它改成 `Popover`，也就是 AppKit 给菜单栏面板用的那个。
- **无 Dock 图标**（`src/dock.rs`）：有**两个**独立来源会把图标放进 Dock，各治一个，少一个就会闪。

  1. **LaunchServices 在进程启动时注册** → Info.plist 的 `LSUIElement = true`，由 `make bundle` 用 PlistBuddy 写入（cargo-bundle 没有这个字段）。裸二进制没有 Info.plist，所以 `cargo run` 这一段治不了。
  2. **gpui 自己调 `setActivationPolicy(Regular)`** → `suppress_regular_policy()` 在 `main()` 开头 swizzle `-[NSApplication setActivationPolicy:]`，把 `Regular` 那次调用吞掉，其余原样转发。

  为什么必须 swizzle 而不能"事后改回来"：gpui 那次调用是发给 Dock 的 IPC，Dock 收到就开始播图标动画，我们在 `run` 回调里微秒后改回 `Accessory` 也拦不住已经起来的动画。实测加了 `LSUIElement` 的 `.app` 依然会闪，就是这个原因。

  `hide_dock_icon()` 仍然保留：`cargo run` 的二进制是 LaunchServices 直接设成 `Regular` 的，没走 `setActivationPolicy:`，swizzle 碰不到，只能在回调里改。所以开发时依然会闪约 50ms（进程启动到回调执行），打包后不闪。

  验证：`lsappinfo info -only ApplicationType <asn>` 返回 `UIElement`。

  swizzle 是运行时改别人的行为，风险要认：gpui 若改用其他 API 设 policy，这段会静默失效（表现是图标又开始闪，不会崩）。**上游给 `Application` 加一个 activation policy 选项就能删掉它** —— 目前 zed 仓库没有相关 issue。

  代价：accessory app 没有应用菜单栏，`cx.set_menus` 的菜单不再显示。退出只剩托盘菜单的 Quit，或窗口有焦点时的 ⌘Q（keymap 绑定仍有效）。`set_menus` 保留着，改回 `Regular` 就会恢复。
- **scale factor**：换算需要菜单栏所在屏幕的 scale factor，而 gpui 的 `PlatformDisplay` 不暴露它。macOS 走 AppKit 直接读 `NSScreen::screens()[0].backingScaleFactor()`（`screens()[0]` 恒为含菜单栏那块屏，`mainScreen` 则跟着 key window 走）；其他平台回退到主窗口每帧镜像进 `ZStatsAppState` 的值。

因为窗口随时会被销毁重建，**任何需要跨"关闭 → 重开"存活的状态都必须放进 `src/state.rs` 的 `ZStatsAppState`**，而不是根视图里。窗口尺寸就是按这个方式记住的。

## 已知问题 / TODO

### Linux 暂不支持托盘

`src/tray.rs` 整体被 `#[cfg(not(target_os = "linux"))]` 关掉，`tray-icon` 依赖也只声明在 `[target.'cfg(not(target_os = "linux"))'.dependencies]` 下。

原因：`tray-icon` 在 Linux 上通过 libappindicator / GTK 实现，菜单事件依赖一个 GTK main loop，而 gpui 在 Linux 上跑的是自己的 X11 / Wayland 事件循环，两者无法直接共存。zedis 也是同样的处理方式。

后续可选方案（未验证）：

- 在独立线程里跑 GTK main loop，只用来驱动托盘，通过 channel 与 gpui 主线程通信；
- 改用 StatusNotifierItem（KDE/GNOME 走 D-Bus 协议）的纯 Rust 实现，绕开 GTK；
- 或在 Linux 上放弃托盘，退化成普通窗口应用（关窗即退出，即 `QuitMode::LastWindowClosed`）。

### 非 macOS 平台多显示器不同 DPI 时托盘定位会偏

macOS 已经通过 AppKit 读到菜单栏那块屏的真实 scale factor，不受影响。其他平台还在用主窗口的 `window.scale_factor()` 兜底，窗口在 1x 屏、托盘在 2x 屏（或反过来）时换算会用错倍率。等真正支持 Windows / Linux 托盘时再补对应的平台实现。

### 托盘图标是占位图形

`src/tray.rs` 的 `placeholder_icon()` 用代码画了一个 22×22 的柱状图，注册为 macOS template image（只用 alpha 通道，系统按明暗模式自动反色）。有真实图标后换成 `include_bytes!("../assets/icon.png")` + `image::load_from_memory`，并加上 `image` 依赖。
