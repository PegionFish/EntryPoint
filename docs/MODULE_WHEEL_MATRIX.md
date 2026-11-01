# 模块 torch/ncnn 供给矩阵（演进计划·计划1-M3 / 计划3 基建）

> 面向模块作者：声明 `requirements.txt` 时如何按目标硬件选 torch 版本与
> wheel 索引；ncnn-vulkan 路线的平台供给现状。版本锚点与
> `config/constraints.txt`【torch 全家桶】联动——**改 torch 版本必须两处同步**
> （constraints 锁版本，模块 requirements 声明索引与用途注释）。

## 1. torch × CUDA compute capability 矩阵

| 目标硬件 | cc | cc 兼容 / 推荐构建 | wheel 平台供给（2026-11-01） | 说明 |
|---|---|---|---|---|
| Tesla P4 / Pascal | sm_61 | PyPI 默认（cu126 内含）或 cu126 索引 | Linux x86_64 / win_amd64 ✅（cu126 索引；PyPI 的 win_amd64 轮为 CPU 构建） | 老卡全系列支持；float16 部分算力受限（faster-whisper 已有 int8 回退先例） |
| Ampere～Ada（30/40 系） | sm_80–sm_89 | PyPI 默认（当前 = cu126 变体）或 cu126 | Linux x86_64 / win_amd64 ✅（Windows 同样须走 cu 索引） | 主流覆盖 |
| Blackwell（RTX 50 系） | sm_120 | **cu128+ 索引**（torch ≥2.7），仓内锚点 cu130（torch 2.11）| Linux x86_64 / win_amd64 ✅（cu128 / cu130 索引） | PyPI 默认变体不含 sm_120（deep-filter/requirements.txt 先例） |
| GB10（RTX Spark，Windows） | sm_121 | ✅ **cc 兼容**：cu128+ / nightly，sm_120 内核经同主版本 PTX 前向兼容 | ❌ **win_arm64 无 torch CUDA 轮**（stable cu128 / cu130、nightly cu130 均无，§1.3） | **cc 兼容 ≠ 可装**：RTX Spark 的 Windows 宿主为 ARM64，当前无 torch GPU 安装路径；sm_121 无独立 wheel；未知 cc 已按「记录+告警不拒绝」处理（compute/cuda.rs） |
| WoA（Snapdragon aarch64 Windows） | — | CUDA 13.4 WoA 已 GA（2026-09，仅 NVIDIA GPU 机型适用，见上行）/ **ONNX-DirectML 模块级回退**（无 win_arm64 轮，需自建） | win_arm64 仅 CPU 轮（torch 官方 CPU 索引 / nightly cpu、onnxruntime CPU EP）；onnxruntime-gpu / -directml 无（§1.3） | 生态未成熟，新模块默认声明 cpu+回退路径，torch 路线待跟进 |
| 纯 CPU | — | PyPI 默认即 CPU 构建 | x86_64 ✅（PyPI）；win_arm64 PyPI 无，仅官方 CPU 索引 / nightly cpu（§1.3） | 无需索引 |

### 1.1 wheel 索引写法（requirements.txt）

```
--extra-index-url https://download.pytorch.org/whl/cu130   # Blackwell/GB10
torch==2.11.0        # 与 config/constraints.txt【torch 全家桶】同步修改
```

- 索引行只写进模块 requirements（constraints 文件不允许索引选项）；
- PyPI 默认变体已满足的（Pascal～Ada）**不要**加索引行——避免拖全量
  CUDA wheel；
- 平台差异走 `post-install.sh` / 环境探测的先例见 faster-whisper（sm_61
  float16 失败回退 int8）。

### 1.2 兼容验证清单（新模块上架前）

1. `python_version` 约束声明（现役模块口径 `>=3.10,<3.13`；3.14 未被生态
   覆盖前不要放开）；
2. torch 版本与 constraints.txt 一致（多模块同版本 → uv 缓存硬链接去重生效）；
3. 目标 cc 下真实推理一次（sm_61 服务器 / 用户真机回报），记录到模块
   CONTRACT/README 占用表。

### 1.3 win_arm64 wheel 平台供给实查（2026-11-01）

§1 表的「cc 兼容」列只回答「内核能否在该 GPU 上跑」；能否装上还取决于宿主平台有没有
对应 wheel（「wheel 平台供给」列）。WoA（含 RTX Spark / GB10 的 Windows）宿主为 ARM64，
平台标签 `win_arm64`：

