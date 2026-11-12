# 参与贡献

Silk 已有可运行基座（引擎 + TSF 适配层）。贡献应优先解决明确的问题，保持实现简单、可验证，并与项目的本地优先和轻量目标一致。
我们不反对 AI Code，但请不要直接提交纯 AI 生成、未经人工审查和验证的 PR 来浪费团队时间。
请务必在开发过程中与仓库作者保持沟通；如遇疑问或涉及重要改动，请及时交流确认。

## 开发环境

- Windows 10/11。
- Go 版本以 [go.mod](go.mod) 为准。
- 使用 Git 管理变更，文本文件统一使用 UTF-8 和 LF 换行。

## 开发流程

1. 从 `main` 创建聚焦单一目标的分支。
2. 实现变更，并为实际行为保留必要的验证。
3. 提交前运行统一构建与验证入口：

   ```powershell
   .\build.ps1
   ```

4. 提交 Pull Request：标题遵循 [AGENTS.md](AGENTS.md) 的提交约定（`类型: 中文主题`，squash 合并后即为主干提交信息），正文说明Summary(简要说明改动的核心逻辑/结构)、Evidence(提供改动有效的具体证据，必须体现 Before / After)、Merge Danger(描述影响范围)；若关联 Issue，请在描述中链接。

## 代码约定

注释、Go 风格与测试规范见 [AGENTS.md](AGENTS.md)。修改公共接口时同步更新调用方与相关文档。

## 报告问题

通过 [GitHub Issues](https://github.com/luobochuanqi/silk-ime/issues) 提交问题，尽量包含 Windows 版本、Silk 版本或提交号、复现步骤、预期行为和实际行为。日志与示例须先移除个人信息。

安全问题请遵循 [安全政策](SECURITY.md)，不要通过公开 Issue 披露。
