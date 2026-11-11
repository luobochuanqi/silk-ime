# Windows TSF 输入法注册配方调研（2026）

> 调研日期：2026-11-11。为 `native/tsftip/src/reg.rs` 的改写与设计文档提供一手证据。
> 证据分三类，随文标注：`官方`＝Microsoft Learn 文档（给节名）；`源码`＝真实输入法仓库文件:行号；
> `实测`＝本机（Windows 11 build 26300.9974，简体中文用户，非提权）当场跑的注册表/COM 调用。推测量标 `[INFERENCE]`。

## 结论（先看这里）

**一句话**：键盘 TIP 的注册分成**两层互不替代**的状态——

1. **机器级目录**（`HKLM\SOFTWARE\Microsoft\CTF\TIP\{CLSID}` + `GUID_TFCAT_TIP_KEYBOARD` 类别成员 + `HKCR\CLSID\{CLSID}\InprocServer32`）
   ＝ 决定 TIP 是否"存在"：设置里的"添加键盘"候选池、以及**能否被加载/激活**。
   写它必须提权；`Register` / `RegisterProfile` / `AddLanguageProfile` / `RegisterCategory` 全部落在这里。
2. **用户级登记**（`HKCU\Control Panel\International\User Profile\<locale>` 里一条 REG_DWORD）
   ＝ 决定"是否出现在该用户的键盘列表 / Win+空格"。写它不需要提权。

**关键实测结论（反直觉，务必注意）**：只写第 2 层、不写第 1 层，条目**会**出现在语言列表里（实测 E1/F1），
但 `ActivateLanguageProfile` 返回 `E_FAIL`、`Set-WinUserLanguageList` 往返一次就把它连同注册表值一起删掉（实测 G1/G2）——
**是一个"看得见但用不了、且会被系统自动清理"的幽灵条目**。
所以两层缺一不可，且第 1 层必须提权。

`SortOrder\AssemblyItem` 是**排序缓存**，不参与可见性判定（实测 E3/E5：只留 SortOrder、删掉 User Profile 值 → 立即不可见）；系统在条目入列时自己补写，安装器不必预写。

silk-ime 现状命中的正是这个缺口：

| 现象 | 根因（本次实测） |
| --- | --- |
| 普通权限跑完只有 `HKCU\...\CTF\TIP\{CLSID}\LanguageProfile\0x00000804\{GUID}\Enable=1` | `Register`、`AddLanguageProfile`、`RegisterProfile`、`RegisterCategory` **全部写 HKLM**，非提权一律 `E_FAIL(0x80004005)`；只有 `EnableLanguageProfile` 写 HKCU 并成功（实测 T1–T6） |
| 设置"添加键盘"列表不出现 Silk | 该候选池来自**机器级目录**（HKLM `CTF\TIP\*` 的 `LanguageProfile` ∩ 键盘类类别成员）。HKLM 无记录＝候选池里没有它（PIME/青简源码注释同结论；该候选池 API 的直接取证见 §8-1） |
| Win+空格没有 | 用户级登记（第 2 层）根本没写：`reg.rs` 没有任何写 `Control Panel\International\User Profile` 的代码 |
| 用户自称跑过一次提升的 `SilkRegister machine`，HKLM 仍为空 | `reg.rs` 无"主动写 HKLM CTF 键"的路径，全靠 COM 调用；而 `register_com` 若真在 HKLM 跑过会留下 `HKLM\Software\Classes\CLSID\{9D4E…}`——实测该键不存在。`[INFERENCE]` 那次提升实际未生效（UAC 被拒 / rundll32 未提权），且代码把全部 HRESULT 丢弃，失败完全不可见 |

### 可执行注册配方（逐键）

设 `{TIP}` = TIP 的 CLSID，`{PROF}` = profile GUID，`LANGID` = `0x0804`，`<DLL>` = DLL 绝对路径。
**① 段必须提权（HKLM/HKCR）；② 段必须普通用户身份（HKCU）。**

**① 机器级（提权；安装器或已提权的 `regsvr32`/`DllRegisterServer`）**

| # | 注册表路径 | 值名 | 类型 | 数据 | 说明 |
| --- | --- | --- | --- | --- | --- |
| 1 | `HKLM\Software\Classes\CLSID\{TIP}` | *(默认)* | REG_SZ | 显示名，如 `Silk 拼音` | COM 类描述；非必需但齐备 |
| 2 | `HKLM\Software\Classes\CLSID\{TIP}\InprocServer32` | *(默认)* | REG_SZ | `<DLL>` | **必需**；COM 加载入口 |
| 3 | 同上 | `ThreadingModel` | REG_SZ | `Apartment` | **必需**；TSF 标准（Weasel/PIME/青简/清风/Mozc 全同） |
| 4 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}` | *(默认)* | REG_SZ | 服务描述 | `Register()` 创建；手写可省 |
| 5 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\LanguageProfile\0x00000804\{PROF}` | `Description` | REG_SZ | `Silk 拼音` | **必需**；语言栏/设置列表显示名 |
| 6 | 同上 | `IconFile` | REG_SZ 或 REG_EXPAND_SZ | `<DLL>` 或 `%SystemRoot%\System32\shell32.dll` | 可为空则无图标 |
| 7 | 同上 | `IconIndex` | REG_DWORD | `0` | |
| 8 | 同上 | `Enable` | REG_DWORD | `0` | 机器级默认启用位；推荐 `0`，让用户级 `Enable` 决定 |
| 9 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{34745C63-B2F0-4784-8B67-5E12C8701A31}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | **必需**；`{34745C63-…}` = `GUID_TFCAT_TIP_KEYBOARD` |
| 10 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Item\{TIP}\{34745C63-B2F0-4784-8B67-5E12C8701A31}` | *(默认)* | REG_SZ | *(可空)* | 反向索引；`RegisterCategory` 成对创建，手写建议同建 |
| 11 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{13A016DF-560B-46CD-947A-4C3AF1E0E35D}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT`（Win8+ 商店应用/现代宿主） |
| 12 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{25504FB4-7BAB-4BC1-9C69-CF81890F0EF5}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT`（输入指示器） |
| 13 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{49D2F9CF-1F5E-11D7-A6D3-00065B84435C}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_TIPCAP_UIELEMENTENABLED`（候选窗 UIElement） |
| 14 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{CCF05DD7-4A87-11D7-A6E2-00065B84435C}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT` |
| 15 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{364215D9-75BC-11D7-A6EF-00065B84435C}\{TIP}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_TIPCAP_COMLESS` |
| 16 | `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}\Category\Category\{534C48C1-0607-4098-A521-4FC899C73E90}\{34745C63-…}` | *(默认)* | REG_SZ | *(可空)* | `GUID_TFCAT_CATEGORY_OF_TIP`→键盘类的两跳映射；`[INFERENCE]` 非必需（见 §8-2） |

