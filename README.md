# Magies Clean

[![最新版本](https://img.shields.io/github/v/release/Zhangwei930/MagiesClean?label=%E6%9C%80%E6%96%B0%E7%89%88%E6%9C%AC)](https://github.com/Zhangwei930/MagiesClean/releases/latest)
[![下载量](https://img.shields.io/github/downloads/Zhangwei930/MagiesClean/total?label=%E4%B8%8B%E8%BD%BD%E9%87%8F)](https://github.com/Zhangwei930/MagiesClean/releases)
[![平台](https://img.shields.io/badge/%E5%B9%B3%E5%8F%B0-Windows%20%7C%20macOS-blue)](#下载)
[![许可证](https://img.shields.io/github/license/Zhangwei930/MagiesClean?label=%E8%AE%B8%E5%8F%AF%E8%AF%81)](LICENSE)

本地离线的自动批量去水印工具，支持图片与 PDF。所有处理都在你的电脑上完成，文件不会上传。

> 仅用于处理你有权处理的图片与文档。

## 功能

- **自动识别水印**：角落文字、Logo、半透明水印、满屏平铺水印、相机/打卡应用叠加的时间地点信息块；PDF 中的水印注释与水印对象。
- **批量学习**：同一批图片里位置、样式一致的水印会被一起学习，识别更准、速度更快。
- **文字识别**：内置中英文文字识别，能认出网址、@用户名、版权声明、时间日期等水印文字。
- **AI 修复**：去除后用本地 AI 模型补全背景，大面积、复杂纹理也能自然衔接；小而细的水印用快速算法。
- **人工复核**：没把握的地方交给你决定；可以勾选多张图片一次处理，也可以手动涂抹补充要去除的区域。
- **安全导出**：始终另存为新文件，原件不会被修改；质量检查不通过的结果不会自动导出。
- **中英文界面**，浅色 / 深色主题。

支持格式：JPG、PNG、WebP、BMP、TIFF、PDF。

## 下载

在 [Releases](https://github.com/Zhangwei930/MagiesClean/releases) 下载对应的安装包：

| 系统 | 文件 |
|---|---|
| Windows 10 / 11（64 位） | `MagiesClean_版本号_Windows_x64-setup.exe` |
| macOS 11 及以上，Apple 芯片（M 系列） | `MagiesClean_版本号_macOS_arm64.dmg` |
| macOS 11 及以上，Intel 芯片 | `MagiesClean_版本号_macOS_x64.dmg` |

安装包没有做开发者签名，第一次打开时系统会拦一下：

- **Windows**：提示“Windows 已保护你的电脑”时，点“更多信息”→“仍要运行”。
- **macOS**：提示“无法验证开发者”时，在“应用程序”里右键点 Magies Clean →“打开”→ 再点“打开”；
  或在终端执行 `xattr -dr com.apple.quarantine "/Applications/Magies Clean.app"`。

## 使用

1. 把图片、PDF 或文件夹拖进窗口，应用会自动扫描。
2. 右侧列出识别到的水印：有把握的会自动去除，其余标为“需复核”，点“去除”或“忽略”即可，结果会自动预览。
3. 左侧勾选要处理的文件（可全选），点右下角“去除已选”/“全部去除”，选择导出位置后开始处理。

## 从源码构建

需要：Rust 1.80+、Node.js 18+；macOS 需安装 Xcode 命令行工具，Windows 需安装 Visual Studio 生成工具（C++）。
在 Windows 上用 Git Bash 运行下面的脚本。

```bash
# 1. 下载模型（AI 修复与文字识别，约 225 MB，会校验 SHA-256）
scripts/fetch-models.sh

# 2. 安装前端依赖
cd apps/desktop && npm install

# 3. 开发运行 / 打包
npx tauri dev
npx tauri build
```

命令行版本（不需要界面，用于批量处理与测试）：

```bash
cargo run -p wm-runtime --release --bin magies-cli -- process <文件或文件夹> --out <导出目录>
cargo run -p wm-runtime --release --bin magies-cli -- models   # 查看模型状态
```

运行测试：`cargo test --workspace --release`

推送 `v*` 标签后，GitHub Actions 会自动构建 Windows 与 macOS 安装包并上传到对应的发布（见 `.github/workflows/release.yml`）。

## 项目结构

| 目录 | 内容 |
|---|---|
| `apps/desktop` | 桌面应用（Tauri 2 + Vue 3） |
| `crates/wm-core` | 核心数据结构、路由规则、多语言文案 |
| `crates/wm-image` | 图片读写、元数据、图像处理基础算法 |
| `crates/wm-detection` | 水印检测：批量学习、平铺图案、叠加文字、信息块、文字识别分类、结果融合 |
| `crates/wm-segmentation` | 从检测框生成像素级蒙版 |
| `crates/wm-removal` | 去除算法：透明度还原、快速修复、纹理合成、AI 修复（分块 + 缓存） |
| `crates/wm-pdf` | PDF 水印对象分析与移除 |
| `crates/wm-ai` | 本地模型加载、校验与各模型适配 |
| `crates/wm-batch` / `wm-storage` / `wm-runtime` | 批处理调度、本地存储、处理流程与命令行 |
| `models` | 模型清单与说明（模型文件用脚本下载，不在仓库中） |

## 第三方模型

| 模型 | 用途 | 许可 |
|---|---|---|
| [LaMa](https://github.com/advimman/lama)（ONNX 版 [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX)） | AI 修复 | 代码 Apache-2.0。注意：官方权重用 Places365 数据集训练，该数据集条款限定非商业研究与教学用途，商业使用前请自行评估 |
| [PaddleOCR PP-OCRv4](https://github.com/PaddlePaddle/PaddleOCR)（ONNX 版 [SWHL/RapidOCR](https://huggingface.co/SWHL/RapidOCR)） | 文字检测与识别 | Apache-2.0 |

## 更新记录

见 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

[MIT](LICENSE)
