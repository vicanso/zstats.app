# 面板侧的跨平台调整方案

这份文档回答一个假设性问题：**假如 zstats 按 [`zstats/docs/cross-platform.md`](https://github.com/vicanso/zstats) 那七条做完了优化，面板这边要跟着改什么。**

它不是移植计划。面板自身的移植可行性是另一件事，结论写在 `design.md` 的「非 macOS 平台编译不过」一节：Linux 上托盘不发点击事件、Wayland 不允许客户端定位窗口、`gpui_linux` 的 `activate`/`hide` 是静默 no-op，三条叠加意味着「点菜单栏图标弹出锚定面板」这个形态在 Linux 上无法还原；Windows 外壳可行但数据层要等 zstats 补课。**本文只处理数据层那一半**——即使面板永远只跑在 macOS 上，其中一部分改动（第一节）也是净收益。

贯穿全文的硬约束：**macOS 上的行为不得倒退。** 每条方案都按「capabilities 在 macOS 上三项全真、
新写的『不支持』分支在 macOS 上不可达」设计。唯一有意改变 macOS 观感的是第一节里进程展开行的那句空值说明——
它从沉默变成了说出原因，属于净增而非改动。

---

## 一、停止猜测，开始读取能力（**已落地**，zstats 0.5.2）

对应 zstats 的第 6 条（capabilities 描述符）。0.5.2 在 `SystemSnapshot::capabilities` 上给出
`memory_footprint` / `memory_pressure` / `cpu_perf_levels`，随快照传递（接在守护进程上的前端读到的
是**守护进程的**能力，不是自己构建的）。面板已改为读取它，见下表的「现状」列。

动手时发现原计划里有两处**并不存在**，如实记下免得后人再找：告警页的「已武装」行从来没有列出
`Pressure`（它只列 cpu / 内存 / 应用 CPU / 应用内存 / 磁盘），阈值编辑器的压力档位也只出现在
压力告警卡上——而那张卡本身就要求该规则先触发过。两者都不需要门禁。

面板今天有多处**在猜平台能不能给出某个数**，猜对是因为只跑在 macOS：

| 位置 | 现在的写法 | 问题 |
|---|---|---|
| `views/overview.rs` 压力卡 | ~~`None` 一律显示「无压力接口」~~ | **已改**：`capabilities.memory_pressure` 为假才说「本平台不报告」，为真而值缺失则显示 `—` 与「还没采到」。压缩内存那一行同理，顺带修掉全 app 唯一一处 `n/a` 与 `—` 混用 |
| `views/processes.rs` 展开行 | ~~footprint 为空只显示 `—`~~ | **已改**：平台不支持说「本平台不提供」，平台支持而读不到说「读不到（他人进程）」——macOS 上恒为后者，正是 `proc_pid_rusage` 对 root 守护进程的 EPERM。这是 0.5.2 在 macOS 上唯一带来实际改善的地方 |
| `views/alerts.rs` 的 armed 行 | 不需要改 | 它从未列出 `Pressure` |
| `views/alerts.rs` 的阈值编辑器 | 不需要改 | 压力档位只出现在压力告警卡上，那张卡的存在前提就是规则已触发 |

`shown_memory` 的回落**没有动**（`phys_footprint_bytes.unwrap_or(memory_bytes)`）——行内那个数字在
macOS 上必须一如既往。变的只是**空值怎么解释**。

**macOS 上的实际影响**：压力卡与压缩内存那两处的新分支在 macOS 上不可达（capability 恒为真），
行为逐像素不变；进程展开行则**有意改变**——footprint 为空时从一个沉默的 `—` 变成「读不到（他人进程）」，
这正是采纳 capabilities 的收益，也是它在一个只跑 macOS 的应用上仍然值得做的理由：
它把「面板替 zstats 猜平台能力」这个隐式耦合换成了显式契约，而 CLAUDE.md 的铁律本来就是
「zstats 拥有数字与告警，面板只负责呈现」——猜测恰恰是那条规则的破口。

---

## 二、空态需要第三种情况

面板的空态目前是二元的：**未采集** vs **采集了但没有**。zstats 补齐平台后会出现第三种：**本平台不支持**。

| 视图 | 今天的两种空态 | 缺的第三种 |
|---|---|---|
| `views/apps.rs:50` | `process_groups` 为 `None` → 「需要同时打开 collect-processes 和 process-groups」 | zstats 第 3 条若在 Windows 上停用进程组，这句话就是**假的**——不是用户没开，是平台不支持 |
| `views/sensors.rs` | `None`→未采集 / `Some([])`→「读不到，不少硬件都这样」 | Windows 上温度默认关（zstats 第 7 条）会落到「未采集」，而真实原因是「本平台默认关闭，因为代价大且多半读不到」 |
| `views/overview.rs` 压力卡 | 见上一节 | 同上 |

**方案**：`widgets::empty_card` 增加一个「平台不支持」的变体（措辞与其余两种明确区分），三处调用点按 capabilities 选择。

**macOS 影响**：三处的 capability 都为 true，永远走不到新分支。

---

## 三、文案要有平台维度（第 5 条落地后**必须同批**）

zstats 第 5 条建议在 Windows/Linux 上补 footprint 的等价物。这对面板是**好消息也是陷阱**：内存列会开始有值，但**那不是同一个量**。

| 平台 | zstats 会给的量 | 语义 |
|---|---|---|
| macOS | `phys_footprint` | 私有脏页 + 压缩页 + GPU/IOKit 分配（活动监视器「内存」列） |
| Windows | `PrivateUsage` | 私有提交量，不含压缩、不含 GPU |
| Linux | `Pss` | 共享页**按使用者数量分摊**——两个进程共享 100 MB，各记 50 MB |

三者都叫「实际占用」，但 PSS 的分摊语义与另外两个根本不同。面板现在的文案（`design.md:119` 与 `processes.mem_footprint` 的说明）逐字讲的是 macOS 的记账方式。**若 zstats 的改动先落地而文案没跟上，面板会用一句 macOS 的解释去说明一个 Linux 的数字——那正是这个仓库最不能容忍的那类错误。**

**方案**：把这一列的标签与 tooltip 做成平台变体，与 zstats 的发布**同批上线**，不得滞后一个版本。

**已经走了这条路的一处**：清理提示清单（`assets/cleanhints-macos.toml`）已按操作系统拆成
一文件一平台，`cleanhints::FILE` 按 cfg 选名，嵌入默认、`~/.zstats` 用户覆盖、GitHub 上
的发布副本三者同名。它没有走「一份文件加一列平台」，理由与文案不同——那不是同一句话的
翻译问题，而是**根本没有对应物**：`~/Library/Caches` 无法通过改前缀变成
`%LOCALAPPDATA%`，归属的工具与它们文档里的清理命令也各不相同。取不到本平台文件时的
退化是「没有注解」，行照常渲染。见 `docs/disk-analysis.md`。

顺带记下更大的账：i18n 里已有 **58 处** macOS 专有概念（Spotlight 10、Trash 8、Library 6、launchd 6、Finder 4、tmutil / ⌘Q / iCloud / 完全磁盘访问各 2），而 `i18n_loader` 的 parity 测试目前只保 en/zh 两维，**没有平台变体机制**。真移植时这是一次架构决定（分平台键集 / 运行时选择 / 分平台文件），不是逐条改文案。

---

## 四、不需要改的（记下来省得重新评估）

| zstats 的改动 | 面板影响 |
|---|---|
| 第 1 条 `dedupe_disks` 空键不去重 | **无**。macOS 上卷名从不为空，去重行为不变；面板也没有暴露这个开关 |
| 第 2 条 模板按平台选择 | **无**。面板通过 `apply_add` 写 per-name 覆盖，读 `ActiveThresholds::from_config`；换哪份内置模板对这两条路径透明 |
| 第 3 条 进程组改用 Job Object | **无**（macOS 路径不变）；Windows 上若停用，见第二节的空态 |
| 第 4 条 内存阈值基准 | **无代码改动**。但阈值编辑器的档位数字（`alert-mem` 的预设 chip）是按 macOS 的量纲挑的，换平台后要重挑 |
| 第 7 条 Windows 温度默认关 | **无**（见第二节空态） |

---

## 五、顺序与耦合

1. **第一节（capabilities）可以先做，且只做它也有收益**——它需要 zstats 先发一版带 capabilities 的 API，但面板侧的改动在 macOS 上是零行为变化，可以独立验证；
2. **第二节跟在第一节后面**，它依赖同一个 capabilities 输入；
3. **第三节必须与 zstats 的 footprint 改动同批**——早了没数据，晚了就是用错误的解释说明正确的数字，后者更糟；
4. 其余按 zstats 的节奏走，面板无动作。

## 六、验证方式

任何一条落地后，判断「macOS 未受影响」的标准是固定的：`make lint && make test` 全绿，且**新增的分支在 macOS 上不可达**——capabilities 在 macOS 上三项全 true，所以新写的每一个「不支持」分支都应该是死代码。如果某个新分支在 macOS 上被走到了，那就是方案理解错了，不是实现写错了。
