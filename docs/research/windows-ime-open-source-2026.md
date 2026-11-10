# Windows 开源／社区中文输入法调研（2026）

> 检索日期：**2026-11-06**（本文所有 GitHub 指标、Release 版本、下载计数均为该日快照）
> 范围：在 Windows 上有当前可用证据、仍处于维护状态的中文（含繁体）输入法项目，包括引擎、Windows 前端、以及把它们串起来的配置方案与词库生态。
> 不覆盖：商业闭源产品（另见商业输入法报告）、以及没有 Windows 版的移动端／Linux／macOS 项目（仅在第 10 节列为排除项）。

---

## 0. 结论摘要

1. **Windows 开源输入的“在位者”仍是 Weasel（小狼毫），但它已进入慢速维护期。** 稳定版 0.17.4 发布于 **2025-06-04**，到检索日已 **17 个月没有新的稳定版**，只有 2026-10-31 的 nightly。项目本身没死（提交、Issue、PR 都活跃），但“稳定交付节奏”是它最大的产品缺口。
2. **“引擎 + 薄 TSF 壳 + 独立 Server 进程”已是 Windows 开源输入的共识架构。** Weasel、PIME、水杉、青简、清风、XSIME、0c、墨奇全部采用这一结构。2025–2026 年新项目里，**Rust/C++ 写 TSF 壳、Go 或 Rust 写引擎服务、命名管道通信**成为主流组合——与本项目技术选型一致。
3. **2026 年出现了一批“新架构 + 完整产品化”的挑战者**，且都不是简单复刻 Rime：水杉（3,359★，纯 TSF 自研引擎）、青简（7,617★，Core/壳分离 + 本地整句模型）、清风（1,413★，码表优先 + 方案驱动）。它们补齐的正是 Weasel 缺的东西：图形化设置、开箱词库、签名安装包、按应用兼容规则。
4. **“开箱即用配置”的热度显著高于引擎本身。** 雾凇拼音 20,050★，超过 librime（4,711★）与 Weasel（8,263★）。这既是 Rime 生态的活力证明，也是“原生 Rime 配置门槛高”的反证。
5. **隐私承诺在开源阵营内部分化明显。** DIME 明确“不学习输入内容、不联网”，雾凇拼音标榜“完全离线”，清风没有任何云输入功能但也没有独立隐私文档；而水杉 **Windows 版云候选默认开启**（首次安装向导页预先勾选，把正在组的拼音经 HTTPS 发给 `inputtools.google.com`），是开源输入法里少见的默认联网值。青简的云功能默认关闭、无自建服务器、密码框绝不发送，是当前**边界描述最清楚**的一份。**注意**：水杉核心仓 README 声称六平台都有可关闭的匿名使用统计，而其 `msime-windows/PRIVACY.md` 明确写 Windows 版“无遥测”——两处表述冲突，本文只按 Windows 专仓的隐私文档描述，并把它列为待核实项。
6. **真正的共同难点不是算法，而是 Windows 平台工程**：代码签名与 SmartScreen/杀软误报、TSF DLL 被锁进几十个宿主进程导致升级必须重启、候选窗盖不住开始菜单（UIAccess）、游戏反作弊拒绝加载 IME。
7. **对 Silk 的判断：** 不要做“又一个 Rime 前端”。可借鉴的是进程结构、按应用兼容规则、词库分层与发布工程；要超越的是安装即用、稳定发布节奏、可验证的联网边界和签名交付。

---

## 1. 统计口径与证据等级

### 1.1 本文使用的指标

| 指标 | 来源 | 能说明 | 不能说明 |
|---|---|---|---|
| GitHub stars | GitHub REST API（`/repos/{owner}/{repo}`）与 shields.io 徽章 | 社区关注度、传播广度 | 用户数、安装量、Windows 市占 |
| Release 资产下载计数 | GitHub Releases API 的 `assets[].download_count` | 经官方 GitHub 渠道的分发量级 | 去重用户、真实使用；不含镜像站、网盘、第三方转载、包管理器 |
| 最新稳定版 / nightly 日期 | Releases、`releases.atom`、官网下载页 | 发布节奏与维护活跃度 | 代码质量 |
| 最后 push 时间 | GitHub API `pushed_at` | 仓库是否仍在改动 | 改动是否面向用户 |
| Open issues 数 | shields.io `issues` 徽章 | 待办压力、反馈密度 | 严重程度分布 |
| 系统要求 / TSF 支持 | 官方 README、官网下载页、安装文档 | 平台兼容承诺 | 实际在各类宿主程序中的表现 |

**明确不做的事：**

- 不把 stars、下载量、Issue 数换算成“Windows 市场份额”。Windows 上不存在可信的第三方输入法活跃份额公开数据（Windows 总设备数 ≠ 任一 IME 用户数）。本文只写“活跃／有社区热度／分发量级”。
- 不把移动端 MAU、Play 安装档与桌面数据混算。
- 不把“配置方案／词库”的 stars 计入输入法本体的热度。

### 1.2 术语分层（本文严格区分）

这是理解 Windows 开源输入生态的前提，混用会导致错误结论：

| 层 | 定义 | 本文涉及 |
|---|---|---|
| **引擎** | 把按键序列转成候选与上屏文本的算法库，无 UI、无系统集成 | librime、MSIME-Engine、清风 `wind_input`、青简 `qingjian-core`、0c 的 Go 引擎、PIME 的 Python/Node/Go 后端运行时、Yong 自有引擎 |
| **Windows 前端 / 外壳** | 实现 TSF（`ITfTextInputProcessor`）、被加载进宿主进程、负责组字区与候选窗 | Weasel、PIME（PIMETextService）、水杉 `windows/`、青简 `qingjian_tsf.dll`、清风 `wind_tsf`、XSIME `RimesTsf.dll`、0c `0c_tsf.dll`、DIME、windows-chewing-tsf、墨奇 |
| **服务进程** | 独立于宿主进程，持有唯一引擎实例，经 IPC 通信 | WeaselServer、PIMELauncher、水杉 Server、`qingjian-server.exe`、`0c-server.exe`、`RimesBroker.exe` |
| **输入方案（schema）** | 定义“按键怎么解释”的配置：全拼／双拼键位、码表、辅助码规则 | Rime `*.schema.yaml`、清风 `*.schema.toml`、水杉内置方案 |
| **词库** | 字词 + 读音 + 词频的数据 | 雾凇、白霜、万象、极点五笔移植等 |
| **配置发行版** | 打包好的方案 + 词库 + 补丁，供用户整体替换 | 雾凇拼音、白霜拼音、万象拼音、oh-my-rime |

---

## 2. Weasel 小狼毫（Rime 的 Windows 前端）

