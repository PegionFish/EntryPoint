# WS-B · faster-whisper ROCm 后端调研落档（E1 准备）

> 日期：2026-08-22 | 负责流：W1/WS-B | 关联：HETERO_DIST_PLAN §3.1(faster-whisper 增强)/§5(E1)、MODULE_SPEC §2.2/§2.3/§2.6
>
> 本机参考环境：AMD RX 7900 XTX（gfx1100，RDNA3）/ ROCm 7.2.4（`/opt/rocm` 最小安装）/ Python 3.14（实验 venv 另选）

---

## 1. 结论速览

| 问题 | 结论 |
|---|---|
| CTranslate2 ≥4.5 官方 PyPI wheel 是否含 HIP 后端？ | **否。PyPI 分发的一直是 CUDA-only 构建**（`WITH_HIP=OFF` 默认值）。HIP 后端自 **v4.7.0**（2026-02-03，PR #1989 合入）起才存在 |
| ROCm 专用 wheel 从哪拿？ | **OpenNMT/CTranslate2 官方 GitHub Releases**，自 v4.7.0 起每个 release 附 `rocm-python-wheels-Linux.zip`（Linux）/ `rocm-python-wheels-Windows.zip`；zip 内含 cp310–cp314 manylinux x86_64 全套轮子 |
| 有无 `--extra-index` / AMD 维护的 pip 源？ | **无。** pip 无法直接引用 zip 内轮子 → 本模块采用「requirements 先装 PyPI 同版本占位 + setup 脚本下载 Release 轮子 `--force-reinstall` 同版本覆盖」两步法 |
| AMD 官方路线？ | 旧路线（2024-10 博客时代）：AMD 维护的 fork `ROCm/CTranslate2` + Docker 镜像（基于 CT2 v3.23）；已被上游 v4.7.0 吸收取代。jordimas fork 的 GHCR container 是上游发布流程的中转预览 |
| gfx1100（RX 7900 XTX）是否需要 `HSA_OVERRIDE_GFX_VERSION`？ | **不需要。** wheel 已把 gfx1100 编译进内置 GPU 内核（覆盖清单见 §3）；仅未收录架构（如 RDNA2 gfx1032）才需 override + `CT2_CUDA_ALLOCATOR=cub_caching` |
| device 名？ | HIP 下仍用 `device="cuda"`（透明映射），adapter.py 现有 `"rocm"→"cuda"` 映射正确，多卡用 `HIP_VISIBLE_DEVICES` 选卡 |
| 源码构建兜底可行吗？ | 可行但仅作 fallback：需 hip-devel/amdclang++/OpenBLAS 等，编译估时 **1–2 小时级**（详见 §4）。wheel 路线通畅时不采用 |

## 2. 版本线与分发形态证据

| CT2 版本 | 发布日 | ROCm 相关内容 |
|---|---|---|
| ≤4.6.3 | — | 无 HIP 后端；PyPI wheel 仅 CUDA |
| v4.7.0 | 2026-02-03 | **首个含 ROCm/HIP**（PR #1989 `WITH_HIP`）；release 附 `rocm-python-wheels-Linux.zip`（270.9 MB） |
| v4.7.1 | 2026-02-04 | Windows 构建修复；社区实测 Strix Halo (gfx1151) 开箱即用 |
| v4.7.2 | 2026-05-18 | CI 源 ROCm 7.2 → 7.2.1 |
| v4.8.0 | 2026-06-06 | 常规迭代 |
| **v4.8.1（本模块 pin）** | 2026-07-03 | 最新稳定。Linux zip：284,164,672 B（~271 MB）、sha256 `2b454399aace4c76fe373e912f8d6a0d2033d6aa58dbfd438840aceca7cc64db` |

v4.8.1 Linux 轮子直链：
```
https://github.com/OpenNMT/CTranslate2/releases/download/v4.8.1/rocm-python-wheels-Linux.zip
```

