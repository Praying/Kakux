---
title: Kaku 配置：先改善工作流，不盲调性能
subtitle: 2026-11-13 · 本机配置与 Kakux 工作区静态检查
file: kaku-config-optimization-2026-11-13
template: doc
lang: zh
---

## A 结论与范围

**优先尝试紧凑布局、分屏焦点提示和标签编号。AI 方面，再评估是否需要不同的快捷与深度模型。**

这里先把“配置优化”理解为本机使用配置。配置系统本身的改进放在最后一节。

- `/home/quran/.config/kaku/kaku.lua:53` 只显式覆盖了深色主题。
- 其他设置依赖它实际找到的 bundled Lua 文件。
- 当前仓库已有 Linux 分支；不能照搬全部 macOS 示例。
- 两个 AI 模型字段当前指向同一个模型。
- 没有核实运行中进程加载的文件，也没有做性能测试。

本页未修改个人配置、模型或服务地址。它不包含 API key。

## B 最值得尝试的使用配置

| 优先级 | 方向 | 建议 | 取舍 |
| --- | --- | --- | --- |
| 高 | 多分屏空间 | `split_pane_gap = 1`，侧边距先试 `16px` | 内容区更宽，但留白更少 |
| 高 | 焦点辨认 | `inactive_pane_hsb = { saturation = 1.0, brightness = 0.92 }` | 更容易分辨活动窗格，非活动内容会变暗 |
| 中 | 多标签导航 | `show_tab_index_in_tab_bar = true` | 编号占少量空间，但位置更明确 |
| 中 | 日志回看 | 确实丢失旧输出时，再把 `scrollback_lines` 调到 `20000` | 保留更多历史，也会增加内存占用 |
| 中 | 文件跳转 | 将 `file_link_editor` 设为已安装、支持路径定位的编辑器 | 先验证实际命令和行列定位 |
| 按习惯 | 剪贴板保护 | 经常只选中阅读时，试 `copy_on_select = false` | 避免选中文本覆盖剪贴板，但需手动复制 |

这是个人偏好试验，不建议直接修改产品默认值。

依据：`assets/macos/Kaku.app/Contents/Resources/kaku.lua:246`、`:4188`、`:4956`；`config/src/config.rs:679`。

## C 最小试用方案

把这些覆盖项放在个人 `kaku.lua` 的 `return config` 之前。建议逐项试用，而不是一次全部启用。

```lua
config.window_padding = {
  left = '16px', right = '16px',
  top = '16px', bottom = '0px',
}
config.split_pane_gap = 1
config.inactive_pane_hsb = {
  saturation = 1.0, brightness = 0.92,
}
config.show_tab_index_in_tab_bar = true
```

检查普通窗口、全屏和三窗格布局。确认顶栏、首行和中文输入候选框没有遮挡。

仓库默认分屏间隔为 `2`，对应五列宽的分隔区域。改成 `1` 后为三列。

显式设置 padding 会替代内置的屏幕尺寸自适应策略。若现在的留白舒服，保留默认，只试分屏间隔。

依据：`config/src/config.rs:679`；`assets/macos/Kaku.app/Contents/Resources/kaku.lua:246`。

## D AI 配置：先验证能力，再选择模型

当前快捷模型与深度模型相同。这样配置合法，也最简单；但没有形成两档模型选择。

- 如果主要使用 `#` 生成命令，可以继续保持现状。
- 如果经常做多轮分析或调用工具，再为 `chat_model` 选择不同模型。
- 不凭模型名称推断能力；先确认企业网关实际提供的模型。
- 当前选择 `responses` 模式，不能据此确认网关完整兼容。
- 先验证普通回复、流式回复、工具调用及取消请求。
- 只有网关不支持 Responses 时，才考虑 `chat_completions`。

网关模型很多或模型发现失败时，可用 `chat_model_choices` 设置已验证的列表。不建议在没有症状时添加它。

这次没有请求企业网关，也没有测量模型质量或延迟。不建议直接启用原生联网搜索或更换服务地址。

依据：`/home/quran/.config/kaku/assistant.toml:8`；`docs/features.md:39`；`kaku/src/assistant_config.rs:100`。

## E 现在不建议改的设置

| 设置 | 建议 | 原因 |
| --- | --- | --- |
| `front_end` / GPU 偏好 | 保留仓库的 `WebGpu` / `LowPower` | 没有硬件或帧时间证据支持换后端 |
| `max_fps` / `animation_fps` | 暂时保留 `60` | 更高帧率不等于更快；低帧率也不保证节电收益 |
| `status_update_interval` | 保留 `1000` | 它控制状态更新调度，不是通用加速开关 |
| `line_height` | 保留产品默认 `1.28` | 这是已接受的排版取舍，不重做自然高度块字符方案 |
| 关闭确认 | 保留 `SmartPrompt` | 不用关闭保护来换取表面上的操作速度 |
| 透明度 | 保留不透明，除非确实喜欢透明效果 | 不为视觉效果引入新的阅读干扰 |

先有卡顿或耗电复现，再对同一负载做对比。不要同时改字体、缓存、渲染后端和帧率。

依据：`assets/macos/Kaku.app/Contents/Resources/kaku.lua:4166`、`:4225`、`:4956`；`kaku-gui/src/termwindow/mod.rs:4062`。

## F 如果你问的是配置系统本身

**优先提升配置的可理解性与可预测性，而不是继续增加开关。**

| 优先级 | 改进候选 | 已看到的依据 | 实施前验证 |
| --- | --- | --- | --- |
| 高 | 按平台生成正确的示例与默认值说明 | 用户模板仍写 JetBrains Mono、底部标签；仓库 Linux 默认已不同 | 对比新安装生成文件与 Linux 实际加载值 |
| 高 | 提供加载路径与生效值的诊断信息 | 用户配置通过多个候选路径寻找 bundled Lua | 先检查现有诊断能否回答问题，避免重复入口 |
| 中 | 减少对 Lua 文本写法的依赖 | 默认配置逐行匹配 `config.font =`、`config.window_padding =` | 用别名、表索引和模块配置做探针，确认影响再改 |
| 中 | 约束大型 bundled Lua 的复杂度 | 文件已有 5187 行，快捷键构造还需规避寄存器预算 | 先测加载时间与加载测试，再考虑小范围拆分 |

这些是改进候选，不是已复现的缺陷。不做未经测量的大规模配置重构。

依据：`config/src/lib.rs:830`、`:860`；`assets/macos/Kaku.app/Contents/Resources/kaku.lua:83`、`:3953`、`:4241`、`:4572`。

## G 证据边界

仓库：`/home/quran/projects/personal/Kakux`，HEAD `0ea81f50`。本次直接读取工作区源码；GitNexus 未索引该仓库。

相对源码路径均以该仓库为根。个人配置文件内容已读取，但没有验证运行时最终生效值。

未改动现有代码和配置，未运行 GUI、构建或测试。公开文档检索没有返回可用结果，因此结论只依赖本地源码与配置。
