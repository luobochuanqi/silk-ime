# 03 Windows 平台适配层（TSF）

本文定义 `silk-tsftip.dll` 的职责边界、注册机制、生命周期与宿主兼容性策略。总体架构见 [02](02-architecture.md)，协议细节见 [04](04-process-and-ipc.md)。

## 平台事实

设计先接受平台给定的事实，再谈方案：

1. **TSF 是唯一路径**。Windows 输入 API 只有 IMM32（遗留）与 TSF 两套；微软官方已明确"系统会阻止通过 IMM32 实现的 IME"，所有新输入法必须以 TSF TIP（Text Input Processor）实现。
2. **TIP 被加载进每个文本输入宿主进程**。CTF 子系统按需在应用进程内以 COM in-proc server 方式实例化 TIP——这是"进程内代码面最小化"（ADR-1）的根因。
3. **传统 IMM 应用由系统桥接**（CUAS）。遗留 IMM32 应用通过系统自带桥使用 TSF 输入法，我们不实现 IMM 层。
4. **触摸键盘也承载 TSF IME**，与 IME 用约定的 PUA 字符交互（翻页等），触屏场景自动受益，无需单独实现。
5. **注册是 COM 注册 + TSF 注册两步**（微软官方注册文档）：标准 in-proc server 注册表项，外加 `ITfInputProcessorProfiles` 与 `ITfCategoryMgr` 的 TSF 专属注册。

## 职责边界

| 做 | 不做 |
| --- | --- |
| COM/TSF 对象实现与注册导出 | 输入逻辑、分词、查词、排序 |
| 按键拦截与语义翻译（VK → 协议键事件） | 词库读写 |
| composition 的创建/更新/提交（TSF 要求进程内完成） | 候选内容生成 |
| 插入点度量（`ITfContextView::GetTextExt`） | 默认态的候选绘制（仅 `host_render` 兜底通道） |
| 协议客户端（连接、请求、超时、降级） | 网络、脚本运行时、任何动态代码 |

"不含输入逻辑"用 crate 边界强制：`tsftip` crate 只依赖 `protocol` 与 `silkui`（兜底渲染），不依赖任何引擎 crate（竞品青简的同名约束）。

## 注册与激活

### 注册内容

| 步骤 | 机制 | 写入位置（per-user / per-machine） |
| --- | --- | --- |
| COM in-proc server | CLSID、`InprocServer32`、`ThreadingModel=Apartment` | `HKCU\Software\Classes\CLSID\…` / `HKLM\Software\Classes\CLSID\…` |
| TIP 注册 | `ITfInputProcessorProfiles::Register` + `AddLanguageProfile`（语言 `0x0804`，profile GUID，名称/图标/描述） | `…\Microsoft\CTF\TIP\{CLSID}\LanguageProfile\0x0804\{ProfileGUID}` |
| 类别注册 | `ITfCategoryMgr::RegisterCategory`（`GUID_TFCAT_TIP_KEYBOARD`、`GUID_TFCAT_CATEGORY_OF_TIP`） | `…\CTF\TIP\{CLSID}\Category\…` |

### 作用域

- **per-user（默认）**：全部写 HKCU，无需管理员。用户级注册是平台支持的标准形态（同产品均提供普通用户安装路径）。
- **per-machine（可选）**：写 HKLM，需管理员；与 UIAccess 候选窗模式配套（UIAccess 要求签名 + 清单 + 受信任目录，见 07）。

### 导出接口（ADR-6）

```
SilkRegister(scope)    // scope: user | machine；幂等
SilkUnregister(scope)  // 逆序清理三类注册；幂等
SilkRegisterSelf()     // 供安装后"登记进当前用户键盘列表"的补登记
```

注册后需将 profile 登记进用户键盘列表才能出现在语言切换中（Win+空格）；安装器在提升安装后以**原始用户身份**补登记一次（竞品踩过的坑：提升上下文注册到了管理员账户）。

### 激活感知

实现 `ITfInputProcessorProfileActivationSink` 感知用户切换；TIP 激活本身由 TSF 调 `Activate` 完成，无需我们抢控制权。

## 生命周期与线程模型

```mermaid
sequenceDiagram
  participant CTF as CTF/TSF
  participant TIP as silk-tsftip.dll
  participant HOST as silkhost.exe

  CTF->>TIP: COM 实例化（每线程，Apartment）
  TIP->>TIP: Activate(threadMgr)
  TIP->>HOST: 连接（失败则拉起 host，见 04）
  CTF->>TIP: OnSetFocus(document)
  TIP->>HOST: focus_in(sid, context)
  loop 每次按键
    CTF->>TIP: OnTestKeyDown / OnKeyDown
    TIP->>HOST: key(sid, event)（预算内同步等待）
    HOST-->>TIP: update / commit / caret
    TIP->>CTF: RequestEditSession 更新 composition
    Note over TIP: 候选显隐：默认路 host 绘窗，兜底路 silkui.Show
  end
  CTF->>TIP: OnKillFocus / Deactivate
  TIP->>HOST: focus_out / bye
```

要点：