| 组件 | win_arm64 供给 | 查证来源 |
|---|---|---|
| torch stable（PyPI） | ❌ 历代版本均无（最新 2.15.0 仅 win_amd64，且为 CPU 构建） | PyPI JSON API |
| torch stable（官方 CPU 索引） | ✅ 仅 CPU：2.7.0～2.15.0+cpu（cp311–313），**缺 2.11.x**——仓内 constraints 锚点 `torch==2.11.0` 无 win_arm64 轮 | download.pytorch.org/whl/cpu |
| torch nightly cpu | ✅ 2.15.0.dev～2.16.0.dev20261031+cpu（cp311–313） | download.pytorch.org/whl/nightly/cpu |
| torch CUDA（stable cu128 / cu130、nightly cu130） | ❌ 无 | download.pytorch.org/whl/{cu128,cu130,nightly/cu130} |
| onnxruntime-gpu / onnxruntime-directml | ❌ 无（1.31.1 / 1.24.4 仅 win_amd64） | PyPI JSON API |
| onnxruntime（CPU EP） | ✅ 1.24.2 起（1.31.1：cp311–314） | PyPI JSON API / simple 索引 |
| cuda-python | ✅ 可装：13.4.4 本身是 py3-none-any 元包，依赖的 cuda-bindings 13.4.4 / cuda-core 1.2.2 均有 win_arm64 轮（cp311–315；分别自 13.4.0b1 / 1.2.1 起）；真机功能未验证 | PyPI JSON API / simple 索引 |
| ctranslate2 | ❌ 无（4.8.4 的 Windows 轮仅 win_amd64） | PyPI JSON API |
| CUDA Toolkit 13.4 WoA | ✅ 已 GA（2026-09，首个支持 WoA 的版本）；T-66 已解包 13.4.2 安装器，数学库为 ARM64 DLL（清单见 CONFIG_REFERENCE §1.2） | NVIDIA 安装器 |

结论：win_arm64 上**没有任何 torch / onnxruntime 的 GPU 轮**——cc 兼容不等于可装，GPU 路径
需走非 wheel 方案（ncnn-vulkan / 自建 / llama.cpp 等，跟踪 E4 批次）。Python 直连 CUDA
可用 cuda-python（win_arm64 轮已出现）或 stdlib ctypes 加载驱动自带的 `nvcuda.dll`。

## 2. ncnn-vulkan 供给现状（vulkan 后端路线）

- **x86_64**：nihui 上游 release 提供 windows/linux 预构建，模块按
  `bin/<os>-<arch>/` 约定由用户放置（waifu2x/real-esrgan adapter 先例）。
- **linux-aarch64**：上游**无预构建**。两条路线：
  1. 源码构建（aarch64 Linux 主机或交叉）：
     `git clone https://github.com/Tencent/ncnn && cmake -B build -DNCNN_VULKAN=ON -DNCNN_BUILD_TOOLS=ON && cmake --build build`
     产物（`build/src/libncnn*` + `build/tools`）放置到模块
     `bin/linux-aarch64/`；
  2. 等待上游/社区出 aarch64 产物（跟踪 nihui releases）。
- **GB10 形态**：ncnn 走 Vulkan 而非 CUDA，理论上覆盖 sm_121；驱动层
  Vulkan ICD 随 NVIDIA 驱动安装，待真机验证后把结论回填本节。
- torch nightly 路线（GB10 备选）：nightly 的 sm_120 内核覆盖 sm_121，
  pin 方案 = requirements 写 `--extra-index-url .../whl/nightly/cu130` +
  注释声明「nightly 不进 constraints，仅实验模块使用」。注意这只是 cc 轴成立：
  nightly cu130 无 win_arm64 轮，Windows 版 GB10（RTX Spark）不适用（§1.3）。

## 3. 决策记录

- animevideo 家族官方无 ncnn/ONNX 权重发布 → 该模块不声明 vulkan 兜底
  （module.toml 注释 / 决策备忘录 ws-f §1.2）；
- deep-filter 的 cu130 索引先例 → Blackwell 支持的标准写法；
- 平台标识口径：`windows-x86_64 / linux-x86_64 / linux-aarch64`（MODULE_SPEC
  §平台一节）；windows-aarch64 待 WoA 路线成熟后新增。
