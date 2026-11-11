# M1 冒烟：在记事本打出第一个候选词

前置：`.\build.ps1` 全绿，产物在 `.tmp\build\`（silk-tsftip.dll、silkui.dll、silkhost.exe、dict\silk.dict）。

## 步骤

1. **注册输入法**（普通用户权限，PowerShell 在仓库根目录执行）：

   ```powershell
   rundll32 .tmp\build\silk-tsftip.dll,SilkRegister user
   ```

2. **让 TSF 感知新输入法**：多数情况直接可见；若 Win+空格 列表里没有"Silk 拼音"，注销重登一次（或任务管理器重启 ctfmon.exe）。
3. **切换**：Win+空格 → 选"Silk 拼音"。
4. **验证输入**（记事本）：

   | 步骤 | 期望 |
   | --- | --- |
   | 输入 `nihao` | 组字区显示 `nihao`（带下划线），下方出现候选窗（你好/…） |
   | 按空格 | "你好"上屏，候选窗消失 |
   | 输入 `ni` 后按 `2` | 第 2 个候选上屏 |
   | 输入 `zhongguoren` | 候选含"中国人" |
   | 输入 `ni` 后按 Esc | 清空，不残留 |
   | Shift 切英文后输入 `hello` | 字母直接落下，无候选窗 |

5. **卸载**（测试完）：`rundll32 .tmp\build\silk-tsftip.dll,SilkUnregister user`

## 已知边界（本迭代不做，勿记为缺陷）

英文单词补全、候选窗在开始菜单/搜索面板前的层级（`host_render` 兜底）、触摸键盘回避、设置界面。

## 反馈格式

每行：`步骤 → 实际表现（或截图）`；若有异常，附 `%LOCALAPPDATA%\Silk\logs\silkhost.log` 尾部与 Windows 版本号。