**② 用户级（普通权限；安装器最后一步，或首次运行）**

| # | 注册表路径 | 值名 | 类型 | 数据 | 说明 |
| --- | --- | --- | --- | --- | --- |
| 17 | `HKCU\Control Panel\International\User Profile\zh-Hans-CN` | `0804:{TIP}{PROF}` | REG_DWORD | `N`（1 起序号） | **决定"出现在该用户键盘列表/Win+空格"**。值名格式 `<langid 四位小写十六进制>:<{CLSID}><{Profile}>`，两段 GUID 紧邻**无分隔符** |
| 18 | `HKCU\Software\Microsoft\CTF\TIP\{TIP}\LanguageProfile\0x00000804\{PROF}` | `Enable` | REG_DWORD | `1` | 用户级启用位（`EnableLanguageProfile` 的落点） |
| 19 | `HKCU\Software\Microsoft\CTF\SortOrder\AssemblyItem\0x00000804\{34745C63-…}\0000000N` | `CLSID` | REG_SZ | `{TIP}` | 排序缓存；**可选**（系统会自动补，见 §2.3） |
| 20 | 同上 | `Profile` | REG_SZ | `{PROF}` | 同上 |
| 21 | 同上 | `KeyboardLayout` | REG_DWORD | `0` | TIP 用 `0`；纯键盘布局用 HKL（如 `0x4090409`） |
| 22 | `HKCU\Software\Microsoft\CTF\SortOrder\Language` | `00000000`, `00000001`, … | REG_SZ | `00000804`, `00000409`, … | 语言顺序；系统维护，不建议手写 |

**③ 不要做的事**

- 不要只写 ② 而不提权——产出"看得见但激活失败、且会被下次 `Set-WinUserLanguageList` 抹掉"的幽灵条目（实测 G1/G2）。
- 不要用 `HKLM\Software\Classes\WOW6432Node\...` 手工兜 32 位；32 位宿主由 `SysWOW64\regsvr32` 各自写自己的视图（青简做法）。
- 不要在升级时对旧 DLL 跑 `regsvr32 /u`——`Unregister(CLSID)` 会把整个 HKLM `CTF\TIP\{TIP}` 连同 profile 一起删（清风源码里的实测记录；Weasel/PIME 同构代码）。
- `rundll32` 不会自己提权。要机器级就必须由提权进程执行，否则宁可明确失败。

## 1. 三个 API 各自写哪里

### 1.1 官方口径

