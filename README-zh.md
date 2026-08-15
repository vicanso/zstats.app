# zstats

[English](README.md) | 中文

macOS 菜单栏系统监控：托盘实时显示 CPU，面板提供进程监控、阈值告警与磁盘空间分析，支持识别并安全清理可再生缓存。

**macOS 菜单栏监控。** 看清机器在忙什么，该提醒时提醒，磁盘空间也能找回来。

托盘实时显示 CPU，点一下弹出面板，看别处就收起。采集、告警、历史全部来自进程内的 [zstats](https://crates.io/crates/zstats)，和 CLI 共用 `~/.zstats`，两边的数字永远对得上。

> 仅 macOS · Apple Silicon 与 Intel · Universal，已签名公证

<img width="718" height="1356" alt="Image" src="https://github.com/user-attachments/assets/09bd8ca5-eef5-4360-b527-8502a0e52148" />

## 为什么是这个

多数菜单栏监控要么只画数字，要么只会催。zstats 两样都做，还能动手：找出那 20 GB 缓存，按 Finder 的方式丢进废纸篓，请内存大户自己退出。数字就是 `zstats` CLI 会印出来的那份——不是另一套差不多的采集器。

## 看

- 托盘旁实时 CPU%
- 概览：P/E 核、内存与压缩、内核内存压力、磁盘/网络吞吐
- 进程按 60 秒均值排序，可按名过滤，一键扫完整张表
- 应用按进程树聚合——浏览器和它所有助手占一行
- 硬件：磁盘卷、最热的传感器、电池健康
- 历史：今天真正烧掉的 CPU，按累计时间排，不是一次尖峰

## 告

阈值（CPU、内存、磁盘、内存压力）由 zstats 规则引擎评估，面板里就能改。

- 原生通知横幅
- 按事件静音 1 或 3 小时
- 内存压力卡片列出占用大户，并提供礼貌退出（⌘Q / SIGTERM，绝不 SIGKILL）

## 清

- **大文件秒查** — Spotlight，≥500 MB（命中太少则降到 ≥100 MB）
- **目录分析** — 后台走一遍家目录（几十万个目录大约半分钟），三张排名：可再生缓存（`CACHEDIR.TAG`）、大文件夹、索引看不见的大文件
- 边扫边出结果；点目录即时下钻；分析根目录可自选
- 清理建议：带 TAG 的缓存加上已知工具缓存（npm、Cargo、Xcode …），一点进废纸篓，并附上工具自己的清理命令
- 规则可换：放一份 `~/.zstats/cleanhints.toml` 即可整体替换内置清单

## 安全

面板对系统动手的地方只有两处，都要确认，都能反悔：

| 动作 | 实际做的事 |
| --- | --- |
| 删除 | Finder 的移入废纸篓。绝不 `rm -rf`。 |
| 退出 | ⌘Q 等价请求 / SIGTERM。绝不 SIGKILL。 |

没有自动清理、自动杀进程。Mail、Messages 等受保护数据零接触跳过。首次分析出现的「桌面 / 文稿 / 下载」授权，就是分析本身。

## 安装

从 [Releases](../../releases) 下载 `zstats.dmg`，拖进「应用程序」。

```bash
make bundle          # 或从源码构建（需要 cargo-bundle）
```

不要和 `zstats serve` 一起跑，会重复采集。

语言、主题、面板透明度在设置窗口。界面完整中英双语，深浅色走原生毛玻璃。

## 开发

```bash
make dev             # 面板常驻不收起
make lint && make test
```

设计记录：[docs/design.md](docs/design.md) · [docs/disk-analysis.md](docs/disk-analysis.md)

Apache-2.0 · [gpui](https://github.com/zed-industries/zed) · [zstats](https://crates.io/crates/zstats)
