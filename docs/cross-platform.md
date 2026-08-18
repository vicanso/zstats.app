# 面板侧的跨平台调整方案

这份文档回答一个假设性问题：**假如 zstats 按 [`zstats/docs/cross-platform.md`](https://github.com/vicanso/zstats) 那七条做完了优化，面板这边要跟着改什么。**

它不是移植计划。面板自身的移植可行性是另一件事，结论写在 `design.md` 的「非 macOS 平台编译不过」一节：Linux 上托盘不发点击事件、Wayland 不允许客户端定位窗口、`gpui_linux` 的 `activate`/`hide` 是静默 no-op，三条叠加意味着「点菜单栏图标弹出锚定面板」这个形态在 Linux 上无法还原；Windows 外壳可行但数据层要等 zstats 补课。**本文只处理数据层那一半**——即使面板永远只跑在 macOS 上，其中一部分改动（第一节）也是净收益。

贯穿全文的硬约束：**macOS 上的行为必须逐项不变。** 下面每条方案都按「在 macOS 上求值为今天的样子」设计，capabilities 在 macOS 上三项全 true，所有分支落回现有路径。

---

## 一、停止猜测，开始读取能力（唯一一条现在就有价值的）

对应 zstats 的第 6 条（capabilities 描述符）与第 5 条（规则的「支持」与「启用」分离）。

面板今天有多处**在猜平台能不能给出某个数**，猜对是因为只跑在 macOS：

| 位置 | 现在的写法 | 问题 |
|---|---|---|
| `views/overview.rs:246` | `pressure_level` 为 `None` → 显示「无压力接口」，tooltip 写死「当前平台不报告内核内存压力」 | 把三种 `None`（平台不支持 / 采集未开 / 首次采样）**当成一种**。macOS 上恰好只可能是第一种 |
| `views/processes.rs:74` | `phys_footprint_bytes.unwrap_or(memory_bytes)` | 静默换量纲。macOS 上 root 进程因 EPERM 落回 RSS，展开行有并列标注；但那是人工补的诚实，不是 API 给的 |
| `views/alerts.rs` 的 armed 行 | 逐条列出基础阈值 | 一条**本平台永不触发**的规则（Pressure）照样被列为「已武装」 |
| `views/alerts.rs` 的阈值编辑器 | `alert-pressure` 的档位 chip 无条件渲染 | 同上，用户可以调一个不会生效的值 |

**方案**：zstats 提供 capabilities 后，这四处改为读取而非假设。

- `pressure` 的第四臂条件从「值是 None」改为「本平台不支持」，文案相应从「当前平台不报告」（一句猜测）变成陈述；若平台支持但本次为 `None`，那是「等待采样」，与其它指标一致显示 `—`；
- `shown_memory` 的回落保持不变（**macOS 行为不能变**），但展开行的说明可以说出原因：「本平台不提供」与「无权限读取」是两句不同的话，后者才是 macOS 上 root 进程的真实情况；
- armed 行与阈值编辑器按「supported」过滤：不支持的规则不列、不给编辑入口，而不是列出来再让它沉默。

**macOS 影响**：capabilities 三项全 true，四处分支全部落到今天的路径。armed 行少不了任何一条，pressure 的第四臂在 macOS 上本来就走不到。

**这条值得单独做**，即使永不移植：它把「面板替 zstats 猜平台能力」这个隐式耦合换成显式契约，而 CLAUDE.md 的铁律本来就是「zstats 拥有数字与告警，面板只负责呈现」——猜测恰恰是那条规则的破口。

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