**wheel 不自带 HIP runtime（Linux）**：PR #1989 作者明确 Linux 需宿主机 `$ROCM_PATH/lib` 提供
`libamdhip64.so` 等库，且需 OpenMP 运行库 `$ROCM_PATH/lib/llvm/lib/libomp.so`。

## 3. wheel 内置 GPU 架构清单（决定 HSA_OVERRIDE_GFX_VERSION 取舍）

gfx803, gfx900, gfx906, gfx908, gfx90a, gfx942, gfx950,
gfx1030, **gfx1100**, gfx1101, gfx1102, gfx1150, gfx1151, gfx1200, gfx1201

- 本机 **gfx1100 ∈ 清单 → 不设置 override**（module.toml `[compute.env]` 注释已说明）
- 未收录架构示例：RDNA2 gfx1032（RX 6600）→ `HSA_OVERRIDE_GFX_VERSION=10.3.0` 且必须加
  `CT2_CUDA_ALLOCATOR=cub_caching`（默认 MallocAsync 异步分配器在 RDNA2 上崩溃）

## 4. 源码构建依赖清单与耗时评估（fallback，不推荐首选）

```bash
# 前置（系统层，apt，AMD repo）：rocm-dev / hip-devel（提供 amdclang++、hiprand、hipblas 头文件与库）、
#                              libopenblas-dev、python3-dev、cmake>=3.21
# 编译（issue #2012 社区配方改写至 gfx1100）：
cmake -DCMAKE_C_COMPILER=amdclang -DCMAKE_CXX_COMPILER=amdclang++ \
      -DWITH_HIP=ON -DWITH_MKL=OFF -DWITH_DNNL=OFF -DWITH_OPENBLAS=ON \
      -DOPENMP_RUNTIME=COMP -DCMAKE_HIP_ARCHITECTURES=gfx1100 -DGPU_TARGETS=gfx1100 \
      -DCMAKE_BUILD_TYPE=Release -DBUILD_TESTS=OFF ...
```

- 耗时评估：HIP kernel 编译为主，桌面 CPU 约 **1–2 小时**（社区报告量级）；wheel 路线失败才启用
- 结论：pip/Release-wheel 路线当前通畅（多个社区真机数据点），源码构建仅登记备查

## 5. 本机就绪度实测（2026-08-22，含 setup 脚本真机运行结果）

| 检查项 | 结果 |
|---|---|
| `/opt/rocm` 存在 + `rocminfo` | ✅ 7.2.4，gfx1100 agent 可见（驱动层 OK） |
| `ldconfig` 含 HSA runtime | ✅ libhsa-runtime64.so.1 → /opt/rocm/lib |
| `libamdhip64.so*`（CT2-HIP wheel 加载必需） | ❌ **全盘缺失**（`find /opt/rocm -name "libamdhip64*"` 为空；ldconfig 无条目） |
| `/opt/rocm/lib/llvm/lib/libomp.so` | ❌ 缺失（最小安装只含 rocm-core/rocminfo/rocm-smi/HSA/hsakmt） |
| ROCm apt 源配置 | ❌ sources.list.d 无 rocm 条目 |

→ **E1 硬前置**（WS-B 写权限之外，移交编排者/E1 执行者）：补装 HIP runtime 库，例如
`sudo apt install hip-runtime-amd`（或 `hip-libraries` 元包，按 AMD repo 命名）并确认
`ldconfig -p | grep amdhip64` 与 `/opt/rocm/lib/llvm/lib/libomp.so` 就位。
在此之前 CT2 HIP wheel 连 `import ctranslate2` 都会 dlopen 失败——setup/verify 脚本对此有结构化诊断。

**setup-rocm.sh 真机运行记录（2026-08-22）：**

- venv 层全绿：requirements-rocm.txt 解析安装成功（fastapi 0.141 / faster-whisper 1.2.1 /
  ctranslate2 4.8.1 PyPI 占位），Release zip 下载后 **sha256 与 GitHub API 摘要逐字一致**
  （`2b4543…64db`）；cp314 轮子从 zip 的 `temp-linux/` 子目录正确解出并 `--force-reinstall`
  同版本覆盖成功——两步法全链路验证通过
