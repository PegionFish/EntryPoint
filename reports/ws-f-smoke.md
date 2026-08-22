# WS-F 冒烟报告：video-upscale / video-interp 引擎与模型落地 + 直连冒烟（E6/E8 前置）

> 波次：W1 | 代理：WS-F（执行） | 日期：2026-08-22
>
> 范围：任务书 1-4（引擎二进制落地、模型落地、不经 daemon 的直连冒烟、契约最小修正）。
> 独占写路径遵守：modules/video-{upscale,interp}/bin/**、models/（manifest target_dir）、本报告。
> 未触碰 runtime/、daemon、其他模块（直连测试只读借用了既有 rembg venv 解释器跑 adapter 函数，未改动）。

---

## 1. 落地文件树（两级）

```
modules/video-upscale/
├── bin/linux-x86_64/
│   └── realesrgan-ncnn-vulkan        # 11.4 MB, chmod +x, 与 module.toml [runtime.binaries] 键一致
└── (module.toml/adapter.py 仅契约修正，见 §4)

modules/video-interp/
├── bin/linux-x86_64/
│   └── rife-ncnn-vulkan              # 9.9 MB, chmod +x, 键一致
└── (adapter.py 仅契约修正，见 §4)

models/                                # 平台模型根（EP_MODELS_ROOT）
├── video-upscale-realesr-animevideov3-x4-ncnn/    # = manifest [[models]] target_dir（整包 models/ 平铺落地）
│   ├── realesr-animevideov3-x2.{param,bin}        # 上游包内全部 5 组模型保留，
│   ├── realesr-animevideov3-x3.{param,bin}        #   -n + -s 由引擎按 x{scale} 解析
│   ├── realesr-animevideov3-x4.{param,bin}        # ← 声明变体（4 KB param + 1.2 MB bin）
│   ├── realesrgan-x4plus-anime.{param,bin}
│   └── realesrgan-x4plus.{param,bin}              # 共 45 MB
└── video-interp-rife-ncnn/                        # = target_dir
    ├── LICENSE                                    # MIT 随附
    └── models/                                    # 保持「models/<rife-*/>」adapter 查找布局
        ├── rife-v4.6/{flownet.param, flownet.bin(10.3MB)}   # ← 默认模型子目录
        ├── rife-UHD/ rife-anime/ rife-HD/          # 其余历代目录全量保留
        └── rife-v2* rife-v3* rife-v4/              #   （params.model_name 可选，README 承诺）
                                                    # 共 431 MB
```

注：rife 整包解压根目录的引擎二进制同名文件未拷入模型目录（仅取 `rife-*` 模型子目录）。

## 2. 直链与 sha256 表

GitHub 直连超时（SSL_read EOF @90s），按任务预案改用 gh-proxy 前缀 `https://gh-proxy.com/`
（实测可用；另试 ghfast.top 未及使用）。资产与决策备忘录附录清单一致：

| 资产 | 直链（实际下载 = gh-proxy 前缀 + 原始 URL） | 大小 | sha256 |
|---|---|---|---|
| realesrgan-ncnn-vulkan-20220424-ubuntu.zip | `https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0/realesrgan-ncnn-vulkan-20220424-ubuntu.zip` | 45 MB (46,878,712 B) | `e5aa6eb131234b87c0c51f82b89390f5e3e642b7b70f2b9bbe95b6a285a40c96` |
| rife-ncnn-vulkan-20221029-ubuntu.zip | `https://github.com/nihui/rife-ncnn-vulkan/releases/download/20221029/rife-ncnn-vulkan-20221029-ubuntu.zip` | 412 MB | `1e2c7ee7fa7daa326542d50622f0afedc80cf6f1858bda411d16385ffa5cdf68` |

上游未发布官方 sha256，以上为下载实测值，供镜像化（备忘录 §5）时比对基线。
两二进制 ldd 动态链接解析完整。

## 3. 冒烟命令与结果

环境：ffmpeg n9.0.1；Vulkan 设备 GPU0=Intel ARL iGPU / GPU1=RTX 5090 D / GPU2=RX 7900 XTX；
两引擎均默认 auto→GPU0(Intel ARL)。测试源：
`ffmpeg -f lavfi -i testsrc=duration=3:size=320x240:rate=25 → /tmp/opencode/e8src.mp4`（75 帧@25fps）

### a) 超分 realesrgan-ncnn-vulkan ×4

单帧三种调用形态均通过（320x240 → **1280x960**）：

