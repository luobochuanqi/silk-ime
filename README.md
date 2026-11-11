# Silk IME

[![CI](https://github.com/luobochuanqi/silk-ime/actions/workflows/ci.yml/badge.svg)](https://github.com/luobochuanqi/silk-ime/actions/workflows/ci.yml)
[![License: GPL v3](https://img.shields.io/badge/license-GPLv3-blue.svg)](LICENSE)

**快而安静的中文输入法。**

Silk 是一款 Windows 优先、本地优先的开源中文输入法项目。我们致力于打造一款快速、稳定、安静、可预测以及极致轻量的次世代输入法软件，让前沿技术真正有价值地融入到输入法中。

Silk 取“轻盈、顺滑、不打扰”之意。它追求的是恰到好处的功能、长期可靠的体验，以及简单透明的实现。

> **项目状态：早期设计阶段。** 目前尚未提供可使用版本。

## 设计原则

### 前沿技术加持

我们希望可以将更多的前沿技术融入进来，通过多领域的工程手段提升软件水准以及用户体验。

### 基础输入可靠稳定

保证舒适流畅的基础输入体验。

### 极简，但恰到好处

开箱即用，且覆盖日常中文输入的高频需求，没有复杂配置。

### 本地优先，数据可控

数据均在本地进行治理，尽可能使用端侧模型。

## 技术栈

Go（引擎与 host 进程）+ Rust（TSF 适配层与候选窗渲染），进程间以命名管道通信。

## 参与贡献

贡献流程见 [CONTRIBUTING.md](CONTRIBUTING.md)；安全漏洞按 [安全政策](SECURITY.md) 披露。

## 开源许可

Silk 采用 [GNU General Public License v3.0](LICENSE) 开源。