| 项 | 内容 |
|---|---|
| 类型 | Windows 前端（TSF）+ 依赖 librime 引擎 |
| 仓库 | <https://github.com/rime/weasel> |
| 许可证 | GPL-3.0（引擎 librime 为 BSD-3-Clause） |
| 语言 | C++ |
| Stars / Forks | **8,263 / 857**（2026-11-06 快照） |
| 最新稳定版 | **0.17.4，2025-06-04** |
| 最新 nightly | **2026-10-31**（`latest` 预发布标签） |
| 最后 push | 2026-10-31 |
| Open issues | 约 258（shields.io，2026-11-06） |
| 累计 Release 下载 | 约 **1,389,210** 次（11 个 release 的资产合计） |

**Windows 支持与系统要求**

- 官网下载页明示“适用于 Windows 8.1, Windows 10, Windows 11”（<https://rime.im/download/>）。旧版 0.14.3 覆盖 Win7、0.9.30 覆盖 XP，均已停止更新。
- 仓库含 `arm64x_wrapper/`（`WeaselTSF_arm64.def`／`WeaselTSF_x64.def`），说明存在 ARM64X 打包路径；官方下载页未单列 ARM64 安装包 → **[INFERENCE]** ARM64 支持已在工程内，但未作为独立卖点宣传。
- 安装需要管理员权限；输入法注册到指定输入语言（可选项含「中文（台湾）」）。

**架构**

`WeaselTSF`（宿主内 DLL）+ `WeaselIPC/IPCServer`（命名管道）+ `WeaselServer`（唯一引擎实例与托盘）+ `WeaselUI`（候选窗，DirectWrite 渲染）+ `WeaselDeployer`（重新部署、方案编译）+ `WeaselSetup`（注册/语言配置）。用户词库与配置在 `%AppData%\Rime`，改完必须“重新部署”才生效。

**输入方案与核心功能**

- 安装包预装：朙月拼音（全拼）、注音／地球拼音、仓颉五代；五笔、粤拼、吴语、中古汉语、国际音标等需经 plum 另行安装（README 明确“不再以安装包预装形式提供”）。
- 方案切换 `Ctrl+\`` 或 `F4`；支持内联预编辑、横排/竖排候选、配色主题、vim 模式。
- 0.17.x 的主要修复集中在**稳定性与宿主兼容**：托盘图标卡死、多线程导致服务崩溃、部分应用无法显示输入法、显卡重置后文字绘制失败、全屏高亮绘制错误、热键为空时 Deployer 崩溃、升级后重启导致文件被删。

**AI / 语音 / 云能力**

- 本体无 AI、无语音、无云候选。能力全部来自配置层：librime 的 Lua 扩展（`librime-lua`）可挂接外部逻辑；社区存在接入大模型的方案，但不属于官方功能。

**隐私与联网**

- 输入转换完全本地；无账号、无遥测。
- 唯一的常规出网是更新检查：`WeaselServerApp.cpp` 调用 `win_sparkle_set_registry_path("Software\\Rime\\Weasel\\Updates")` + `win_sparkle_init()`，即使用 WinSparkle 读取 appcast（`update/appcast.xml` 指向 `rime.github.io/release/weasel/`）。源码中未见显式关闭自动检查的调用 → **[INFERENCE]** 默认会周期性联网检查更新，可通过托盘「检查更新」菜单项交互触发，也可在注册表关闭。这一点在“纯本地”叙事中常被忽略。
- 安装包自身提示：安全软件可能对 IME 注册服务报警，需核对 checksum 后再放行。

**安装与配置门槛（关键劣势）**

1. 装完是“空引擎 + 少量方案”，简体用户通常要自己找配置发行版（雾凇/白霜/万象），清空用户目录、解压、重新部署。
2. 配置是 YAML + `patch` 覆写 + 方案/词库两层文件，改错一个缩进就部署失败，且错误提示不友好。
3. “重新部署”是显式动作，非即时生效。
4. 图形化设置只有 `WeaselDeployer` 的有限几个开关（输入语言、候选数、字体、配色），无方案管理 GUI。

**优劣**

| 优势 | 劣势 |
|---|---|
| 生态最大：方案、词库、教程、第三方工具齐全 | 17 个月无稳定版，用户被迫在 0.17.4 与 nightly 之间选 |
| 引擎/前端分层干净，跨平台方案可复用 | 开箱不可用（简体），配置门槛高 |
| 完全本地、无账号、GPL 自由 | 宿主兼容问题长期存在（Electron、游戏、全屏、高 DPI） |
| 1.39M 次官方下载，是开源 Windows IME 中分发量最大的 | 安装包未在当前渠道做现代代码签名，SmartScreen 易拦（官方仅提示核对 checksum） |

**对 Silk 的启示**

- Weasel 证明了“引擎在独立进程 + DLL 极薄”能跑十几年，这是必须继承的结构。
- 它的失败点是**交付**而非架构：把“方案/词库选择 + 部署 + 重启”这套流程对用户隐藏，就是 Silk 的产品机会。
- 它的 0.17.x 修复清单几乎就是 Silk 的宿主兼容测试清单（托盘崩溃、显卡重置、全屏高亮、升级后文件残留、空热键崩溃）。

---

## 3. PIME（可编程输入法框架）

| 项 | 内容 |
|---|---|
| 类型 | Windows 前端框架（TSF 封装）+ 多语言后端运行时 |
| 仓库 | <https://github.com/EasyIME/PIME> |
| 许可证 | LGPL-2.1（README；GitHub 识别为 `NOASSERTION`，以 README 与 LICENSE 文本为准） |
| 语言 | C++（libIME）+ Python 3 / Node.js / Go 后端 |
| Stars / Forks | **1,530 / 257** |
| 最新版本 | **2.0.0-beta4，2026-10-23**（预发布） |
| 最后一个稳定版 | **v1.3.0-stable，2023-01-20** |
| 最后 push | 2026-11-01 |
| Open issues | 约 345 |
| 累计 Release 下载 | 约 **459,890** 次（32 个 release 资产合计） |

**架构**

