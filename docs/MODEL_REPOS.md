# 模型适配 Repo 反向同步（MODEL REPOS）

EntryPoint 的每个推理模块（`modules/<module_id>/`）本质上是**对某个上游开源仓库的适配层**。
本目录说明如何把每个适配层反向同步成一个独立 Git 仓库，分别推送到 GitHub，
实现"主仓库管编排、模型仓库管适配"的发布分离。

## 适配矩阵

反向 repo 统一命名为 `EntryPoint_<模型>`，明确标注是 EntryPoint 的插件。

| 模块 (modules/) | 上游仓库 | 反向同步 repo（GitHub） | 模型 (models/) |
|-----------------|----------|------------------------|----------------|
| `birefnet` | [ZhengPeng7/BiRefNet](https://github.com/ZhengPeng7/BiRefNet) | [EntryPoint_BiRefNet](https://github.com/PegionFish/EntryPoint_BiRefNet) | birefnet-general, birefnet-portrait |
| `deep-filter` | [Rikorose/DeepFilterNet](https://github.com/Rikorose/DeepFilterNet) | [EntryPoint_DeepFilterNet](https://github.com/PegionFish/EntryPoint_DeepFilterNet) | df3 |
| `faster-whisper` | [SYSTRAN/faster-whisper](https://github.com/SYSTRAN/faster-whisper) | [EntryPoint_faster-whisper](https://github.com/PegionFish/EntryPoint_faster-whisper) | large-v1/v2/v3, medium(.en), small(.en), base(.en), tiny(.en) |
| `firered-ocr` | [FireRedTeam/FireRed-OCR](https://github.com/FireRedTeam/FireRed-OCR) | [EntryPoint_FireRed-OCR](https://github.com/PegionFish/EntryPoint_FireRed-OCR) | firered-ocr-2b |
| `paddleocr` | [PaddlePaddle/PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) | [EntryPoint_PaddleOCR](https://github.com/PegionFish/EntryPoint_PaddleOCR) | v4-chinese, pp-structure-v3 |
| `qwen3-asr` | [QwenLM/Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR) | [EntryPoint_Qwen3-ASR](https://github.com/PegionFish/EntryPoint_Qwen3-ASR) | qwen3-asr-0.6b, qwen3-asr-1.7b, qwen3-forced-aligner-0.6b |
| `qwen3-tts` | [QwenLM/Qwen3-TTS](https://github.com/QwenLM/Qwen3-TTS) | [EntryPoint_Qwen3-TTS](https://github.com/PegionFish/EntryPoint_Qwen3-TTS) | qwen3-tts-0.6b, qwen3-tts-12hz-1.7b-* |
| `animevideo` | [xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) | [EntryPoint_Real-ESRGAN/adapters/animevideo](https://github.com/PegionFish/EntryPoint_Real-ESRGAN) | animevideo-xsx2-pth |
| `realesr` | [xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) | [EntryPoint_Real-ESRGAN/adapters/realesr](https://github.com/PegionFish/EntryPoint_Real-ESRGAN) | realesr-animevideov3-* |
| `rembg` | [danielgatis/rembg](https://github.com/danielgatis/rembg) | [EntryPoint_rembg](https://github.com/PegionFish/EntryPoint_rembg) | u2net, u2netp, isnet-anime, u2net_human_seg, u2net_cloth_seg 等 |
| `rife` | [nihui/rife-ncnn-vulkan](https://github.com/nihui/rife-ncnn-vulkan) | [EntryPoint_rife-ncnn-vulkan](https://github.com/PegionFish/EntryPoint_rife-ncnn-vulkan) | rife-v46-ncnn |

## 目录位置与结构

反向同步的目的地：`/home/bob/EntryPoint_Models/EntryPoint_<模型>/`（不在主仓库内，各自独立 `git init`）。

```
/home/bob/EntryPoint_Models/
├── EntryPoint_BiRefNet/      # upstream: ZhengPeng7/BiRefNet
│   ├── adapter.py            # HTTP 推理适配器（与 modules/<id>/ 完全一致）
│   ├── module.toml           # EntryPoint 模块清单
│   ├── requirements*.txt
│   ├── LICENSE               # 与上游一致的译本（许可证兼容）
│   ├── README.md             # 模块用法文档
│   ├── ADAPTER.md            # 适配层总览（血缘、同步方式）
│   └── upstream.json         # 同步元数据（upstream/版本/source commit）
├── EntryPoint_Real-ESRGAN/   # 上游一项目多模型 → 含两个适配器
│   └── adapters/{animevideo,realesr}/
└── ...
```

每个 repo 的 git remote：
- `origin` → `git@github.com:PegionFish/EntryPoint_<模型>.git`（已由 gh 创建并推送，default 推送目标）
- `upstream` → 对应**上游原版**（只读对照，如 SYSTRAN/faster-whisper）

## 已发布 GitHub 位置

全部 10 个插件 repo 已用 `gh repo create ... --public` 创建并推送（重复执行无副作用；改名用 `gh repo rename`）：

```bash
gh repo create PegionFish/EntryPoint_<模型> --public --description "EntryPoint 适配层：..."
```

## 一次性同步工作流

复用脚本 `scripts/sync-model-repos.sh`：

```bash
# 全量：把当前主仓库所有 modules/ 反向刷新到 EntryPoint_Models/ 并提交
./scripts/sync-model-repos.sh

# 单个模块：只刷新 faster-whisper
./scripts/sync-model-repos.sh --only faster-whisper

# 同步并推送到各 repo 的 origin（Push 目标已指向 PegionFish/EntryPoint_<模型>）
./scripts/sync-model-repos.sh --push
```

等价手动流程：

```bash
rm -rf /home/bob/EntryPoint_Models/EntryPoint_faster-whisper/*
cp -r modules/faster-whisper/. /home/bob/EntryPoint_Models/EntryPoint_faster-whisper/
cd /home/bob/EntryPoint_Models/EntryPoint_faster-whisper
git add -A && git commit -m "sync(adapter): $(date +%Y-%m-%d) from EntryPoint main"
git push origin main
```

## 一次性同步工作流

复用脚本 `scripts/sync-model-repos.sh`：

```bash
# 全量：把当前主仓库所有 modules/ 反向刷新到 EntryPoint_Models/ 并提交
./scripts/sync-model-repos.sh

# 单个 repo：只刷新 faster-whisper
./scripts/sync-model-repos.sh --only faster-whisper

# 连同 GitHub 一起推送（需 origin 已配置）
./scripts/sync-model-repos.sh --push
```

等价手动流程：

```bash
rm -rf /home/bob/EntryPoint_Models/faster-whisper/*
cp -r modules/faster-whisper/. /home/bob/EntryPoint_Models/faster-whisper/
cd /home/bob/EntryPoint_Models/faster-whisper
git add -A && git commit -m "sync(adapter): $(date +%Y-%m-%d) from EntryPoint main"
git push origin main
```

## 反向约束（设计约定）

1. **适配层单向流动**：主仓库 `modules/` 是唯一事实源（single source of truth）；反向 repo 只读同步。
2. **不含模型权重**：`module.toml` 只声明下载源（HF/ModelScope/GitHub release），权重走
   `models/`（见 `docs/DEPLOYMENT.md`），反向 repo 不携带大文件。
3. **`upstream.json` 记录血缘**：每个反向 repo 的 `upstream.json` 记录 `source commit`（主仓库当时 HEAD），
   保证可追溯某次适配对应的主仓库版本。
4. **改名需三处一致**：`modules/<id>/` 目录名、`module.toml [module].id`、反向 repo 名（`EntryPoint_<模型>`）。
5. **许可证兼容**：每个反向 repo 的 `LICENSE` 取上游原文（与上游 SPDX 一致：MIT / Apache-2.0 / BSD-3-Clause），
   `module.toml [module].license` 与之对齐。

## 常见问题

- **想对照上游原版差异？** `git diff upstream/main -- adapter.py`（需要先 `git fetch upstream`）。
- **主仓库模块更新了但反向 repo 没变？** 重跑 `sync-model-repos.sh` 即可，脚本只改动文件并新增提交，不会改写历史。
- **Q: 为什么 Real-ESRGAN 一个 repo 两个适配器？** 上游把 AnimeVideo 与 RealESR 两个模型族放在同一项目（共享 `realesrgan` 代码），单独拆分会失去上游对照价值，故同一个 repo 内分 `adapters/` 子目录。
- **Q: GitHub 许可证徽标没显示？** `gh repo view --json licenseInfo` 若为 null，是 GitHub 索引延迟，检查 repo 根目录是否存在 `LICENSE` 文件即可。