- **每线程一个 TIP 实例**，实例数 = 活跃文本线程数。实例内状态无共享；跨线程共享状态只存在于 host（会话运行时负责并发模型）。
- **TSF 回调里禁止长阻塞**。所有对 host 的请求有硬超时（默认 15 ms，见 04）；edit session 只做 composition 文本更新，不做业务计算。
- **窗口切换是竞态高发区**（小狼毫"切换/新建窗口时多线程崩溃"的教训）。焦点、文档、会话三者的状态迁移在 shim 内串行化到单一状态机，任何 TSF 回调只投递事件，不直接改共享状态。
- **初始化时机**：D3D/DXGI 工厂与设备不得在 `DllMain` 中创建/释放（DXGI 官方约束）；渲染栈初始化延后到 `ActivateEx`/首次绘窗。

## 按键处理与 composition

### 按键决策

`OnTestKeyDown` 依据**本地缓存的模式状态**（中/英、全/半）决定是否拦截：中文模式下字母/数字/编辑键进引擎路径，其余（Ctrl 组合等）放行。英文模式下字母键仍进引擎做单词补全（可配置关闭）；补全关闭或降级形态为纯直通、零 IPC。模式状态以 host 为权威、shim 缓存最终副本（模式切换本身走一次请求）。

### composition 生命周期

```mermaid
stateDiagram-v2
  [*] --> Idle
  Idle --> Composing: 字母键（StartComposition + 写 preedit）
  Composing --> Composing: 续键/退格（Range 更新）
  Composing --> Idle: 选择上屏 / 空格上屏（SetText + EndComposition）
  Composing --> Idle: Esc（清空 + EndComposition）
  Composing --> Idle: 失焦（策略：原样上屏，见下）
  Idle --> [*]
```

- composition 通过 `ITfInsertAtSelection` + `StartComposition` 建立，preedit 下划线样式用 `GUID_PROP_ATTRIBUTE`。
- 失焦处置策略：**原样上屏已输入字符**（不丢用户输入，与主流 IME 一致）；配置可改为"丢弃"。
- 候选窗由 `silkui` 自绘，但**必须**经 `ITfUIElementMgr::BeginUIElement` 登记（候选列表实现 `ITfCandidateListUIElement`）：这是系统 light dismiss、全屏/游戏抑制、触摸键盘策略的接入点（官方 UIless 模式要求）；语言栏/托盘入口后置。

## 宿主兼容性矩阵

| 宿主类别 | 示例 | 主要风险 | 验证层级 |
| --- | --- | --- | --- |
| Win32 / GDI | 记事本、资源管理器地址栏 | 低，基准 | 自动化 + 手测 |
| DirectWrite 浏览器 | Chrome / Edge / Firefox | `GetTextExt` 语义、页面内嵌编辑器 | 手测清单 |
| 办公套件 | Word / WPS | selection 语义复杂、组合期重排 | 手测清单 |
| 终端 | Windows Terminal / conhost | 光标定位、快速连续输入 | 自动化 + 手测 |
| WinUI / UWP / XAML | 系统设置、Store 应用 | 焦点与坐标语义、悬浮键盘 | 手测清单 |
| 提权进程 | 管理员记事本 | 完整性级别：host 服务需可被高完整性进程连接 | 手测 |
| 远程桌面 | RDP 会话 | 延迟放大、剪贴板/焦点切换 | 手测 |
| 系统 UI | 开始菜单、搜索面板 | z-order：进程外候选窗可能被遮挡 → `host_render` 兜底（06） | 手测 |
| 全屏/游戏 | 反作弊游戏 | **明确不支持**：反作弊拒载第三方 IME | 文档声明 |
| 触摸键盘 | 系统触屏键盘 | PUA 按键约定、布局切换 | 手测 |

## 多架构支持

- 发布 x64 与 arm64 两个构建；x64 模拟进程加载 x64 DLL，arm64 原生进程加载 arm64 DLL。
- **Arm64X 合并包**（单一 DLL 同时服务 x64/arm64 进程）是优化项：MSVC 支持，Rust 工具链支持度待验证（开放项 O-1）。
- host（Go）：x64 与 arm64 原生构建；Go 静态链接，无运行库依赖。

## 开放验证项（M0 spike 清单）

| ID | 事项 | 验证方法 |
| --- | --- | --- |
| O-1 | Rust 产出 Arm64X DLL 的工具链可行性 | 最小 TIP 探针在 x64/arm64 宿主加载 |
| O-2 | per-user 注册在 Win10 21H2+ / Win11 的行为一致性 | 两台干净虚拟机安装探针 |
| O-3 | `GetTextExt` 在 WinUI/终端的坐标语义 | 探针记录坐标与实际插入点偏差 |
| O-4 | 高完整性宿主连中完整性 host 管道 | 探针在提权记事本按键 |
| O-5 | DLL 占用下的升级替换策略（07） | 安装器脚本在开 N 个应用时升级 |

## 需求映射

覆盖 R-1（注册/激活/卸载）、R-2（热路径拦截）、R-3（中英文输入面）、R-5（平台分离）、R-12（多架构）、R-13（兼容矩阵）。