- `libIME`（后被抽为独立仓库 [libIME2](https://github.com/EasyIME/libIME2)）是对 TSF 的轻封装；`PIMETextService` 是文本服务骨架；后端是独立进程，通过命名管道通信。
- 后端运行时：内嵌 Python（beta4 升至 3.15.1）、内嵌 Node.js（22.23.4 LTS）、以及 2026 年新增的 **Go 后端**（`go-backend/`，含 `simple_ime` 示例）。
- 输入法即普通 Python/JS/Go 模块，`ime.json` 声明名字、图标、后端。

**捆绑的输入方案（仓库实测目录）**

- `python/input_methods/`：`chewing`（新酷音注音）、`checj`（仓颉）、`chedayi`（大易）、`chearray`（行列）、`cheliu`（呒虾米）、`chephonetic`、`chepinyin`、`chesimplex`（速成）、`cheeng`、`cheez`、`braille_chewing`（点字）、`meow`（喵码表）、`rime`（Rime 桥接）。
- `node/input_methods/`：`emojime`、`meow`。
- 也就是说：**PIME 本身不提供拼音引擎，靠 `rime` 模块转调 librime**；自有方案全部是繁体码表谱系。

**Windows 支持**

- 安装到 `C:\Program Files (x86)\PIME`，同时注册 x86 与 x64（或 ARM64）TSF DLL。
- beta4 新增 **ARM64X 转发 DLL**，使原生 ARM64 应用（Windows Terminal、Edge、Office）可加载 PIME；同时修复 32 位 Node 缺失后继续用 22 LTS 的兼容策略。
- 未在 README 明示最低 Windows 版本（TSF 语义上需 Vista 以上）。

**2.0 的实际进展（重要）**

beta4（2026-10-23）是一次**紧急安全更新**：修复内嵌 Python/Node 与第三方依赖的多项 CVE（远程代码执行、内存破坏），并重构 RPC 管道（异步连接、自愈重连、JSON 解析非抛异常、命名管道长度边界检查）、重做安装器。这说明 2.0 仍处在“修基础、补安全”阶段，而不是新增输入能力。

**隐私与联网**

- 框架层无遥测、无账号。是否联网取决于所加载的 Python/JS 模块——例如 `rime` 模块继承 librime 的本地行为，而第三方脚本可以是任意代码。
- **风险点**：输入法后端是可执行脚本，插件即任意代码执行；beta4 的 CVE 修复清单正说明这类“嵌入式运行时 + 第三方依赖”的维护负担会直接变成安全问题。

**维护状态判断**

- 活跃但**长期 beta**：稳定版停留在 2023-01，2.0 已连出 4 个 beta；345 个未关 Issue 说明反馈积压严重。
- 主要价值是“给非 C++ 开发者一条做 Windows IME 的路”，而不是给终端用户的成品输入法。

**优劣**

| 优势 | 劣势 |
|---|---|
| 用 Python/JS/Go 写输入法，原型速度快 | 稳定版停滞近 4 年，2.0 仍 beta |
| 架构被后续项目复用（墨奇明确参考 PIME 与 Weasel；libIME2 被墨奇用作 TSF 层） | 自带方案偏繁体码表，无现代拼音体验 |
| 支持 x86/x64/ARM64 三架构 | 嵌入整运行时 → 体积、内存、CVE 面全部放大 |
| LGPL-2.1 允许闭源链接（相对 GPL 更宽松） | 插件无沙箱，供应链风险高 |

**对 Silk 的启示**

- **反面教材**：为了“可扩展”而内嵌完整语言运行时，会把输入法的安全边界扩到该运行时及其依赖的 CVE 面。Silk 若做扩展，宜采用**受控协议 + 声明式配置**，而不是“用户可放脚本进 IME 进程”。
- **正面借鉴**：`ime.json` 式的声明清单（名字/图标/后端）与多后端并存，是低成本支持多种输入方案的干净做法。
- LGPL-2.1 的 libIME2 可以被 GPL-3.0 项目（含 Silk）安全使用，是 TSF 薄壳的现成参考实现之一。

---

## 4. Rime 的引擎与配置层（librime / plum / 方案发行版）

这一层不直接面向终端用户，但决定了所有 Rime 前端的能力上限，也是 Silk 在“输入方案与词库”上最可能的复用来源。

| 项目 | 层次 | 许可证 | 最新 | Stars | 最后 push |
|---|---|---|---|---|---|
| [librime](https://github.com/rime/librime) | 引擎 | BSD-3-Clause | 1.17.0（2026-06-06）；nightly 2026-11-05 | **4,711** | 2026-11-05 |
| [plum 東風破](https://github.com/rime/plum) | 方案/词库包管理器 | LGPL-3.0 | 无 Release，按 commit 分发 | 1,963 | 2026-05-08 |
| [rime-ice 雾凇拼音](https://github.com/iDvel/rime-ice) | 配置发行版 | GPL-3.0 | 2026.06.30 | **20,050** | 2026-10-31 |
| [rime-wanxiang 万象拼音](https://github.com/amzxyz/rime-wanxiang) | 配置发行版 + 词库 | CC-BY-4.0 | v18.1.9（2026-11-06） | 5,076 | 2026-11-06 |
| [oh-my-rime](https://github.com/Mintimate/oh-my-rime) | 配置模板 | GPL-3.0 | 无 GitHub Release | 5,019 | 2026-10-29 |
| [rime-frost 白霜拼音](https://github.com/gaboolic/rime-frost) | 词库 | GPL-3.0 | 持续 | 3,811 | 2026-11-04 |
| [rime-wubi](https://github.com/rime/rime-wubi) | 方案（五笔） | LGPL-3.0 | 仓库自 2024-01 起无更新 | 351 | 2024-01-05 |
| [rime-double-pinyin](https://github.com/rime/rime-double-pinyin) | 方案（双拼） | GPL-3.0 | 2025-09 | 479 | 2025-09-23 |

**关键判断**

1. **星数分布证明需求在“配置”而非“引擎”。** 雾凇（20,050★）是 librime（4,711★）的 4.3 倍、Weasel（8,263★）的 2.4 倍。这不能当成用户量，但能说明：新人进入 Rime 生态时，第一需求是“一份能直接用的配置”。
2. **雾凇拼音的安装说明就是 Rime 门槛的量化证据**：三步 = 装 Rime 前端 → 清空用户目录全部文件 → 解压覆盖 → 重新部署。官方自述“适配小狼毫、鼠须管、Fcitx5、iBus，几乎全部 Rime 应用”，也提供 plum 与命令行路径——但仍是“文件操作型”安装。
3. **词库层已经出现工程化竞争**：白霜拼音自述用高质量语料重新统计字频/词频并做归一化，作者另建了[方案评测仓库](https://github.com/gaboolic/rime-schema-compare)横向比较各方案准确率；万象拼音把“带调拼音标注 + 词组构成 + 精准词频 + 语法模型”作为卖点。墨奇、清风都直接内置白霜词库——**词库正在成为可采购／可复用的组件**，而不是绑定在某一个输入法里。
4. **许可证差异要注意**：万象拼音用 CC-BY-4.0（数据许可，不是软件许可），雾凇/白霜/oh-my-rime 用 GPL-3.0，librime 是 BSD-3-Clause。Silk 自带 `LICENSE` 为 GPL-3.0，复用这些词库在许可上可行，但**必须在 NOTICE 里逐项标注**（清风的 NOTICE.md 是很好的样板）。

---

## 5. 2026 年出现的新架构挑战者

### 5.1 水杉输入法 MSIME（metasequoiaime）

| 项 | 内容 |
|---|---|
| 类型 | Windows 前端（**纯 TSF，自研引擎，非 Rime**）+ 跨平台共享引擎 |
| 仓库 | [msime-windows](https://github.com/metasequoiaime/msime-windows)（Windows 产品）、[msime](https://github.com/metasequoiaime/msime)（跨平台核心/文档） |
| 许可证 | GPL-3.0-only |
| Stars | **3,359**（Windows 仓）/ 约 1.2k（核心仓） |
| 最新版 | **v0.13.2，2026-11-04**（29 个稳定版，迭代极快） |
| 创建 / 最后 push | 2023-10-14 / 2026-11-06 |
| Open issues | 约 341 |
| 累计 Release 下载 | 约 **44,959** 次 |

**架构**：`windows/`（TSF DLL：按键预判、焦点、edit session、管道协议）+ `server/`（算法调度、候选窗与工具栏宿主、设置程序）+ `ui/`（自研原生 GUI 框架 `msimeui`，Win32 宿主 + Direct2D/DirectWrite）+ `ui-html/`（候选窗与设置页的 HTML/CSS）+ `installer/`（**自签名** + Inno Setup）。引擎在仓外 `MSIME-Engine`（C++，Windows/macOS/Linux 前端共用）。

**功能**：全拼、双拼（小鹤/自然码/首道/微软）、86 五笔、辅助码（蓝天小雨点/自然码/首右/小鹤）、日文罗马字、中英混输、emoji、手写识别、屏幕键盘、剪贴板历史、悬浮工具栏、多套皮肤。**候选旁显示中英互译释义**是其差异化（也是青简在自家设计文档里明确对标的形态）。

**AI / 云**：谷歌云候选（`general.cloud_candidates`）、AI 联想（DeepSeek/OpenAI/SiliconFlow/Groq，需自填 token）、候选翻译（腾讯云 TMT）、语音输入（豆包流式/OpenAI 等）。

**隐私（重点，官方 PRIVACY.md 逐条可核）**

- 本地转换不联网。
- **云候选默认开启**：首次安装有向导页说明并预先勾选；开启时把正在组的拼音经 HTTPS 发给 `https://inputtools.google.com/request`。不发送已上屏文本、词库、词频、设置或账号。
- AI 联想/翻译/语音默认 `enabled = true` 但 token 是占位符，运行时拒绝占位符 → 未配置时不发请求。
- 该仓 `PRIVACY.md` 明确写 **Windows 版无任何遥测**（并给出“搜索 telemetry/analytics/sentry/… 无结果”的核验方式）；更新检查仅在按按钮时发生，不携带标识。
- 已知短板：**API token 明文存于 `config.toml`**（其他平台用 Keychain/Secret Service）；卸载会删除整个 `%LOCALAPPDATA%\metasequoiaime`（含用户词库），需先备份。
- 安装包**自签名**（非 CA 签发）。

> **⚠️ 与官方另一处表述冲突**：跨平台核心仓 [msime](https://github.com/metasequoiaime/msime) 的 README 称“六平台都会向 `api.msime.app` 发送每日活跃、会话和崩溃记录，**默认开启**，可在设置里关闭”。两处对 Windows 版的描述不一致，本文不替其调和，仅记录冲突并在第 11 节列为待核实项；选型时应以实测网络行为为准。

**社区信号**：README 记录 2026-09-02 前后“约 5000 名新用户加入，内测 API 配额被耗尽”，并公开招募开发者、标注 good first issue / no-code / needs-design。属于典型的“短期爆发 + 积极扩张”阶段；约 341 个未关 Issue 与 29 个稳定版并存，说明迭代快、遗留也多。

### 5.2 青简 Qingjian

| 项 | 内容 |
|---|---|
| 类型 | 跨平台前端（Windows TSF / macOS IMK / Linux Fcitx / Android）+ Rust 共享核心 |
| 仓库 | [qingjian-team/qingjian](https://github.com/qingjian-team/qingjian)、官网 <https://qingjian.app> |
| 许可证 | GPL-3.0-or-later（名称与 logo 不在代码授权内） |
| Stars | **7,617**（创建于 **2026-09-07**，两月内） |
| 最新版 | v0.3.2，2026-10-30 |
| 累计 Release 下载 | 约 **169,710** 次 |
| Open issues | 约 188 |

**架构（明确对标 Weasel）**：`apps/windows/tsf`（`qingjian_tsf.dll`，只做按键转发与文档写入，候选窗由 Server 自绘）+ `apps/windows/server`（`qingjian-server.exe`，持有唯一 `qingjian-core::Engine`）+ 命名管道 `\\.\pipe\qingjian`。CRATE 划分：`qingjian-core / dictionary / learning / lm / neural / predict / translate / voice / render / platform / update`。官方文档给出的理由值得直接引用：**核心不能待在 DLL 里，否则会被复制进几十个进程、状态无法共享、崩溃会连累宿主**；并且刻意把 DLL 与 Server 拆成两个 package，用 crate 边界强制“DLL 不含 Engine”。

**Windows 支持**：64 位 Windows 10 1809+ / Windows 11。安装位置**必须**保持 `C:\Program Files\Qingjian`（装到别处时候选窗会被开始菜单/任务栏搜索框压住）。**测试版无正式代码签名**，SmartScreen 会拦；已知问题明确写着“开始菜单、任务栏搜索框、设置等系统界面中候选窗可能被遮挡，签名版将解决”。

**功能**：全拼、整句、简拼、拼写纠错、双拼、模糊音、大千注音、五笔与五笔拼音混输、自制双拼/形码、辅码、字根图、本地整句模型、英文模式、按应用记住中英模式、符号面板、繁体输出、自定义短语、导入拼音/五笔词库。差异化是“**候选旁的译词**”（英语/日语/西语/越南语/德语可学）与学习统计。

**AI / 云（隐私设计最清晰的开源样本之一）**

- 全部转换、词库、学习、释义本地完成；无账号、无统计上报。
- **云联想默认关闭**；开启后把拼音、光标前后几十字上下文、本地候选**直接发给你自填的 AI 服务商**（默认 DeepSeek，可换成任意 OpenAI 兼容接口），**不经过青简服务器**（青简没有自己的服务器）。
- 密码框（Secure Input）**绝不发送**；标记为私密的输入框不联想。
- 请求节奏可调（停键 300ms、最短间隔 3000ms），同样的输入复用上次结果不重复联网；密钥无效时暂停 10 分钟。
- 一处诚实的“减分项”：**测试版默认记录本地输入日志**（每次上屏的按键、切分、候选、选了第几个），只写本机，可关可清。

**判断**：星数（7,617）与下载量（约 17 万）在两个月内同时很高，是 2026 年最显著的“新项目热度”。但它是**测试版**：无签名、安装路径受限、系统界面候选窗被遮挡。这恰好说明“产品化最后一公里”比算法更难。

### 5.3 清风输入法 WindInput

| 项 | 内容 |
|---|---|
| 类型 | Windows 前端（C++ TSF）+ Rust 跨平台核心 + Rust 安装器 |
| 仓库 | [huanfeng/WindInput](https://github.com/huanfeng/WindInput)、官网 <https://windinput.com> |
| 许可证 | **GPL-3.0-or-later（自 v0.127.0 起；此前为 MIT）** |
| Stars | **1,413**（创建 2026-01-21） |
| 最新版 | v0.130.1，2026-11-04（28 个稳定版，全年高频） |
| 累计 Release 下载 | 约 **12,483** 次 |
| Open issues | 约 44 |

**架构**：`wind_input`（Rust：引擎、词库、候选管理、UI 渲染、IPC）+ `wind_tsf`（C++ TSF）+ `wind_macos`（Swift IMKit）+ `wind_linux`（Fcitx5 addon，预览）；配套 `wind-installer`（Rust 安装器）、`wind-portable`（便携启动器）、`wind-ui-rust`、`wind-setting`，均独立仓库、可缺省。

**产品特色（对 Silk 最直接可抄的部分）**

- **方案驱动 + 四种引擎类型**：拼音（全拼/双拼）、码表、混输（码表优先、拼音兜底）、英文。方案文件是 `*.schema.toml`（`%APPDATA%\WindInput\schemas\`），引擎行为是**全局配置**（`config.toml` 的 `[schema.codetable]` 等），方案文件只写固定参数——这条“行为全局 / 参数随方案”的分工比 Rime 的 patch 覆写清晰得多。
- **四层词库叠加**：系统词库（只读 `.dict.yaml`，分主词库/可开关的扩展词库）、用户词库（`user.dict.yaml`）、临时词库（自动造词暂存，用满次数转正）、命令短语（跨方案共享）。**词频与候选调整是两个独立维度**：词频不修改候选 weight，删除即精确回落；候选调整是“钉死位次/隐藏”的硬指令。
- **按应用兼容规则 `compat.toml`**：三层合并（系统层 / 定制层 / 用户层），字段级叠加，支持 `unset`、`disabled`、按窗口类名（`Chrome_WidgetWin_*`）匹配、候选窗首显策略、应用独立初始中英状态、`host_render` 宿主渲染。这是我在所有开源 IME 里见到的**最完整的宿主兼容工程化方案**。
- **数据分层**：`%APPDATA%`（漫游：配置/词库/主题）+ `%LOCALAPPDATA%`（本机：编译缓存/日志）+ 安装目录 `data\`（只读出厂）。同名文件覆盖，仅 `config.toml`/`compat.toml` 逐项合并。
- 标准安装 / 便捷模式（不改系统 + 便携启动器）、命令直通车、软键盘、Unicode 输入、计算器、主题市场与主题编辑器。

**Windows 支持与发布工程**：Windows 10/11 64 位；**0.121 起程序与安装包已做数字签名（个人代码签名证书）**，因此 SmartScreen 提示会显示发布者名称；仍提示“新版本刚发布、信誉不足时可能被拦”。已知问题坦诚列出：开始菜单候选框不支持鼠标（UIAccess 未全部满足）、部分反作弊严格的游戏不加载 IME、升级必须重启电脑（DLL 被占用）。

**隐私与联网**：无账号、无云输入功能；文档与下载页都未列任何把输入内容外发的功能。但两点需要如实记录：（1）工作区 `Cargo.toml` 含 `axum = "0.7"` + `tower-http`，注释写明是「HTTP server（core 内嵌 web API）」，即核心进程内含一个**本地** HTTP 服务（用于设置/主题站联动或 CLI），本地监听不等于出网，但应视为攻击面；（2）**未见独立隐私说明文档**，出网行为（更新检查的具体实现）未在文档中说明——列为待核实项。

### 5.4 其他值得记录的 Windows 项目

| 项目 | 类型 | 许可 | 最新 | Stars | 累计下载 | 说明 |
|---|---|---|---|---:|---:|---|
| [墨奇 moqi-im-windows](https://github.com/gaboolic/moqi-im-windows) | 前端（TSF，C++） | **MIT** | 1.0.5，2026-08-18 | 386 | 5,995 | TSF 层用 libIME2（参考 PIME/Weasel），引擎 `moqi-ime` 用 **Go**，内置白霜拼音，托盘可切主题/方案，改 librime 组句算法输出多条整句；支持 WebDAV 词库同步与云剪贴板（默认关）；**原生 AI：整句优化/翻译/问答，靠用户自填 api key**。2026-08 后无新提交 → 观察是否停更 |
| [Cassotis 言泉](https://github.com/shenmin/cassotis-ime) | 前端（Delphi/TSF） | GPL-3.0 | v2.6.0，2026-11-02 | 302 | 3,559 | 实验性拼音，DLL x64+x86、Server 仅 x64；**候选排序全部本地 CPU**，自训字级语言模型 + 语料训练排序器；主打英文识别（`pro`/`markdown`/`Effie` 优先出现在数字候选前）；58 个 release、几乎每周一发 |
| [DIME](https://github.com/jrywu/DIME) | 前端（原生 C++/TSF） | BSD-3-Clause | v1.3.644，2026-05-08 | 130 | 2,100 | 繁体码表框架（大易/行列/注音/自建 `.cin`），**单安装包支持 x86/x64/ARM64**，同时支持桌面与传统 Store 应用；支持 TSF 标准反查；**明确“不学习输入内容、不联网”**；100% 原生无第三方二进制 |
| [windows-chewing-tsf 新酷音](https://github.com/chewing/windows-chewing-tsf) | 前端（Rust TSF + libchewing） | GPL-3.0-or-later | 稳定 v26.7.2.0（2026-07-21）；nightly 2026-11-04 | 251 | 23,881 | 繁体注音；**开发已迁移到 Codeberg**（<https://codeberg.org/chewing/windows-chewing-tsf>），GitHub 仅镜像/发布；110 个 release，nightly 频繁 |
| [XSIME 犀术](https://github.com/scholay-xsime/xsime) | 前端（原生 TSF `RimesTsf.dll` + `RimesBroker.exe` 持 librime） | **Apache-2.0** | 1.2.0，2026-10-27 | 约 1,189 | — | 五端（macOS/Win/Linux/Android/iOS），**Windows EXE 未签名**；TSF DLL 保持极薄、Broker 持唯一 librime 实例并支持崩溃后重连；产品取向激进（Buffer 缓冲工作台、Capsule 剪贴板、Mailbox AI 会话），已超出“输入法”范畴 |
| [0C 靈犀注音](https://github.com/0c-biggo/0c-ime) | 前端（Rust TSF）+ Go 引擎 | **AGPL-3.0** | 0.2.8，2026-10-28（GitHub 仅源码 tar.xz，安装包在官网） | 161 | 未计入（无二进制资产） | 繁体注音；本机 Transformer 重排选字（8 层/512 宽/约 3261 万参数，模型合计约 250MB），**P95 延迟约 7–16ms**，模型不可用时自动退回统计引擎；Windows 需 10 1803+ x64（暂不支持 ARM64）；自述评测首选错误率从新酷音 35.8% 降到 14.0% |
| [Yong 小小输入法](http://yong.dgod.net/) | 前端 + 自研引擎 | 仓库未提供 LICENSE 文件（**许可不明**） | **3.2.0，2026-05-02** | 194（仅 GitHub 镜像） | 无 GitHub Release | 官网论坛仍活跃（近 1.4 万帖、11-01 仍有回复）；3.2.0 自述“最后一个支持 XP 和 32 位的版本”，优化 64 位内存占用、改善辅助码与拼音加载速度。**Windows 集成方式（TSF/IMM32）未能从公开仓库核实**，仓库以 Linux/Android 源码为主 |

---

## 6. 横向对比

### 6.1 Windows 平台能力

| 项目 | 最低系统（官方） | 架构 | TSF | 代码签名 | 更新方式 | 安装是否需要重启 |
|---|---|---|---|---|---|---|
| Weasel | Win 8.1–11 | x64（仓库含 ARM64X 工程） | 是 | 未见签名说明 | 内置 WinSparkle（appcast）+ 托盘手动检查 | 未明示（宿主内 DLL 占用，重开程序/重启） |
| PIME | 未明示（TSF 需 Vista+） | x86 / x64 / **ARM64X** | 是 | 未见 | 手动下载安装包 | 占用文件重启后删 |
| 水杉 | Win 10/11 | x64（需 VC++ 2015–2022 x64） | 是（纯 TSF） | **代码签名（Certum）** | 手动检查（设置→关于）+ 官网 | 未明示 |
| 青简 | Win 10 1809+ / 11 | x64 | 是 | **无签名（测试版）** | 官网/GitHub + 设置内检查 | 已开程序需重开 |
| 清风 | Win 10/11 64 位 | x64 | 是 | **个人代码签名证书（0.121+）** | 手动安装包 | **官方要求重启** |
| 墨奇 | 未明示 | 未明示 | 是（libIME2） | 未见 | 手动 | 未明示 |
| Cassotis | Win 10/11 | DLL x64+x86，Server x64 | 是 | 未见 | 手动 | 未明示 |
| DIME | Win 8/10/11 | **x86/x64/ARM64** | 是 | 未见 | 手动（MSI/Universal） | 未明示 |
| windows-chewing-tsf | 未明示 | MSI（架构未逐一核实） | 是（Rust） | 未见 | nightly + 手动 | 未明示 |
| XSIME | x64 系统（含 x64/x86 TSF） | x64 + x86 TSF | 是 | **EXE 未签名** | 设置内检查更新 | 未明示 |
| 0C | Win 10 1803+ x64 | x64（**不含 ARM64**） | 是（Rust，含 32 位 DLL） | 未见 | 手动/setup | 不强制结束旧引擎、不必登出 |

**在本轮核实的官方文档里，只有清风与水杉明确把“签名”写进发布说明**（清风为个人代码签名证书，水杉为自签名）；青简、XSIME 明确承认未签名并把它列为已知问题；Weasel、PIME、墨奇、Cassotis、DIME、新酷音、0C 的公开文档里没有签名承诺——未写不等于未签，但至少不是对外承诺。这是 Windows 输入法最常见的信任缺口。

### 6.2 功能与取向

| 项目 | 输入方式 | AI | 云/联网默认 | 账号 | 图形化设置 | 主要短板 |
|---|---|---|---|---|---|---|
| Weasel | 全拼/注音/仓颉（五笔等需 plum） | 无（Lua 可扩展） | 内置 WinSparkle 更新检查（**[INFERENCE]** 默认自动） | 无 | 极简（部署器） | 门槛高、稳定版停滞 |
| PIME | 繁体码表谱系 + Rime 桥接 | 无 | 取决于插件 | 无 | 有（简单） | 长期 beta、安全面大 |
| 水杉 | 全拼/双拼/五笔/辅助码/日文 | **有**（联句、翻译、语音） | **云候选默认开（Google）** | 无 | 完整原生设置 | 自签名、token 明文、默认联网 |
| 青简 | 全拼/整句/双拼/注音/五笔/辅码 | **有**（云联想、整句补全、问字） | **默认关**，用自填服务商 | 无 | 完整 | 无签名、装路径受限 |
| 清风 | 全拼/双拼/五笔/混输 | 无 | 未见云功能；有内嵌本地 web API（细节未公开） | 无 | **完整 + 按应用规则** | 签名仅个人证书、无隐私文档 |
| 墨奇 | Rime 方案（内置白霜） | **有**（自填 key） | 默认关（WebDAV 需自配） | 无（WebDAV） | 托盘菜单 + JSON | 2026-08 后无更新 |
| Cassotis | 拼音（字级 LM 排序） | 无 | 本地 | 无 | 有 | 实验性、生态小 |
| DIME | 大易/行列/注音/自建 cin | 无 | **完全离线** | 无 | 有 | 仅繁体码表谱系 |
| 0C | 注音（Transformer 选字） | 本机小型 Transformer | **完全离线** | 无 | 有（Tauri） | 仅繁体注音、AGPL、无 ARM64 |
| XSIME | 全拼/双拼/五笔/并击/英文 | **有**（多连接器） | 功能取向偏联网 | 本地为主 | 完整 | 范围膨胀、未签名 |

---

## 7. 可量化热度（快照 2026-11-06）与解读

| 项目 | Stars | 累计 Release 下载 | 最新稳定版 | 距今 |
|---|---:|---:|---|---|
| Weasel | 8,263 | **1,389,210** | 0.17.4（2025-06-04） | 约 17 个月 |
| 青简 | 7,617 | 169,710 | v0.3.2（2026-10-30） | 7 天 |
| librime（引擎） | 4,711 | 106k | 1.17.0（2026-06-06） | 5 个月 |
| 水杉（Windows） | 3,359 | 44,959 | v0.13.2（2026-11-04） | 2 天 |
| PIME | 1,530 | 459,890 | 1.3.0-stable（2023-01-20） | 约 3 年 10 个月 |
| 清风 | 1,413 | 12,483 | v0.130.1（2026-11-04） | 2 天 |
| XSIME | ~1,189 | — | 1.2.0（2026-10-27） | 10 天 |
| 墨奇 | 386 | 5,995 | 1.0.5（2026-08-18） | 约 3 个月 |
| Cassotis | 302 | 3,559 | v2.6.0（2026-11-02） | 4 天 |
| windows-chewing-tsf | 251 | 23,881 | v26.7.2.0（2026-07-21） | 约 3.5 个月 |
| 0C | 161 | 未计入（Release 仅源码包） | 0.2.8（2026-10-28） | 9 天 |
| DIME | 130 | 2,100 | v1.3.644（2026-05-08） | 6 个月 |
| Yong 小小 | 194 | 无 GitHub Release | 3.2.0（2026-05-02） | 6 个月 |

**必须一起读的三条限制**

1. **下载计数 ≠ 用户数。** 包含重复下载、CI、镜像抓取、好奇试用；安装包体积大时（青简 Windows 安装包 84.8MB）下载与实际使用的偏差更大。Weasel 的 139 万次是 11 个版本累计、跨度 5 年以上，不能推出“139 万活跃用户”。
2. **stars 与下载量比例在不同项目间差异极大。** 青简 7,617★ / 17 万下载 ≈ 1:22；Weasel 8,263★ / 139 万下载 ≈ 1:168。前者是高关注度的新项目，后者是长周期分发的成熟工具。**用 stars 排名会得出与分发量相反的结论**——这正是“stars 不能当市场份额”的实证。
3. **Open issues 与项目健康度不是单调关系。** PIME 345 个未关 Issue 配上 3 年 10 个月无稳定版，是积压；水杉 341 个未关 Issue 配上 29 个稳定版与 2 天前的发布，是高吞吐下的待办。两者不能同等看待。

---

## 8. 共性痛点（Windows 平台工程的真实成本）

以下每一条都能在至少两个项目的官方文档里找到明确记载，属于**可复用的风险清单**：

1. **代码签名与信誉。** 无签名 → SmartScreen “未知发布者” + 杀软启发式报毒。清风与青简都解释了根因：IME 必须把 DLL 注入其他进程并注册 TSF 组件，行为特征与恶意软件高度相似。清风 0.121 起用个人代码签名证书，提示里才出现发布者名；青简明确写“此问题将在签名版中解决”。**签名是准入成本，不是可选项。**
2. **TSF DLL 被锁 → 升级必须重启。** 清风文档写得最清楚：DLL 被加载进“每一个使用输入法的进程”，Windows 不允许替换被占用的 DLL，新旧混用会表现为不生效、候选窗不出、输入错乱甚至崩溃。青简的绕法是在 Server 侧持有引擎、TSF 侧尽量薄，但仍需“重开程序”。
3. **候选窗的窗口层级（UIAccess）。** 只有取得 UIAccess 的窗口才能盖在开始菜单之上，而该权限要求签名 + 清单声明 + 安装在受信任目录。青简的已知问题（候选窗被系统界面遮挡）与清风的“开始菜单候选框不支持鼠标”是同一个根因，两者都用“宿主进程代理渲染”做缓解。
4. **游戏与反作弊。** 判据严格的游戏会拒绝加载第三方 IME；清风 0.121 通过把 TSF 组件装进系统目录 + 签名，才让 CS2 一类游戏放行。
5. **按应用差异化是刚需。** 候选窗定位、首显策略、初始中英状态、是否强制英文（密码框）在不同宿主里表现不同。清风的 `compat.toml` 三层合并 + 窗口类名匹配是当前最成熟的解法；Rime/Weasel 用户长期靠手工 patch 与 issue 里的“这个程序要用这个设置”维持。
6. **安装/升级的“部署”语义。** Rime 需要“重新部署”，青简/墨奇需要“重开程序”，清风需要“重启电脑”。对普通用户，这三者都是理解成本。
7. **bus factor。** Weasel 的提交集中在少数维护者；Yong 依赖单一作者；Cassotis、0C、墨奇都是单人主导项目。选型时不能只看 star。
8. **“开源 ≠ 不联网”。** 水杉云候选默认开、PIME 可跑任意 Python/JS、Rime 有 WinSparkle 自动更新检查。隐私承诺必须落到**具体默认配置**上才可信。

---

## 9. 对 Silk IME 的启示

以下是基于上述证据的推断，不是市场事实。

### 9.1 架构

1. **进程结构沿用“Weasel / 青简 / 0c”三家共识，并且可以放心用 Go。** 三家独立收敛到同一形状：TSF DLL（宿主内，只做按键转发 + 文档写入 + 画候选）+ 命名管道 + 单例 Server 进程（持有唯一引擎）。墨奇（`moqi-ime`）与 0c（`0c-server`，`cmd/0c-server` 用 `go build`）证明 **Go 写引擎服务、原生 C++/Rust 写 TSF 壳**是可行的生产组合——正是 Silk 的方向。青简的 crate 边界论证值得直接采纳为模块纪律：**“DLL 不含 Engine”要由构建单元强制，而不是靠自觉**。
2. **候选窗自绘 + 宿主代理渲染是必须做的两层。** 只在 Server 侧画窗会盖不住开始菜单（青简）；只在宿主内画会受限于宿主进程（Weasel 的绘制崩溃史）。清风的 `host_render` 开关说明工业级做法是“默认外部窗口，个别宿主切代理渲染”。
3. **按应用兼容规则要在第一版就做成数据而不是代码。** 清风的 `compat.toml`（字段级三层合并 + `unset` + 窗口类名匹配 + 容错到单字段降级）是现成的最佳实践；如果 Silk 一开始就把“某程序特殊处理”写成 `if (process == "Weixin.exe")`，半年后会无法维护。
4. **配置分权：行为全局、参数随方案。** 清风把“上屏策略/调频/模糊音”放在全局 `config.toml`，方案文件只写码长、词库列表、双拼布局。对比 Rime 的 YAML + patch 覆写（改错一个缩进就部署失败），这条边界让“改行为”和“加方案”互不干扰，也更容易做 GUI。
5. **词库分层 = 系统（只读）/ 用户 / 临时 / 短语，且词频与候选调整分离。** 清风的四层与“词频不改 weight、可精确回落”的设计，比“把词频直接加进权重”更容易解释、也更容易撤销。词库格式用 Rime 的 `.dict.yaml` 可直接吃下雾凇/白霜/万象生态（清风、墨奇都这么做），这是低成本获得词库供给的杠杆。
6. **不要内嵌通用语言运行时做扩展。** PIME 的 beta4 紧急安全更新（Python/Node 及依赖的多项 CVE、RCE）就是这条路的账单。Silk 若需要扩展性，宜用声明式配置 + 受控协议。
7. **许可证可复用面**：librime BSD-3、libIME2 LGPL-2.1、XSIME Apache-2.0 可以安全借鉴/链接到 GPL-3.0 的 Silk；**Weasel/PIME 主体（GPL-3.0/LGPL-2.1）与 0c（AGPL-3.0）要按各自条款处理**，AGPL 对网络服务有额外义务，代码级借鉴需谨慎。词库（雾凇/白霜 GPL-3.0、万象 CC-BY-4.0）必须在 NOTICE 逐项标注来源与许可。

### 9.2 产品

1. **稳定发布节奏本身就是差异化。** Weasel 17 个月无稳定版、PIME 3 年 10 个月无稳定版，而清风与水杉均在 2026 年保持高频发布。用户在 nightly 与旧稳定版之间二选一的体验，是 Silk 可以直接赢的地方。
2. **签名 + 安装即用 = 入场券。** 青简、XSIME 把“未签名”写成已知问题；清风用个人证书换取 SmartScreen 显示发布者。Silk 应在首个可分发版本就有签名、校验和、明确的发布渠道，而不是等到“稳定版”。
3. **AI 的正确位置是“候选增强 + 可关 + 不用自家服务器”。** 青简的模型（默认关、发给你自填的服务商、密码框绝不发送、可调请求节奏、同样的输入不重复联网、失败降级不画占位）比水杉（云候选默认开、发 Google）更适合作为 Silk 的参照。墨奇的“把 AI 动作写成带 `{{candidate_1}}` 模板的 JSON 配置 + 用户自填 key”也是低成本做法。
4. **隐私要写成“逐条可核对的文档”。** 水杉的 PRIVACY.md 是优秀结构（每个功能 → 发什么、发到哪个 endpoint、文件与键名、怎么关），但它同时暴露了“默认开”的代价。Silk 应做到“默认全关 + 同样的可核对粒度”，并补上水杉的短板：**凭据不能明文落盘**（Windows 用 DPAPI/凭据管理器）。
5. **卸载与备份是产品的一部分。** 水杉卸载会删除整个本地目录（含用户词库），清风提供备份/还原与“卸载时是否删除数据”。输入法持有的是用户长期积累的词频，误删等于数据丢失。
6. **不要做“又一个 Rime 前端”。** 雾凇 20,050★ 说明用户要的是“开箱即用的结果”，不是“可配置的框架”。Silk 的价值在于把方案选择、部署、重启、宿主兼容这些内部机制藏起来，同时对愿意深入的用户保留文件式可检查性（清风的数据目录分层是很好的折中）。
7. **暂不做**：语音（水杉的语音依赖云 ASR token）、皮肤商店（清风有 market/theme 站，是独立产品线）、脚本插件（PIME 的教训）、多端同步（墨奇的 WebDAV 与青简的无账号策略说明这块需求真实但成本高）。

---

## 10. 排除项（热门但无当前 Windows 版，或证据不足）

只说明为什么不纳入对比，不参与打分：

- **Trime 同文**（Android，GPL-3.0）：Rime 的 Android 前端，无 Windows 版。
- **Squirrel 鼠须管**（macOS，GPL-3.0）：Rime 的 macOS 前端，无 Windows 版。Weasel 的稳定版停滞不能由 Squirrel 的节奏替代解释。
- **Fcitx5 / fcitx5-rime / ibus-rime**（Linux/BSD）：Windows 上无官方前端。
- **OpenVanilla / McBopomofo 小麥注音**（macOS，MIT）：繁体 macOS 谱系，无 Windows 版。
- **Mozc**（日语引擎，BSD-3）：官方 Windows 支持已不是主路径，且非中文输入法。
- **HeliBoard / FlorisBoard / FUTO Keyboard**（Android）：无 Windows 版。
- **TypeDuck 粤拼**：未能核实到可访问的官方仓库（检索 2026-11-06 时 `TypeDuck-HK/TypeDuck` 返回 404），也未找到 Windows 版证据，故不纳入。
- **微软拼音/五笔、搜狗、QQ 输入法**：闭源商业或系统内置，见商业输入法报告与总览报告。

---

## 11. 未核实项与后续可查清单

明确标注，避免把推断当事实：

| 项 | 状态 |
|---|---|
| Weasel 是否默认自动检查更新（WinSparkle 行为） | **[INFERENCE]** 源码只配置了 registry path 与语言，未显式关闭自动检查；WinSparkle 默认按周期自动检查。需实测网络行为确认 |
| Weasel 是否随安装包分发 ARM64 构建 | **[INFERENCE]** 仓库有 `arm64x_wrapper/`，下载页未单列 ARM64 包 |
| Weasel 当前安装包是否已代码签名 | 未在 README/下载页找到签名说明；官方仅提示核对 checksum |
| windows-chewing-tsf 的架构覆盖（是否含 ARM64） | 未逐一核实 MSI 内容 |
| 水杉 Windows 版到底有无遥测 | **官方自相矛盾**：`msime-windows/PRIVACY.md` 称无任何遥测，跨平台仓 `msime` 的 README 称六平台默认上报匿名统计到 `api.msime.app`。需实测或向作者确认 |
| 水杉云候选在升级安装时的默认值 | 文档称首次安装走向导页（可取消勾选）、“升级会跳过该页，因为配置已归用户”，但 `config.default.toml` 与 `ime_config.cpp` 的缺省仍是 `true`。升级用户的真实默认值未核实 |
| 各项目“活跃用户”规模 | **不存在可信公开数据**；本文一律不填写 |
| 清风是否有自动更新检查、内嵌 web API 是否只监听本地 | 工作区引入 `axum`/`tower-http` 并注明「core 内嵌 web API」；文档未说明监听地址与鉴权。需实测端口绑定与是否有出网请求 |
| 0C 在 Windows 上的下载量 | 版本号已核实为 0.2.8（2026-10-28）；GitHub Release 只提供源码 `tar.xz` 与模型包，Windows `setup.exe` 走官网 `0c.biggo.com/download`，无公开计数 |

后续若需要更硬的采用数据，可行方向：**包管理器清单**——实测 **Weasel 已进入 winget 官方仓库**（`manifests/r/Rime/Weasel/`，含 0.17.4 与 en-US/zh-Hans/zh-Hant locale 清单，<https://github.com/microsoft/winget-pkgs/tree/master/manifests/r/Rime/Weasel>），而 PIME、水杉、青简、清风、Cassotis、墨奇、新酷音在 `winget-pkgs` 中逐个查询均无 manifest（2026-11-06 返回 404）。这说明**除 Weasel 外，2026 年的新项目几乎都没进系统级包管理器**：既是分发渠道缺口，也意味着它们的下载计数几乎全部来自 GitHub Releases 与官网，缺少独立第三方口径。winget 本身不公开安装计数，故不能反推用户量。其他方向：Scoop/Chocolatey 桶、官网下载统计（通常不公开）、社区问卷。
