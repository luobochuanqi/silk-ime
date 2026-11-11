# Windows 输入法候选窗 UI 绘制方案调研（2026）

> 调研日期：2026-11-10。为 [design/foundation/06-ui.md](../design/foundation/06-ui.md) 的渲染选型提供证据。来源标注：`竞品调研` = 本库 [windows-ime-research-2026.md](windows-ime-research-2026.md) 等四份 2026-11-06 快照报告；`官方` = Microsoft Learn / Skia / 项目源码，随文标注文档名。

## 结论（先看这里）

**主路**：`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 独立 popup + `CreateSwapChainForHwnd`（flip）+ Direct2D DeviceContext + DirectWrite，**不透明背景**，圆角/阴影/深色边框交给 DWM（`DWMWCP_ROUNDSMALL` 等）。
**兜底路**：同一 `silkui` 库的第二后端 = `ID2D1DCRenderTarget` + 预乘 alpha DIB + `UpdateLayeredWindow`（无 GPU 依赖，接受灰度 AA 与自绘圆角阴影）。

决定性理由：

1. **ClearType 只在不透明目标生效**（`D2D1_ALPHA_MODE_IGNORE`；带 alpha 的目标文字抗锯齿自动降灰度）——官方 `D2D1_TEXT_ANTIALIAS_MODE` Remarks、`Supported Pixel Formats and Alpha Modes`。候选窗要"圆角+投影+半透明"就与 ClearType 互斥，除非文字区不透明。
2. **per-pixel alpha 分层窗口"cannot ever be rounded"**（官方 `apply-rounded-corners`："Apps that cannot ever be rounded"）——`UpdateLayeredWindow` 主路拿不到系统圆角/阴影，只能自绘；而自绘=半透明=灰度文本，回到理由 1 的死结。
3. **flip 交换链 + DWM 是官方为弹出窗点名的路径**：`DWMWCP_ROUNDSMALL` 的官方示例就是"自定义菜单/弹出窗"；合成器直取、零拷贝，且与 ClearType 兼容。
4. **三段式是行业反复验证的形态**：Weasel 2018 年就提出"迁回前端"兜底（issue #110）但八年未实现；青简/MSIME 走通"进程外 + UIAccess"，WindInput 走通"进程外 + 宿主内兜底"。本设计的默认路 + `host_render` 兜底 + UIAccess 可选增强，正是这三段的合集。

## 渲染技术对比

| 方案 | CJK 文本质量 | 体积/运行时 | GPU 依赖与 TDR | Rust 绑定 | 结论 |
| --- | --- | --- | --- | --- | --- |
| Direct2D + DirectWrite | ClearType（不透明目标）；`DWRITE_RENDERING_MODE_NATURAL` 亚像素定位；系统字形回退（`IDWriteFontFallback`，Win8.1+） | 无额外运行时（d2d1/dwrite 系统自带） | 可软件回退（`ID2D1DCRenderTarget`）；`D2DERR_RECREATE_TARGET` 恢复契约明确 | `windows` 0.62.x 全绑定齐备 | **采用** |
| Skia (skia-safe) | 亚像素 AA 但"requests, does not require"；字体回退要自接 `SkFontMgr` | +textlayout 21.2 MB（0.157.0 release 实测） | 自管上下文/重建，无系统恢复契约 | 成熟但重 | 弃：体积换不到更好的系统 CJK 回退 |
| GDI / GDI+ | ClearType 参考实现；但无 per-pixel alpha | 零 | 无 GPU | 有 | 弃：圆角/阴影/半透明全废，高 DPI 发虚 |
| WebView2 | Chromium 文本栈，与系统度量不一致 | WebView2 Runtime + 浏览器进程组 | 独立 GPU 进程 | `webview2-com` | 弃：TSF DLL 内不可行（进程组 + app-container 约束） |
| XAML / WinUI Islands | 官方明言 Visual Layer 无文字图元，仍需 DirectWrite | Windows App SDK 运行时 | 同 D3D | 需 C++/WinRT 垫层 | 弃：纯负担无收益 |
| 纯软件光栅（tiny-skia/softbuffer） | tiny-skia 官方 README："The main missing feature is text rendering" | ≈200 KiB | 零 GPU | 成熟 | 弃：CJK 文字栈全自研；至多作第三兜底 |

## 窗口合成路径对比

| 维度 | UpdateLayeredWindow | 普通 HWND + flip 交换链 + DWM | DirectComposition |
| --- | --- | --- | --- |
| 更新粒度 | 官方：总是整窗更新（`UpdateLayeredWindow` Remarks），建议窗口尽量小 | DWM 重定向位图，flip 模型 | 交换链直进合成器，零拷贝、独立合成线程 |
| 圆角/阴影 | **永远没有系统圆角**（per-pixel alpha 除外清单） | 系统白送（`DWMWCP_ROUNDSMALL`） | 只有矩形裁剪，圆角自绘 |
| ClearType | 灰度 AA（alpha DIB） | 不透明文字区可 ClearType | 自绘圆角=半透明=灰度 |
| GPU 重置 | 无 GPU 依赖，最健壮 | `D2DERR_RECREATE_TARGET` → 重建重画 | `CheckDeviceState` + 全量重建（`WM_PAINT` 契约） |
| 定位 | — | — | **主路弃选**：为延迟换掉 ClearType 不划算 |

## 微软官方现行要求（候选窗相关）

来源：官方 `Input Method Editor (IME) requirements`、`TSF UIless mode overview`、`DXGI overview`、`apply-rounded-corners`。

- **owned window**：候选窗所有者取 `ITfContextView::GetWnd`（失败回退 `GetFocus`）。
- **light dismiss**：显隐必须 `NotifyWinEvent(EVENT_OBJECT_IME_SHOW/HIDE/CHANGE)`；禁止 sticky 窗/全屏窗。
- **UIElement 登记是硬要求**：实现 `ITfTextInputProcessorEx`，显示任何 UI 前 `ITfUIElementMgr::BeginUIElement`（候选列表实现 `ITfCandidateListUIElement`）——这是游戏/全屏应用接受 IME UI、light dismiss、触摸键盘策略的接入点。
- **DPI**：IME 不得自行声明 DPI 感知等级（跑在宿主进程等级上），只做响应。
- **触摸键盘**：`IFrameworkInputPane` 拿占位与显隐事件做回避；翻页键经 `VK_PACKET` + `ToUnicode`（0xF003/0xF004）。
- **DXGI**：不得在 `DllMain` 创建/释放 DXGI 工厂或设备——进程内渲染栈初始化必须延后。

## 实践先例（项目 × 渲染技术 × 窗口手法）

| 项目 | 候选窗绘制 | 窗口/部署手法 | 来源 |
| --- | --- | --- | --- |
| 小狼毫 Weasel | WeaselUI/WeaselPanel 自绘，DirectWrite 文本 | 候选窗由 `WeaselServer` 进程渲染（进程外路） | 竞品调研 |
| 水杉 MSIME | 自研原生框架（Win32 + Direct2D/DirectWrite）；生产分层：`EnsureForComposition` 失败走 DC render target + 预乘 DIB + `UpdateLayeredWindow`（二手证据） | Server 进程渲染 | 竞品调研 |
| 清风 WindInput | 原生渲染 + `host_render` 开关做宿主代理渲染兜底 | 双路：进程外默认 + 宿主进程内兜底 | 竞品调研 |
| 青简 qingjian | TSF DLL 内绘候选窗（Server 回候选、DLL 绘制） | 进程内路；未上 UIAccess | 竞品调研 |
| XSIME | 同青简形态 | 进程内路；未上 UIAccess | 竞品调研 |

行业收敛观察：

- **单路径都有真实缺陷**：只在 server 进程画窗，未启用 UIAccess 时被开始菜单/搜索面板盖住（青简/XSIME 已知问题）；只在宿主进程内画，图形栈代码进入所有进程（Weasel 有绘制崩溃史）。
- **UIAccess 的代价**：候选窗盖过系统 UI 需签名 + uiAccess 清单 + 受信任目录（即强制 Program Files 安装），青简因此把安装路径锁死在 `C:\Program Files\Qingjian`。
- **GPU 重置是真实故障**：Weasel 曾修复"显卡重置后绘制失败"，任何 GPU 渲染路径都必须有渲染目标重建逻辑。
- **进程外渲染的原始权衡记录**：Weasel 2018 年 issue #110「將候選詞界面遷移至前端」是这笔交易的一手决策记录——lotem 明确列出进程外收益（"介面代碼只須編譯一次；介面只加載到內存一份；前端穩定，倘介面出錯可保應用程序不垮"）与代价（"兼容性風險++"）。此后八年代价逐项兑现为 issue：#150 遮挡、#100 高 DPI 坐标偏移（JetBrains 系）、#1445 切应用丢候选、#1672 定位错误。"迁回前端"兜底路 Weasel 自己提出但八年未实现（`WeaselUI` 至今由 `WeaselServer` 持有）——行业在 2024–2026 年重做这笔交易，结论一致：接受，再用 UIAccess/band 补层级短板。
- **游戏按 IME 身份过滤 UI**：部分游戏（英雄联盟、Dota 2）对候选窗做针对性行为/白名单判断（WindInput `game-compat-tsf-uielement.md` 记录）——印证"宿主内兜底路"与官方 UIElement 登记的必要性。

## 待 spike 验证（已并入 06 开放验证项 O-6…O-11）

DWM 圆角在无边框 popup 的实际表现（O-6）；进程内 D3D 初始化的宿主兼容性与冷启耗时（O-7）；灰度 AA 的 CJK 观感阈值与 clip+Clear 补救副作用（O-8）；生僻字回退覆盖（O-9）；TDR 端到端恢复质量（O-10）；兜底路单帧成本（O-11）。

**未覆盖**（调研收束时跳过）：XAML Islands 在 Rust 下的具体绑定路径、Win2D 桌面可编译性、GDI+ 与 DWrite 混排度量一致性、DirectComposition 与硬件叠加层的功耗差异、WebView2 负向实验记录、fcitx5-windows 的 Windows 候选 UI 实现、微软 `TextInputHost.exe` 的 manifest 内容、Weasel 引入 D2D/DComp 的具体版本。