```bash
realesrgan-ncnn-vulkan -i in.png -o out.png \
  -m models/video-upscale-realesr-animevideov3-x4-ncnn \
  -n realesr-animevideov3 -s 4      # 形态A：上游原生名，rc=0, 0.97s
  -n realesr-animevideov3-x4 -s 4   # 形态B：任务书字面名，rc=0, 0.97s ✓
  -s 4                              # 形态C：adapter 现行调用（缺省-n），rc=0 ✓
```

全序列目录模式（同 adapter 参数形）：`-i f_in(75帧) -o f_out4x -m <target_dir> -s 4 -t 0 -f png`
→ **75/75 帧、1280x960、3.43s**（注意：输出目录须预先存在，否则报 invalid outputpath extension type）。

### b) 插帧 rife-ncnn-vulkan ×2（rife-v4.6）

```bash
rife-ncnn-vulkan -i f_in(75帧) -o mids -m models/video-interp-rife-ncnn/models/rife-v4.6
```
→ rc=0，**输出 150 帧（%08d 从 1 起）**，2.29s。内容校验：奇数位帧 vs 原帧 PSNR≈40.6dB
（原帧经管线重渲染非逐位相同）；回封 `-framerate 50` → **150 帧 @50fps = 精确翻倍**。

### c) 修复后 adapter 全管线直连验证（不经 daemon；借 rembg venv 只读解释器调 run_* 函数）

| 模块 | 结果 |
|---|---|
| video-upscale (vulkan) | `run_upscale(e8src.mp4, {scale_factor:4, model:realesrgan-animevideov3-x4-ncnn})` → 输出 mp4 **1280x960 @25fps 75 帧**，wall 3.9s |
| video-interp (vulkan) | `run_interpolate(e8src.mp4, {multiplier:2})` → 输出 mp4 **320x240 @50fps 150 帧**（out_frames=150），wall 1.5s |

## 4. 契约修正列表【契约修正】

依据任务书第 4 条「发现出入时最小修正」，共 3 处必要修正（均为小修，已实测回归）：

1. **modules/video-upscale/adapter.py `_extract_frames`**：`-vsync 0` → `-fps_mode passthrough`。
   本机 ffmpeg ≥7 已移除 `-vsync`（n9.0.1 直接报 Unrecognized option），原命令在目标环境必然失败。两模块同修。
2. **modules/video-interp/adapter.py `interp_frames_ncnn`**：移除本地 N-1 中间帧交错重组，
   直接采用引擎目录模式产物并校验 `produced == cur_count * 2`。
   实测钉定版本 20221029 目录模式已原生输出完整 2N 序列（含重渲染原帧、1-based 命名、支持 `-n 目标帧数`）；
   原「N 入 → N-1 出」假设对应旧版引擎，按现行为会触发帧数断言 → 500。同时补 `out_dir.mkdir`（引擎要求输出目录已存在）。
3. **modules/video-interp/adapter.py `run_interpolate`**：`params.model_name` 缺省值由空串改为 `"rife-v4.6"`。
   留空会使 find_ncnn_model_dir 按字典序误选 rife-HD（ASCII 大写在前），静默用错模型；rife-v4.6 为 manifest default=true 变体。

未改项说明：video-upscale adapter 不传 `-n` 现可正确工作——引擎缺省名即 `realesr-animevideov3`，
加载时按 `-s` 自动追加 `-x{scale}` 后缀（形态 A/C 实测等价）；暂不加显式映射以免过度修改（见 §5 待办）。
module.toml 无需改动：[runtime.binaries] 相对路径与落地完全一致；[[models]] url/target_dir 与落地一致。

## 5. 未决问题

1. **size_estimate 校准**：esrgan 模型目录实测 45MB 与 manifest 一致；interp 解压后 431MB 略超按 zip 体积估的 411MB（解压膨胀），建议 E8 前把该条目 size_estimate_mb 上修至 ~430 以免平台配额误判。另：平台若支持 param+bin 双文件变体条目可免整包下载（备忘录 §5.4，非本次范围）。
2. **GPU 选择策略**：两引擎默认 auto=GPU0（Intel ARL iGPU）。E8 若需指定 dGPU（5090/7900XTX），经 `-g gpu-id` 注入即可；是否纳入 [compute.env] 词表待编排者定夺。
3. **rife 多轮 pass（multiplier≥4）**：修复后 pass N 以引擎输出为输入再翻倍，路径成立但 >2x 未实测（本轮范围 ×2）；建议 E8 补 multiplier=4 用例。
4. **gh-proxy 可用性时效**：镜像前缀随时间可能失效，sha256 已留档供校验替代源；长期仍以备忘录 §5 自建镜像建议为准。
5. **torch/OV 分支不受本次影响**：仍按既定边界返回 501 EXPERIMENTAL（E6/E7 载体另行推进）。