`官方` [Text Service Registration](https://learn.microsoft.com/en-us/windows/win32/tsf/text-service-registration)（2025-03-11 更新）：
> "In addition to the standard COM in-proc server registry entries, a text service must register itself with the Text Services Framework (TSF)"——三步为 `ITfInputProcessorProfiles::Register` → `AddLanguageProfile` → `ITfCategoryMgr::RegisterCategory`；同页另有一句 **"Text service providers should also provide digital signatures with their binary executables."**

`官方` [ITfInputProcessorProfileMgr](https://learn.microsoft.com/en-us/windows/win32/api/msctf/nn-msctf-itfinputprocessorprofilemgr) Remarks：
> "In Windows Vista, it is recommended to use this interface instead of using the following methods: `Register`, `Unregister`, `AddLanguageProfile`, `RemoveLanguageProfile`, `EnumInputProcessorInfo`, `ActivateLanguageProfile`, `GetActiveLanguageProfile`, `EnumLanguageProfiles`."

即**现行推荐入口是 `ITfInputProcessorProfileMgr::RegisterProfile`**（`Register`+`AddLanguageProfile` 的合并体，多出 `bEnabledByDefault` 与 `dwFlags`；`TF_RP_HIDDENINSETTINGUI` 可让 profile 不进设置 UI）。
官方文档**没有**任何一处说明这些 API 的权限要求，也没写它们落到哪个注册表路径。

### 1.2 实际写入位置（可读实现 + 本机实测）

`源码` Wine `msctf`（形状与本机实测的键形态一致，可作为可读参照）：

- `Register`：`RegCreateKeyExW(HKEY_LOCAL_MACHINE, "Software\\Microsoft\\CTF\\TIP\\<clsid>")`，失败即 `E_FAIL`
  —— [dlls/msctf/inputprocessor.c:291-311](https://raw.githubusercontent.com/wine-mirror/wine/master/dlls/msctf/inputprocessor.c)
- `AddLanguageProfile`：**先 `RegOpenKeyExW(HKEY_LOCAL_MACHINE, …\CTF\TIP\<clsid>)`，打不开直接 `E_FAIL`**；然后写机器级
  `…\LanguageProfile\0x<langid>\<profguid>` 的 `Description`/`IconFile`/`IconIndex`/`Enable=0`，并 `add_userkey()` 在
  **HKCU** 建同名空键 + `Enable=0`（同文件 :331-384、:219-241）
- `EnableLanguageProfile`：只写 **HKCU** 的 `…\CTF\TIP\<clsid>\LanguageProfile\0x<langid>\<prof>\Enable`（:629-657）
- `EnableLanguageProfileByDefault`：写 **HKLM** 同一位置（:706-734）
- `ITfCategoryMgr::RegisterCategory`：先 `RegOpenKeyExW(HKEY_LOCAL_MACHINE, …\CTF\TIP\<clsid>)`，再**成对**建
  `Category\Category\<catid>\<guid>` 与 `Category\Item\<guid>\<catid>`
  —— [dlls/msctf/categorymgr.c:97-144](https://raw.githubusercontent.com/wine-mirror/wine/master/dlls/msctf/categorymgr.c)

`实测`（§5 T1–T6，同一台机、非提权）：`Register` / `AddLanguageProfile` / `RegisterProfile` / `RegisterCategory` /
`EnableLanguageProfileByDefault` 全部返回 `0x80004005 (E_FAIL)`；只有 `EnableLanguageProfile` 返回 `S_OK` 并写出
`HKCU\...\Enable=1`。`reg add` 到 `HKLM\SOFTWARE\Microsoft\CTF\TIP\{…}` 直接"拒绝访问"。

→ **三个注册 API 都是机器级操作，只有 `EnableLanguageProfile` 是用户级。** 这解释了 silk-ime 的全部现象：
普通权限下前三个失败（被 `let _ =` 吞掉），只有最后一个成功，于是 HKCU 只剩一个 `Enable=1`。

### 1.3 官方是否支持"按用户（HKCU-only）注册键盘 TIP"

**不支持。** 两条官方文本指向同一结论：

- `官方` [Application Compatibility: UAC: COM Per-User Configuration](https://learn.microsoft.com/en-us/previous-versions/bb756926(v=msdn.10))：
  > "Per-user COM objects configurations are stored in HKEY_CURRENT_USER\Software\Classes, while per-machine configurations are stored in HKEY_LOCAL_MACHINE\Software\Classes. **Typically, per-user configurations take precedence.** … Beginning with Windows Vista … if the integrity level of a process is higher than Medium, **the COM runtime ignores per-user COM configuration and accesses only per-machine COM configuration.**"

  这正是键盘 TIP 的致命点：它必须能在开始菜单搜索、锁屏、设置等高完整性宿主里被加载，而那些宿主会忽略 HKCU 的 COM 注册。
- TSF 侧不存在"per-user TIP"概念：`CTF\TIP` 只有 HKLM 目录 + HKCU 的 `Enable` 覆盖位。

**所以 HKCU-only 的键盘 TIP 没有受支持的完整形态**：可以出现在列表（实测），但无法激活（实测）、且会被系统清理（实测）。
`[INFERENCE]` 微软从未提供 per-user TSF 注册 API；上两条官方文本 + 本机行为一致指向"必须机器级注册"。

## 2. 机制分析：谁枚举什么

### 2.1 (a) 设置 →"添加键盘"候选列表

候选池＝**机器级目录**：枚举 `HKLM\SOFTWARE\Microsoft\CTF\TIP\*` 的 `LanguageProfile\0x<langid>\*`，用
`Category\Category\{34745C63-…}\{CLSID}` 存在与否筛掉非键盘类，再按 locale 归到各语言分组。

证据：
- `源码` Wine `ProfilesEnumGuid_Constructor` 打开 `HKLM\SOFTWARE\Microsoft\CTF\TIP`，`Next` 逐个把子键名
  `CLSIDFromString`——即 `EnumInputProcessorInfo` 的来源就是 HKLM 目录。
- `源码` `EnumTfLanguageProfiles` 遍历 `HKLM …\LanguageProfile\0x<langid>\*`，用 `ITfCategoryMgr::FindClosestCategory` 过滤类别。
- `源码` PIME `libIME2/src/ImeModule.cpp:432-444` 与青简 `registry/mod.rs:24` 的注释都直接指出"设置面板/切换器另有一套注册表要求"。
- `实测`：HKLM 已注册的微软五笔（`{6A498709-…}`/`{82590C13-…}`）一旦出现在用户列表就能被激活；HKCU-only 的 silk 不能（T13/T15）。

### 2.2 (b) Win+空格切换器

读的是"当前用户已启用输入法列表"：①`HKCU\Control Panel\International\User Profile\<locale>`（**成员资格**）
+ ②`HKCU\Software\Microsoft\CTF\SortOrder\...`（**顺序**），随后按 ① 的 CLSID/Profile 去 HKLM 目录解析并加载。

`源码` 青简 `apps/windows/tsf/src/com/registry/mod.rs:24`：
> "除「键盘类 TIP」外还要声明沉浸式 / 系统托盘等能力，否则 Win10/11 的输入切换器会把它过滤掉（表现为「装上又消失」）。"

`源码` PIME `libIME2/src/ImeModule.cpp:432-444`：
> "NOTE: For Windows newer than Windows 8, we have to manually write some settings to the registry so the input methods can appear in the Windows control panel. Registry path: `HKEY_CURRENT_USER\Control Panel\International\User Profile\<locale_name>`, Sub key: `<lang ID>:{text service GUID}{input module GUID}`. **Unfortunately, this is not documented officially by Microsoft.**"

两个独立项目在无官方文档的情况下各自发现同一套键——这是本调研最硬的一手证据。

### 2.3 `SortOrder\AssemblyItem` 精确语义

本机实测的完整形态（简体中文用户，已装微软拼音 + 五笔）：

```
HKCU\Software\Microsoft\CTF\SortOrder\AssemblyItem\0x00000804
  \ {34745C63-B2F0-4784-8B67-5E12C8701A31}          ← 恒为 GUID_TFCAT_TIP_KEYBOARD
      \ 00000000
            CLSID          REG_SZ    {81D4E9C9-1D3B-41BC-9E6C-4B40BF79E35E}
            KeyboardLayout REG_DWORD 0x0
            Profile        REG_SZ    {FA550B04-5AD7-411F-A5AC-CA038EC515D7}
      \ 00000001                                    ← 加入五笔后系统自动多出
            CLSID          REG_SZ    {6A498709-E00B-4C45-A018-8F9E4081AE40}
            KeyboardLayout REG_DWORD 0x0
            Profile        REG_SZ    {82590C13-F4DD-44F4-BA1D-8667246FDF8E}
HKCU\Software\Microsoft\CTF\SortOrder\Language
  00000000  REG_SZ  00000804
  00000001  REG_SZ  00000409
```

- **第一层 GUID**：`{34745C63-…}` = `GUID_TFCAT_TIP_KEYBOARD`，即"装配类别"。纯键盘布局（`00000409` 美式键盘）也挂在此类别下，用 `KeyboardLayout` 非零区分，`CLSID`/`Profile` 填全零 GUID（本机 `0x00000409` 分支即如此）。
- **索引编号**：子键名是 **8 位零填充十六进制递增序号**，在 `(<langid>, <category GUID>)` 作用域内从 `00000000` 起。
- **`KeyboardLayout`**：TIP 写 `0`；纯布局写 HKL（`0x4090409` / `0x8040804`）。
- **写入时机（实测）**：**系统在条目入列时自己写**。本机用 `Set-WinUserLanguageList` 加五笔后立刻多出 `00000001`；
  反之直接手写 `User Profile` 值也能立即出现在列表（E1/F1），但 **SortOrder 不会自动补**，此时顺序按 `User Profile` 值的
  REG_DWORD 序号（1,2,…）决定。`[INFERENCE]` 两者并存时 SortOrder 优先。
- **可见性判据（实测 E3/E5）**：删掉 `User Profile` 值、只留 SortOrder → **立刻不可见**。即 SortOrder 只影响顺序。
- **安装器该不该预写**：**不必**，写了也无害。要"顺序确定"时可在写完 `User Profile` 值后追加一个
  `0000000N`（N = 该类别下现有最大索引 + 1）。Weasel/青简/清风都不预写；PIME 走 `User Profile` 值 + **1..N 连续重编号**
  （`ImeModule.cpp:221-235`，注释警告编号有空洞会让 `InputSwitch.dll` 崩）。

### 2.4 从"注册完"到"用起来"的完整状态机

```
[文件] DLL 落地（任意可读路径；AppContainer 宿主需额外 ALL APPLICATION PACKAGES RX ACL）
   ↓ regsvr32 / DllRegisterServer（**必须提权**）
[HKLM] CLSID\InprocServer32                              ← COM 能找到 DLL
[HKLM] CTF\TIP\{TIP}\LanguageProfile\0x0804\{PROF}       ← Description/Icon：候选池的来源
[HKLM] CTF\TIP\{TIP}\Category\Category\GUID_TFCAT_TIP_KEYBOARD\{TIP}  ← 键盘类资格
   ↓ （用户级，普通权限）
[HKCU] Control Panel\International\User Profile\zh-Hans-CN  ← 出现在该用户列表 / Win+空格
[HKCU] CTF\TIP\{TIP}\LanguageProfile\0x0804\{PROF}\Enable=1 ← 启用
[HKCU] CTF\SortOrder\AssemblyItem\0x00000804\{34745C63-…}\0000000N ← 顺序（系统补）
   ↓ ctfmon / 宿主进程
[激活] CoCreateInstance(TIP CLSID) → ITfTextInputProcessor::Activate
```

## 3. 真实项目对照

| 项目 | COM 注册 | TIP/Profile | 类别 | 用户语言列表（"开箱即用"） | 安装位置 / 提权 |
| --- | --- | --- | --- | --- | --- |
| **小狼毫 Weasel** | `WeaselTSF/Register.cpp:162-212` 手写 `HKCR\CLSID\{…}\InprocServer32` | `ITfInputProcessorProfileMgr::RegisterProfile`（`Register.cpp:45-50`）——**用的就是新接口** | `Register.cpp:78-88`，16 个类别（含 `TIP_KEYBOARD`/`IMMERSIVESUPPORT`/`SYSTRAYSUPPORT`/`COMLESS`/`WOW16`） | **`Set-WinUserLanguageList`**（`WeaselSetup/WeaselLanguageProfile.ps1:23-70`，由 `imesetup.cpp:154-203` 隐藏进程调起）；另调 `EnableLanguageProfile(TRUE)`+`EnableLanguageProfileByDefault(TRUE)`（`imesetup.cpp:375-382`） | `output/install.nsi:43` `RequestExecutionLevel admin`；DLL 复制到 **`%SystemRoot%\System32\weasel.dll`**（`imesetup.cpp:222-223`） |
| **PIME** | `libIME2/src/ImeModule.cpp:483-495` 手写 `HKCR\CLSID\{…}\InprocServer32`+`Apartment` | `Register`+`AddLanguageProfile`（`ImeModule.cpp:399-424`），并**同时**调 `EnableLanguageProfileByDefault(TRUE)` 与 `EnableLanguageProfile(TRUE)`（注释：重装不会清掉早期删除留下的 per-user `Enable=0`，那会让它从 Win+空格消失） | `ImeModule.cpp:508-548`：`TIP_KEYBOARD`、`DISPLAYATTRIBUTEPROVIDER`、`INPUTMODECOMPARTMENT`、`UIELEMENTENABLED`、`IMMERSIVESUPPORT`、`SYSTRAYSUPPORT` | **直接写注册表** `HKEY_USERS\<SID>\Control Panel\International\User Profile\<locale>`，值名 `%04x:{CLSID}{ProfileGUID}`、REG_DWORD 序号；连 `HKU\__PIME_Default_user__`（默认用户配置单元）一起写；1..N 连续重编号（`ImeModule.cpp:240-314`） | NSIS；注释明写"requires Administrator permission…regsvr32 should be run with Administrator"（`ImeModule.cpp:466-467`） |
| **青简 qingjian** | `apps/windows/tsf/src/com/registry/mod.rs:41-47`：`CLASSES_ROOT` 建 `CLSID\{…}`+`InprocServer32`+`Apartment` | 同函数内 `Register`+`AddLanguageProfile`（`mod.rs:55-80`） | `mod.rs:25-33` 8 个：`TIP_KEYBOARD`、`UIELEMENTENABLED`、`SECUREMODE`、`COMLESS`、`INPUTMODECOMPARTMENT`、`IMMERSIVESUPPORT`、`SYSTRAYSUPPORT`、`DISPLAYATTRIBUTEPROVIDER` | **不做**（`[源码无对应代码]`；安装后需用户手动"设置→语言→中文→选项→添加键盘"） | `qingjian.iss:50` `PrivilegesRequired=admin`；装到 `C:\Program Files\Qingjian`；64/32 位各用 `regsvr32` 与 `SysWOW64\regsvr32` 注册一次（`qingjian.iss:134-137`）；目录加 `ALL APPLICATION PACKAGES`(S-1-15-2-1) RX ACL |
| **清风 WindInput** | `wind_tsf/src/Register.cpp:36-90` 手写 `HKCR\CLSID\{…}\InprocServer32`+`Apartment` | `RegisterProfile()` 优先 `ITfInputProcessorProfileMgr::RegisterProfile(…, TRUE, 0)`，失败回落 `Register`+`AddLanguageProfile`（`Register.cpp:282-368`） | `Register.cpp:380-437` 16 个，注释"与小狼毫保持一致…确保 Win11 开始菜单/UWP 兼容" | `scripts/dev.ps1:1221-1250`：`Get-WinUserLanguageList` → `InputMethodTips.Add(tip)` → `Set-WinUserLanguageList -Force`；注释结论与本调研一致：**"regsvr32/DllRegisterServer 只把 IME 注册为系统级'可用'；对已配置好的语言，Windows 不会自动把新 TIP 追加进用户启用列表（RegisterProfile 的 bEnabledByDefault 仅在该语言【首次添加】时生效）"** | 安装器提权；dev 脚本注释"部署在管理员令牌下运行；同账户 UAC 提升时 HKCU 仍指向本人" |
| **Mozc** | `src/win32/base/tsf_registrar.cc` `RegisterCOMServer`：`HKCR\CLSID\{…}`+`InProcServer32`+`Apartment`，注释 "These operations are allowed only for administrators" | `RegisterProfiles`：`Register`+`AddLanguageProfile`+`ILanguageProfileEx::SetLanguageProfileDisplayName` | `kCategories[]`：`DISPLAYATTRIBUTEPROVIDER`、`COMLESS`、`INPUTMODECOMPARTMENT`、`UIELEMENTENABLED`、`TIP_KEYBOARD`、`IMMERSIVESUPPORT`、`SYSTRAYSUPPORT` | 无 | 安装器提权 |

**横向结论**：
1. 五家**全部**提权写 HKLM，没有一家做 per-user-only 键盘 TIP——做不成（§1.3）。
2. `GUID_TFCAT_TIP_KEYBOARD` 五家全有；`IMMERSIVESUPPORT`+`SYSTRAYSUPPORT`+`UIELEMENTENABLED`+`INPUTMODECOMPARTMENT`+`COMLESS` 四家五家有，是"Win10/11 被切换器过滤"的对症药（青简注释）。
3. "开箱即用"只有两条落地路径：`Set-WinUserLanguageList`（Weasel、清风）或直接写 `Control Panel\International\User Profile`（PIME）。前者是官方 PowerShell 表面、更稳妥但会做 HKLM 校验；后者可控到值名/序号，且是唯一能可靠批量写多用户的方式。
4. `ThreadingModel` 五家统一 `Apartment`。

## 4. 官方现行要求 / 装载条件

- `官方` [Input Method Editors (IME)](https://learn.microsoft.com/en-us/windows/apps/develop/input/input-method-editors)（2026-10-02 更新）"Requirements for IMEs"：
  > "A third-party IME must meet these requirements: **Must be digitally signed**；Must be Text Services Framework (TSF) aware, with appropriate IME flags set correctly；Must follow the guidelines described in [Input Method Editor (IME) requirements]。A third-party IME that doesn't meet these requirements is **blocked from running**."
- 同页："**All IMEs run in the Windows system, which is hardened** to stop malicious IMEs…"
- `官方` [Text Service Registration](https://learn.microsoft.com/en-us/windows/win32/tsf/text-service-registration)：要求给二进制提供数字签名。
- `官方` [UAC: COM Per-User Configuration](https://learn.microsoft.com/en-us/previous-versions/bb756926(v=msdn.10))：完整性级别 > Medium 时 COM 只读 HKLM（见 §1.3 引文）。

**签名与安装位置对"可见性/加载"的影响**：

| 事项 | 结论 | 证据 |
| --- | --- | --- |
| 未签名 | 对**是否出现在列表**无影响（纯注册表）；但会被官方定义为 "blocked from running"，实际表现随宿主而定（Trusted Mode 游戏、部分系统 UI 拒载） | 官方 IME overview；清风 `dev.ps1` 警告"未签名 DLL 普通程序可用，但 Trusted Mode 游戏（CS2 等）会拒绝加载" |
| 装 Program Files vs 用户目录 | 对"是否出现在列表"**无影响**；对**能否被 AppContainer 宿主（设置、开始菜单搜索、任务栏）加载**有影响——需给 DLL 加 `ALL APPLICATION PACKAGES`(S-1-15-2-1) 读+执行权限 | 青简 `installer/README.md` 第 2 条；Weasel 干脆装进 `System32` |
| uiAccess | 只影响候选窗能否盖住系统 UI，与注册可见性无关 | 见 [windows-ime-ui-rendering-2026.md](windows-ime-ui-rendering-2026.md) |

## 5. 本机实测记录（2026-11-11，Win11 26300.9974，简体中文，非提权）

方法：`Z:\tmpreg` 下用 PowerShell + `Add-Type` 手写 COM 接口（`ITfInputProcessorProfiles` / `ITfInputProcessorProfileMgr` /
`ITfCategoryMgr`）与 `reg` 命令。测前导出 `HKCU\Software\Microsoft\CTF`、`HKCU\Control Panel\International`、
`HKCU\Keyboard Layout` 快照，测后逐项回滚并核对（最终状态：User Profile 仅微软拼音、SortOrder 仅 `00000000`、
HKCU `CTF\TIP` 无 silk 键）。

| # | 操作 | 结果 |
| --- | --- | --- |
| T1 | 非提权 `ITfInputProcessorProfiles::Register(silk)` | `0x80004005` (E_FAIL) |
| T2 | 非提权 `AddLanguageProfile(silk, 0x0804)` | `0x80004005` |
| T3 | 非提权 `ITfInputProcessorProfileMgr::RegisterProfile(silk, …, bEnabledByDefault=TRUE, 0)` | `0x80004005` |
| T4 | 非提权 `ITfCategoryMgr::RegisterCategory(silk, TIP_KEYBOARD, silk)` | `0x80004005` |
| T5 | 非提权 `EnableLanguageProfile(silk, 0x0804, prof, TRUE)` | `S_OK`，写出且**仅**写出 `HKCU\...\CTF\TIP\{silk}\LanguageProfile\0x00000804\{prof}\Enable=1`（与用户报告的真机状态逐字一致） |
| T6 | `EnableLanguageProfileByDefault` | `0x80004005`（写 HKLM） |
| T7 | `reg add HKLM\SOFTWARE\Microsoft\CTF\TIP\{…}` | "拒绝访问" |
| T8 | `reg add …\CTF\TIP\{silk}` 只建根键后再 `AddLanguageProfile` | `0x80004005`（仍要 HKLM） |
| T9 | HKLM 已注册的微软五笔 + 手写 `User Profile` 值 | `Get-WinUserLanguageList` **立即可见**，自动获得 REG_DWORD 序号 |
| T10 | 在 T9 基础上 `Set-WinUserLanguageList` 往返 | 五笔保留；silk（HKCU-only）被丢弃 |
| T11 | `InstallLayoutOrTip("0804:{silk}{prof}", 0)` | 返回 `1`，但注册表**零写入**（静默失败） |
| T12 | `Set-WinUserLanguageList` 追加 silk（HKLM 未注册） | 不报 TIP 相关的错误（只有无关的"显示语言将在下次登录生效"警告），但往返后列表里没有 silk |
| T13 | 判别实验：`{Wubi}+真 profile` → 接受；`{Wubi}+伪造 profile` → 拒绝；`{未知 CLSID}+伪造 profile` → 拒绝 | `Set-WinUserLanguageList` 的校验是 **HKLM TIP + Profile 双重**，不是只看 CLSID |
| T14 | `EnumLanguageProfiles(0x0804)` | 只返回 HKLM 注册的 profile；HKCU-only 的 silk 不出现 |
| T15 | 加入五笔后 `SortOrder\AssemblyItem\0x00000804\{34745C63-…}` | 系统自动新增子键 `00000001`（`CLSID`/`Profile`/`KeyboardLayout=0`） |

**可见性隔离实验（最关键一组）**

| # | 状态 | `Get-WinUserLanguageList` 含 silk？ |
| --- | --- | --- |
| E0 | 全清（无 UP 值、无 CTF 键、无 SortOrder） | 否 |
| E1 | **只**写 `User Profile` 值（HKLM 未注册 silk） | **是** |
| E2 | E1 + 手写 SortOrder 条目 | 是 |
| E3 | 删 `User Profile` 值，保留 SortOrder | 否 |
| E4 | E3 + HKCU `CTF\TIP\...\Enable=1` | 否 |
| E6 | 再写回 `User Profile` 值（CTF 键仍在） | 是 |
| F2 | 在"E1 状态"下跑一次 `Set-WinUserLanguageList`（不改内容） | **否**，且注册表值被一并删除 |
| H1 | 在 E1 状态下 `ActivateLanguageProfile(silk)` | **`0x80004005`**（看得见，激活不了） |

结论：**`User Profile` 值 = 可见性开关；SortOrder = 仅顺序；HKLM 目录 = 能否被激活/是否进候选池**。
只做用户级会得到"看得见、点不动、下次被清理"的幽灵条目。

## 6. 普通权限能达到的极限

普通用户（Medium IL，无提权）**能**：
1. 写 `HKCU\Software\Classes\CLSID\{TIP}\InprocServer32`（per-user COM，Medium IL 进程的 COM 认）。
2. 写 `HKCU\...\CTF\TIP\{TIP}\LanguageProfile\0x0804\{PROF}\Enable=1`（`EnableLanguageProfile` 的落点）。
3. 写 `HKCU\Control Panel\International\User Profile\<locale>` 值 → 条目**出现在列表**。
4. 写 `HKCU\...\SortOrder\...`。

普通用户**不能**：
1. 写 `HKLM\Software\Classes\CLSID\{TIP}`——高完整性宿主（开始菜单、锁屏、设置）唯一认的 COM 注册。
2. 写 `HKLM\SOFTWARE\Microsoft\CTF\TIP\{TIP}` 及其子键——候选池与校验的唯一来源。

**极限**：进程在 Medium IL 宿主内或许能被 CLSID 实例化，但 `ActivateLanguageProfile` 会失败、条目不会进"添加键盘"候选池、
`Set-WinUserLanguageList` 会静默剔除它、且下次列表重建时条目连同注册表值一起消失。

**silk-ime 用户看到的"注册成功"是 API 返回值的幻觉**：`EnableLanguageProfile` 真的成功了，其余三个 `E_FAIL` 被 `let _ =` 丢弃。

## 7. 对 silk-ime 的落地建议

### 7.1 必须改的语义

1. **分清两层、两次运行**：机器级（提权）＝ COM + TIP + Category；用户级（不提升）＝ `Enable` + 用户语言列表。
   现在 `Scope::Machine` 只改 COM 键，语义应改为"**整套机器级注册**"；两者都要能独立重入。
2. **不许吞 HRESULT**：`Register` / `RegisterProfile` / `RegisterCategory` 失败必须上抛（日志 + 非零返回）。
   本 case 的全部排查成本都来自这一句 `let _ =`。
3. **改用 `ITfInputProcessorProfileMgr::RegisterProfile`**（官方 Vista+ 推荐），一次写全 `LanguageProfile` 四个值；
   `bEnabledByDefault=FALSE`（让用户级决定），`dwFlags=0`。`Register`+`AddLanguageProfile` 只作 Win7 回落。
4. **类别补齐**：当前只有 `TIP_KEYBOARD` + `CATEGORY_OF_TIP`。至少补
   `IMMERSIVESUPPORT`、`SYSTRAYSUPPORT`、`UIELEMENTENABLED`、`INPUTMODECOMPARTMENT`、`COMLESS`（五家实现全有；缺了切换器会过滤）。
5. **补"用户语言列表"这一步**——这是"开箱即用"的开关。两条路径：
   - **路径 A（推荐）**：注册完机器级后，以**当前用户**身份走 `Set-WinUserLanguageList` 语义（Weasel/清风现成）。
     注意它会对 HKLM 目录做严格校验（T13），所以必须先完成机器级注册、且最好在新进程里做（TSF 目录有最终一致性延迟，见 §9-11）。
   - **路径 B（可控，PIME 式）**：直接写 `HKCU\Control Panel\International\User Profile\<locale>`，值名
     `<langid 四位小写十六进制>:<{CLSID}><{Profile}>`、REG_DWORD、数据 = 现有最大值 + 1，并把既有条目重编号为 1..N 连续
     （PIME 注释：编号有空洞会让 `InputSwitch.dll` 崩）。本机实测 E1 立即生效。
   - 两者**不能混用**同一个条目：混用会被 `Set-WinUserLanguageList` 的下一次运行清掉（F2）。
6. **`SortOrder` 可选**：不写也行；要写就按 §2.3 的形态，索引取该类别现有最大 + 1。
7. **`Machine` 必须真提权**：`rundll32` 不自动提权。要么由提权安装器调用，要么清单要求 `requireAdministrator`；
   不要再保留一个"看起来能注册机器级"的普通权限入口——那只会产出幽灵条目。
8. **反注册要对称**：删机器级三件套之外，**必须**同时删 `HKCU\...\User Profile\<locale>` 的值并重编号，
   否则卸载后留下点不动的幽灵条目（PIME `unregisterUserProfiles` 同款）。

### 7.2 建议的注册步骤（伪代码）

```
register_machine(dir):                     # 必须提权；任何一步失败都整体报错
    ensure HKCR\CLSID\{TIP}                                  (默认=描述)
    ensure HKCR\CLSID\{TIP}\InprocServer32                   (默认=<DLL>, ThreadingModel=Apartment)
    p = CoCreateInstance(CLSID_TF_InputProcessorProfiles) as ITfInputProcessorProfileMgr
    p.RegisterProfile(TIP, 0x0804, PROF, "Silk 拼音", <DLL>, 0, NULL, 0, FALSE, 0)
    for cat in [TIP_KEYBOARD, TIPCAP_IMMERSIVESUPPORT, TIPCAP_SYSTRAYSUPPORT,
                TIPCAP_UIELEMENTENABLED, TIPCAP_INPUTMODECOMPARTMENT, TIPCAP_COMLESS]:
        catMgr.RegisterCategory(TIP, cat, TIP)
    # CATEGORY_OF_TIP 可加，非必需（§8-2）

register_user(locale="zh-Hans-CN"):        # 不提升；HKCU
    profiles.EnableLanguageProfile(TIP, 0x0804, PROF, TRUE)      # HKCU Enable=1
    add_to_user_language_list(locale, TIP, PROF)                 # 路径 A 或 B（择一）
    # 可选：append_sortorder(0x0804, TIP_KEYBOARD, TIP, PROF, 0)
```

### 7.3 与现有 `reg.rs` 的逐项差异

| 位置 | 现状 | 应改为 |
| --- | --- | --- |
| `Scope` | `User` 全 HKCU / `Machine` 只影响 COM | `Machine` = 全套机器级（COM + TIP + Category）；`User` = 仅 `Enable` + 用户列表 |
| `register_com` | 写 `Software\Classes\CLSID\…`（HKCU 或 HKLM） | 机器级必须 HKLM 且失败即报错；HKCU 那份对键盘 TIP 无实际价值，可留作兜底但不计入成功判据 |
| `register_profile` | `Register` + `AddLanguageProfile`，`let _ =` | `RegisterProfile`（Vista+），检查返回值 |
| `register_categories` | 2 个类别 | 6 个（见 7.1-4） |
| 缺失 | 无用户语言列表登记、无反注册联动 | 加 `register_user_language_list`；反注册同步删 `User Profile` 值并重编号 |
| 反注册 | `RemoveLanguageProfile` + `Unregister` | 保留；另需删 `HKCU\...\User Profile\<locale>` 值与 `SortOrder` 条目 |

## 8. 未取得一手证据 / 开放问题

1. **"设置→添加键盘"候选池的具体 API**：尝试用 `input.dll` 的 `EnumLayoutOrTipForSetup` 直接取证，但
   `LAYOUTORTIP` 结构在本机解不出可读字符串（218 条记录全乱），**未取得一手证据**。目前依据是 PIME/青简源码注释 + HKLM 目录行为反推，
   结论 `[INFERENCE]`：候选池枚举 HKLM `CTF\TIP\*` ∩ 键盘类。
2. **`GUID_TFCAT_CATEGORY_OF_TIP`（`{534C48C1-…}`）是否键盘 TIP 必需**：本机第三方键盘 TIP（微软拼音 `{81D4E9C9-…}`、
   五笔 `{6A498709-…}`）在 `Category\Category\{534C48C1-…}` 下**没有**成员却完全可用；仅 `{78CB5B0E-…}`（GDI+ 相关）有。
   故 `[INFERENCE]` 非必需。silk 写上无害。
3. **`Category\Item` 反向索引是否必需**：`RegisterCategory` 成对建；手写省略是否可接受未单独验证。建议照建（零成本）。
4. **`SortOrder` 与 `User Profile` 序号冲突时谁优先**：`[INFERENCE]`（SortOrder 优先），未做对照实验。
5. **`LanguageProfile` 下 `Display Description`（MUI 间接串）、`ProfileFlags`、`SubstituteLayout` 的语义**：官方无文档。
   本机微软拼音 profile 有 `Display Description` 且 `Enable=0`；微软日文 profile 有 `ProfileFlags=0x2`。未验证。
6. **`RegisterProfile` 的 `bEnabledByDefault` 在"语言已存在"时是否真的无效**：只有清风 `dev.ps1:1226` 一句注释为孤证，
   本机无提权无法复现。
7. **用户那次 `SilkRegister machine` 到底发生了什么**：注册表无痕，只能确定"没有成功"。
8. **签名"blocked from running"的具体拦截点**（哪个进程、哪条校验）：官方只有一句话，无实现细节。
9. **Win+空格切换器是否会对"可见但无法激活"的条目做二次过滤**：H1 只证明 `ActivateLanguageProfile` 失败，未验证切换器 UI 的表现。

## 9. 引用

**官方**

1. Text Service Registration — https://learn.microsoft.com/en-us/windows/win32/tsf/text-service-registration （更新 2025-03-11）
2. ITfInputProcessorProfiles::Register — https://learn.microsoft.com/en-us/windows/win32/api/msctf/nf-msctf-itfinputprocessorprofiles-register
3. ITfInputProcessorProfileMgr（"Vista 起推荐替代清单"） — https://learn.microsoft.com/en-us/windows/win32/api/msctf/nn-msctf-itfinputprocessorprofilemgr
4. ITfInputProcessorProfileMgr::RegisterProfile（`TF_RP_*` flags） — https://learn.microsoft.com/en-us/windows/win32/api/msctf/nf-msctf-itfinputprocessorprofilemgr-registerprofile
5. ITfCategoryMgr::RegisterCategory — https://learn.microsoft.com/en-us/windows/win32/api/msctf/nf-msctf-itfcategorymgr-registercategory
6. Predefined Category Values — https://learn.microsoft.com/en-us/windows/win32/tsf/predefined-category-values （更新 2025-06-24）
7. Input Method Editors (IME)（"must be digitally signed…blocked from running"） — https://learn.microsoft.com/en-us/windows/apps/develop/input/input-method-editors （更新 2026-10-02）
8. Application Compatibility: UAC: COM Per-User Configuration — https://learn.microsoft.com/en-us/previous-versions/bb756926(v=msdn.10)
9. Set-WinUserLanguageList（`InputMethodTips` 的 `Language ID:Keyboard layout ID` 形式） — https://learn.microsoft.com/en-us/powershell/module/international/set-winuserlanguagelist
10. Add Input Method Editor (IME) to Windows PE（微软自己给出的 `CTF\TIP` / `SortOrder\AssemblyItem` / `Assemblies` 手写样例；Win7 归档） — https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-7/dd744589(v=ws.10)
11. Windows TSF installer: same-process Profile/Category readback is stale…（MS 支持答复：注册调用结果与枚举验证要分开，且无强制刷新 API） — https://learn.microsoft.com/en-us/answers/questions/6024569/

**源码**

12. rime/weasel — `WeaselTSF/Register.cpp`（COM/Profile/Categories）、`WeaselSetup/WeaselLanguageProfile.ps1`（`Set-WinUserLanguageList`）、`WeaselSetup/imesetup.cpp`（提权 + DLL 落到 System32）、`output/install.nsi:43`
13. EasyIME/libIME2 — `src/ImeModule.cpp`（PIME 的 `registerLangProfiles` / `registerUserProfiles` / `registerServer`）
14. qingjian — `apps/windows/tsf/src/com/registry/mod.rs`、`apps/windows/installer/qingjian.iss`、`apps/windows/installer/README.md`
15. WindInput — `wind_tsf/src/Register.cpp`、`scripts/dev.ps1:1221-1250`
16. mozc — `src/win32/base/tsf_registrar.cc`、`src/win32/base/input_dll.h`（`LAYOUTORTIP` / `ILOT_*` 定义）
    https://chromium.googlesource.com/external/mozc/+/master/src/win32/base/tsf_registrar.cc
17. Wine msctf（Windows 行为的可读参照实现） — `dlls/msctf/inputprocessor.c`、`dlls/msctf/categorymgr.c`

**本机实测**（2026-11-11，Windows 11 26300.9974，简体中文，非提权）

18. 本文 §5 的 T1–T15 与 E0–H1；被测机既有第三方 TIP：`{81D4E9C9-…}`/`{FA550B04-…}`（微软拼音）、`{6A498709-…}`/`{82590C13-…}`（微软五笔）
