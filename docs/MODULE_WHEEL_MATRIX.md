# 模块 torch/ncnn 供给矩阵（演进计划·计划1-M3 / 计划3 基建）

> 面向模块作者：声明 `requirements.txt` 时如何按目标硬件选 torch 版本与
> wheel 索引；ncnn-vulkan 路线的平台供给现状。版本锚点与
> `config/constraints.txt`【torch 全家桶】联动——**改 torch 版本必须两处同步**
> （constraints 锁版本，模块 requirements 声明索引与用途注释）。

## 1. torch × CUDA compute capability 矩阵

| 目标硬件 | cc | 推荐构建 | 说明 |
|---|---|---|---|
| Tesla P4 / Pascal | sm_61 | PyPI 默认（cu126 内含）或 cu126 索引 | 老卡全系列支持；float16 部分算力受限（faster-whisper 已有 int8 回退先例） |
| Ampere～Ada（30/40 系） | sm_80–sm_89 | PyPI 默认（当前 = cu126 变体）或 cu126 | 主流覆盖 |
| Blackwell（RTX 50 系） | sm_120 | **cu128+ 索引**（torch ≥2.7），仓内锚点 cu130（torch 2.11）| PyPI 默认变体不含 sm_120（deep-filter/requirements.txt 先例） |
| GB10（RTX Spark，Windows） | sm_121 | **cu128+ / nightly**：sm_120 内核经同主版本 PTX 前向兼容 | sm_121 无独立 wheel；未知 cc 已按「记录+告警不拒绝」处理（compute/cuda.rs） |
| WoA（Snapdragon aarch64 Windows） | — | CUDA 13.4 preview 早期 / **ONNX-DirectML 模块级回退** | 生态未成熟，新模块默认声明 cpu+回退路径，torch 路线待跟进 |
| 纯 CPU | — | PyPI 默认即 CPU 构建 | 无需索引 |

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
  注释声明「nightly 不进 constraints，仅实验模块使用」。

## 3. 决策记录

- animevideo 家族官方无 ncnn/ONNX 权重发布 → 该模块不声明 vulkan 兜底
  （module.toml 注释 / 决策备忘录 ws-f §1.2）；
- deep-filter 的 cu130 索引先例 → Blackwell 支持的标准写法；
- 平台标识口径：`windows-x86_64 / linux-x86_64 / linux-aarch64`（MODULE_SPEC
  §平台一节）；windows-aarch64 待 WoA 路线成熟后新增。