- 摘要步按预期结构化失败（exit 3）：`ImportError: libhiprand.so.1: cannot open shared object file`
  ——HIP 轮子的首个缺失宿主库是 `libhiprand.so.1`（同属 hip-runtime-amd 包），
  与 §5 上表的缺库结论互相印证；补装 HIP runtime 后重跑即可走通
- 备注：本机直连 GitHub 仅 ~9 KB/s；因有官方 sha256 锚定，经加速镜像下载不损失完整性，
  实测 gh-proxy.com 前缀 31 秒完成 271 MB。脚本本身仍默认直连 URL

## 6. 本模块落地方式

- `modules/faster-whisper/requirements-rocm.txt`：基础依赖与 requirements.txt 对齐；
  `ctranslate2==4.8.1` 为 **占位 pin**（PyPI 上是 CUDA 构建）——保证平台 M2/M3
  `uv pip install -r` 可解析、deps hash 稳定；随后由
  `scripts/hetero/whisper-rocm/setup-rocm.sh` 用 GitHub Release v4.8.1 的 cpXX HIP 轮子
  `--force-reinstall` 同版本覆盖（两步法，zip 不能被 pip 直接引用所致）
- `scripts/hetero/whisper-rocm/setup-rocm.sh`：/tmp 自建 venv → 装 requirements-rocm.txt →
  下载校验 zip → 解出匹配 cp 标签轮子覆盖安装 → 打印 ct2 版本与 CUDA(=HIP) 设备数/支持算精度
- `scripts/hetero/whisper-rocm/verify-rocm.py`：加载 large-v3（EP_MODEL_DIR 或 argv），合成正弦 wav，
  `device="cuda"` 全链路转写，打印 segments 数/首段文本/实际设备；`--dry` 只做运行时诊断不推理
- module.toml：`[runtime] requirements_by_backend = { rocm = ..., cuda = ... }`；
  cpu 无条目回退 requirements.txt（CPU 兜底口径）

## 7. 证据链接

1. CT2 安装文档（PyPI wheel=CUDA 12 口径、`WITH_HIP` 构建开关）：https://opennmt.net/CTranslate2/installation.html
2. PyPI ctranslate2 页（明示 AMD 用户去 releases 页拿专用轮子）：https://pypi.org/project/ctranslate2/
3. PR #1989 Introduce AMD GPU support with ROCm HIP（Linux/Windows 宿主依赖、目标 ROCm 7.x）：https://github.com/OpenNMT/CTranslate2/pull/1989
4. Releases 页（v4.7.0 起附 rocm-python-wheels-Linux.zip；v4.8.1 资产与 sha256 经 GitHub API 核实）：https://github.com/OpenNMT/CTranslate2/releases
5. v4.8.1 Linux zip 直链：https://github.com/OpenNMT/CTranslate2/releases/download/v4.8.1/rocm-python-wheels-Linux.zip
6. SYSTRAN/faster-whisper issue #1370（gfx 内置清单、device="cuda" 透明映射、社区真机确认汇总）：https://github.com/SYSTRAN/faster-whisper/issues/1370
7. issue #2012（RDNA2 override + cub_caching 配方 + 源码构建命令）：https://github.com/OpenNMT/CTranslate2/issues/2012
8. WhisperLive ROCm_whisper.md（官方轮子替换 PyPI 轮子的标准操作）：https://github.com/collabora/WhisperLive/blob/main/ROCm_whisper.md
9. AMD ROCm 博客（旧 fork+Docker 路线，已过时备案）：https://rocm.blogs.amd.com/artificial-intelligence/ctranslate2/README.html
10. 社区 Strix Halo 全流程配方（v4.7.x 开箱即用佐证）：https://github.com/nabe2030/faster-whisper-rocm-strix-halo
