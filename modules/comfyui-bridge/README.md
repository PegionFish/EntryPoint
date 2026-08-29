# ComfyUI 桥接模块(comfyui-bridge)

> 版本 0.1.0 | 契约:[CONTRACT.md](CONTRACT.md) | 总体计划:[docs/COMFYUI_BRIDGE_PLAN.md](../../docs/COMFYUI_BRIDGE_PLAN.md)

本模块把 EntryPoint 管线的产物自动发送给 **本地或远程的 ComfyUI 实例** 执行,并把生成结果取回管线继续流转。
平台**不拉起、不托管** ComfyUI 进程——模块只连接一个已运行的 ComfyUI(默认 `http://127.0.0.1:8188`),
把它的 HTTP API 包装成 EntryPoint 标准模块。

核心概念只有三个:

| 概念 | 说明 |
|---|---|
| **工作流模板** | 在 ComfyUI 中以 API 格式导出的 JSON 文件,上传到模块后按名字引用 |
| **inject 注入映射** | 一段 JSON,把管线产物(图片/文本)与常量(seed/steps 等)写进工作流的指定字段 |
| **能力 `generate`** | 同步阻塞全流程:上传输入 → 提交工作流 → 轮询取回产物 |

---

## 1. 快速开始(5 分钟)

> 目标:从"ComfyUI 已在运行"到"管线产出第一张结果图"。

### 1.1 启动 ComfyUI

以 aki 整合包为例(路径以你的实际安装位置为准):

```
/home/bob/Desktop/AI_Applications/ComfyUI-aki/ComfyUI-aki-v3/
```

- 整合包:使用整合包自带的启动脚本/启动器启动(不同整合包入口名称不同,以整合包说明为准)。
- ComfyUI 原生方式:在安装目录执行 `python main.py --port 8188`(默认端口即 8188)。

启动后验证 ComfyUI 已就绪:

```bash
curl -s http://127.0.0.1:8188/system_stats
# 期望:返回含 comfyui_version 的 JSON;浏览器打开 http://127.0.0.1:8188 能看到 ComfyUI 界面
```

> 注意:平台启动 EntryPoint 时 ComfyUI 可以尚未运行——模块的 `/health` 会返回 503
> (`comfyui unreachable`),这是预期行为。**先启动 ComfyUI,再跑管线即可**。

### 1.2 确认模块已自动发现

EntryPoint daemon 启动时自动扫描 `modules/` 目录,无需手动安装。验证:

- WebUI:模块页出现 **"ComfyUI 桥接"**;
- 或命令行(输出:`<待回填>`):

```bash
curl -s http://127.0.0.1:9800/api/modules | grep comfyui-bridge
```

### 1.3 上传(或确认)工作流模板

模块随包附带 3 个示例模板(`upscale_4x` / `style_transfer` / `txt2img`,见 §4)。
确认与上传均通过 WebUI 的 **工作流管理** 卡片:

1. 进入模块详情页(模块运行中);
2. 打开"工作流管理"卡片,查看已上传列表;
3. 点"上传"选择 ComfyUI 导出的 API 格式 `.json`(制作方法见 §2),上传后列表即时刷新;
   重名上传为覆盖(响应中 `replaced: true`)。

命令行等价方式(经 daemon 模块代理,输出:`<待回填>`):

```bash
curl -s http://127.0.0.1:9800/api/modules/comfyui-bridge/extra/workflows
```

### 1.4 跑通第一条管线

1. WebUI → 管线页,选择示例管线 `comfyui-demo`
   (配置文件 `config/pipelines/comfyui_demo.toml`:`file_input → comfyui-bridge.generate → file_output`);
2. 按管线要求上传一张输入图片;
3. 运行管线;
4. 在任务卡片观察 `EP-PROGRESS:NN%` 百分比进度(轮询估算,粒度较粗,见 §6);
5. 完成后下载产物——文件输出节点落盘的结果图。

若一切正常,你已完成"管线 → ComfyUI → 产物回流水线"的完整闭环。
若中途报错,直接跳到 §3.6 常见错误排查。

---

## 2. 工作流制作指南

ComfyUI 界面里搭好的图,必须以 **API 格式** 导出才能被本模块消费(普通界面格式不被接受)。

### 2.1 开启开发者模式

在 ComfyUI 网页界面:右上角 **设置(Settings) → 开发者模式选项 / Dev mode** 打开开关。
开启后导出菜单才会出现 **"Save (API Format)"** 按钮。

### 2.2 导出 API 格式工作流

搭好(或打开)一个工作流后,点击 **Save (API Format)**,得到一个 `.json` 文件
(建议命名如 `my_upscale.api.json`)。

> 注意与普通 **Save** 区分:普通 Save 保存的是界面格式(含画布坐标),
> 本模块上传校验会拒绝非 API 格式(报错 400)。

### 2.3 上传到模块

§1.3 的"工作流管理"卡片 → 上传 → 选择刚导出的 `.json`。规则:

- 服务端校验:必须为合法 JSON 且是 API 格式(顶层对象、每个值是含 `class_type` 与 `inputs` 的对象);
- 文件名清洗:仅保留 `[A-Za-z0-9._-]`,路径分量被剥离(防穿越);重名**覆盖**;
- 管线中用 `workflow` 参数引用时写**不含 `.json` 后缀的名字**(如 `my_upscale.api`)。

### 2.4 API 格式 JSON 结构解读(节点 id / inputs 字段名怎么读)

API 格式本质是:`{ "<节点id>": { "class_type": "<节点类名>", "inputs": { "<字段名>": <值> } }, ... }`

一个放大工作流的片段示例:

```json
{
  "1": { "class_type": "LoadImage",            "inputs": { "image": "example.png" } },
  "2": { "class_type": "UpscaleModelLoader",   "inputs": { "model_name": "RealESRGAN_x4plus.pth" } },
  "3": { "class_type": "ImageUpscaleWithModel","inputs": { "upscale_model": ["2", 0], "image": ["1", 0] } },
  "4": { "class_type": "SaveImage",            "inputs": { "filename_prefix": "ComfyUI", "images": ["3", 0] } }
}
```

怎么读:

| 你看到的 | 含义 |
|---|---|
| `"1"`、`"3"` | **节点 id**(导出时自动编号的字符串)。inject 键的第一段就是它 |
| `"class_type": "LoadImage"` | 节点类名,决定节点行为 |
| `"inputs": { "image": ... }` | **inputs 字段名**(如 `image` / `text` / `seed` / `steps`)是 inject 键的第三段 |
| `["2", 0]` | 连接类字段值:数组 = 引用上游节点 id 与其输出槽序号,**不是**字面量 |
| `"seed": 42` | 字面量字段(数字/字符串/布尔),可被 inject 覆盖 |

**找注入键的方法**:在 ComfyUI 界面点选目标节点 → 看它的输入参数名(界面名与 inputs 字段名基本一致)→
在导出的 JSON 里搜 `class_type` 定位节点 id → 注入键即 `<节点id>.inputs.<字段名>`。

> inject 的键指向 **inputs 字段**,而非连接。你**不能**用 inject 改变节点连线;
> 改连线请在 ComfyUI 里重新连图并重新导出。

---

## 3. inject 语法完整参考

`inject` 是 `generate` 能力的一个参数(string,内容为 JSON 对象)。
**键 = `<工作流节点id>.inputs.<字段名>`,值 = 来源表达式**。

### 3.1 四类来源表达式

| 表达式 | 语义 |
|---|---|
| `$input` | 首个上游产物文件 |
| `$input.<上游节点id>` | 定向引用指定上游节点的文件产物(多条输入边时必需) |
| `$input.<上游节点id>`(上游为文本产物) | 文本注入字符串字段(txt2img 提示词场景) |
| 字面量 | 数字/字符串/布尔常量(seed、steps 等) |

逐条示例(键中的节点 id 需与你的工作流一致):

| inject 键值对 | 场景 |
|---|---|
| `"3.inputs.image": "$input"` | 单输入管线:把上游唯一产物文件注入节点 3 的 `image` 字段 |
| `"5.inputs.image": "$input.ref"` | 多输入:定向取上游节点 `ref` 的**文件**产物注入节点 5 |
| `"7.inputs.text": "$input.prompt"` | 上游 `prompt` 节点产物为**文本**(如 `.txt`),内容注入节点 7 的 `text` 字段 |
| `"9.inputs.seed": 42` | 数字常量:固定随机种子 |
| `"7.inputs.text": "lowres, watermark"` | 字符串常量:固定负向提示词 |
| `"9.inputs.save": true` | 布尔常量 |

### 3.2 执行规则

1. 文件类来源先 `POST /upload/image` 上传、再把返回文件名写入字段;文本/字面量原样写入。
2. **注入前逐项校验键存在**(节点 id 必须在工作流中);缺失立即 400 报错并列出可用节点清单,不提交给 ComfyUI(D6)。
3. 未映射字段保留模板默认值;键天然唯一,无冲突。
4. `output_nodes` 指定取回哪些输出;全部下载到产物目录,第一个为主产物返回下游。

### 3.3 多组注入示例(双图 + 提示词 + 常量)

管线有两个上游:`input`(主图文件)与 `ref`(参考图文件),另有一个文本上游 `prompt`,
同时固定 KSampler 的种子与步数:

```json
{
  "3.inputs.image": "$input",
  "5.inputs.image": "$input.ref",
  "7.inputs.text":  "$input.prompt",
  "9.inputs.seed":  42,
  "9.inputs.steps": 28
}
```

解读:节点 3 与 5 是两个 LoadImage(分别收主图与参考图),节点 7 是 CLIPTextEncode(收提示词文本),
节点 9 是 KSampler。`$input`(不带后缀)取首个上游产物;带 `<上游节点id>` 的写法在多条输入边时**必需**。

### 3.4 txt2img 完整示例

文生图(随包模板 `txt2img.api.json` 的节点编号:2 = 正向 CLIPTextEncode、3 = 负向 CLIPTextEncode、5 = KSampler):
正向提示词来自文本上游节点 `input`(.txt 提示词文件),负向提示词与采样参数用常量写死:

```json
{
  "2.inputs.text": "$input.input",
  "3.inputs.text": "lowres, blurry, watermark",
  "5.inputs.seed": 42,
  "5.inputs.steps": 28
}
```

解读:`$input.input` = 定向引用上游节点 id `input` 的**文本**产物(示例管线用 file_input 读 .txt 提示词),
写入正向提示词字段;负向提示词与 KSampler 参数用字面量固定。
配套示例管线 `config/pipelines/comfyui_txt2img_demo.toml`(以实际文件为准):

```toml
[[nodes]]
id = "input"                 # 文本上游:file_input 读 .txt 提示词,节点 id 即 $input.input 中的 "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "generate"
kind = "module"
module_id = "comfyui-bridge"
capability = "generate"
timeout_secs = 1800          # 长任务务必放宽,见 §6.2
[nodes.params]
workflow = "txt2img"
inject = '{"2.inputs.text": "$input.input", "3.inputs.text": "lowres, blurry, watermark", "5.inputs.seed": 42}'
```

> 前提:ComfyUI 侧需已安装 txt2img 工作流引用的基础模型(SD1.5/SDXL 任一)。
> 平台不校验、也不负责模型下载——工作流内部引用什么模型归 ComfyUI 管(见 §6.3)。

### 3.5 output_nodes:多输出取回

工作流可以有多个 SaveImage/Save 节点(例如同时输出"原图放大"与"对比图")。

- **缺省**:取回**全部**有图片产出的节点;
- **指定**:传 `output_nodes` 参数,值为 Save 节点 id 的**逗号分隔**字符串,如 `"4,11"`;
- 全部产物都会下载到任务产物目录,**排第一个的节点产物作为主产物**返回给下游节点。

### 3.6 常见错误排查

错误经 `{"error": "<message>"}` 返回(HTTP 4xx/5xx),并记录中文日志。
映射总表(与 CONTRACT.md §4.2 一致):

| 现象 | HTTP | error 语义 |
|---|---|---|
| ComfyUI `/system_stats` 不通 | 503 | comfyui unreachable |
| inject JSON 非法 / 键不存在 | 400 | invalid inject mapping: \<detail\> + available nodes |
| workflow 名不存在 | 400 | workflow "\<name\>" not found; available: [...] |
| `POST /prompt` 被 ComfyUI 拒绝(400) | 502 | comfyui rejected prompt: \<node errors 摘要\> |
| 轮询超时(默认 1800s,可被引擎节点 timeout 先杀) | 504 | comfyui generation timeout after Ns |
| history 中执行错误(status_str=error / execution_error) | 502 | comfyui execution error: \<摘要\> |
| `/view` 下载产物失败 | 502 | failed to fetch output \<filename\> |
| 产物目录不可写 | 500 | output dir not writable: \<path\> |

**键不存在报错样例**(inject 引用了不存在于工作流中的节点 id `13`;报错全文为示意,以 adapter 实际返回为准):

```json
{"error": "invalid inject mapping: node \"13\" not found in workflow \"upscale_4x\"; available nodes: [\"1\", \"2\", \"3\", \"4\"]"}
```

排查动作:打开工作流 JSON,核对该节点 id 是否存在;确认键格式为三段
`<节点id>.inputs.<字段名>`(常见错误:漏掉中间的 `.inputs.`、把字段名写成界面显示名)。

**其他高频问题速查**:

| 症状 | 原因与处理 |
|---|---|
| 503 comfyui unreachable | ComfyUI 没启动 / 地址不对;先 `curl http://127.0.0.1:8188/system_stats` 验证 |
| 400 workflow "..." not found | `workflow` 参数与上传名不一致(不含 `.json`);报错会列出可用名 |
| 502 comfyui rejected prompt | 工作流内部引用的模型/文件在 ComfyUI 侧不存在,或节点参数非法——回 ComfyUI 界面验证同一工作流能否手动跑通 |
| 504 timeout | 生成太慢;放大引擎节点的 `timeout_secs`(见 §6.2) |

---

## 4. 示例模板走读

模块随包附带 3 个示例模板(`modules/comfyui-bridge/workflows/`),均只用 ComfyUI 内置节点。
**下文的节点 id 均与随包模板 JSON 一致**——若你在 ComfyUI 中自己搭建/重新导出工作流,
节点 id 几乎必然不同,inject 键必须以你实际上传的 JSON 为准(查看方法见 §2.4)。

### 4.1 upscale_4x — 图片放大

**节点图**(对应 `upscale_4x.api.json`,采用 ImageScale 几何放大,无外部模型依赖):

```
LoadImage(1) → ImageScale(3) → SaveImage(4)
```

**注入点**(共 1 处):节点 1 LoadImage 的 `image` 字段(接管线输入图)。
可选字面量注入:节点 3 的 `width` / `height`(覆盖模板默认 1024x1024)。

**配套管线**(`config/pipelines/comfyui_demo.toml`):

```toml
[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"       # 输入图片

[[nodes]]
id = "generate"
kind = "module"
module_id = "comfyui-bridge"
capability = "generate"
timeout_secs = 1800          # 放大耗时可能远超默认超时,见 §6.2
[nodes.params]
workflow = "upscale_4x"
inject = '{"1.inputs.image": "$input"}'

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"
```

若你只上传一张图、管线只有一个上游,直接写 `"$input"` 即可,无需定向引用。

### 4.2 style_transfer — 风格化 / 修复类(img2img)

**节点图**(对应 `style_transfer.api.json`,双图混合后 img2img 精修):

```
LoadImage(1, 内容图) ─┐
LoadImage(2, 参考图) ─┴→ ImageBlend(3) → VAEEncode(7) ─┐
CheckpointLoader(4) → CLIPTextEncode 正向(5)/负向(6) ──┼→ KSampler(8) → VAEDecode(9) → SaveImage(10)
```

**注入点**(共 4 处):两个 LoadImage 的 `image` 字段(节点 1/2)+ KSampler 的 `seed` 与 `steps` 字段(节点 8)。
可选字面量注入:节点 3 的 `blend_factor`(混合比例)、节点 5 的 `text`(正向提示词)。

**inject 示例**(编号与随包模板一致):

```json
{
  "1.inputs.image": "$input",
  "2.inputs.image": "$input.ref",
  "8.inputs.seed": 42,
  "8.inputs.steps": 28
}
```

**配套管线**:在 `comfyui_demo.toml` 基础上给管线**第二条输入边**(参考图,上游节点 id `ref`),
`inject` 改为上面的双图版本。多条输入边时 `$input.<上游节点id>` **必需**,
否则模块无法区分"主图"与"参考图"。另注意:模板默认引用 `v1-5-pruned-emaonly.safetensors`,
ComfyUI 侧需已装该基础模型(或字面量注入节点 4 的 `ckpt_name` 换成已装模型)。

### 4.3 txt2img — 文生图

**节点图**(对应 `txt2img.api.json`):

```
CheckpointLoaderSimple(1) ─┬→ CLIPTextEncode 正向(2) ─┐
                           ├→ CLIPTextEncode 负向(3) ─┤
                           └→ KSampler(5, model)      ├→ VAEDecode(6) → SaveImage(7)
EmptyLatentImage(4) → KSampler(5, latent_image) ──────┘
```

**注入点**(共 4 处):两个 CLIPTextEncode 的 `text` 字段(节点 2/3)+ KSampler 的 `seed` 与 `steps` 字段(节点 5)。
`inject` 完整示例与配套管线见 §3.4,此处不重复。

**要点**:

- 正向提示词用 `$input.<文本上游节点id>`(文本产物注入),负向提示词常用字符串常量;
- 工作流内部 `CheckpointLoaderSimple` 引用的模型必须已在 ComfyUI 侧就位(模板默认 SD1.5,
  SDXL 亦可——改节点 1 的 `ckpt_name` 为字面量注入或重新导出);
- 出图耗时对步数敏感,`timeout_secs` 建议 ≥ 1800。

---

## 5. 远程实例接入

ComfyUI 不必与本模块同机。`generate` 的目标地址解析顺序:

1. **参数 `base_url`**(管线节点 params 中直接指定)——优先级最高;
2. 环境变量 **`COMFYUI_URL`**(模块清单 `[compute.env]` 默认注入 `http://127.0.0.1:8188`)。

### 5.1 指向远程实例

管线节点参数示例:

```toml
[nodes.params]
base_url = "http://192.168.1.20:8188"
workflow = "upscale_4x"
inject   = '{"1.inputs.image": "$input"}'
```

### 5.2 让远程 ComfyUI 接受外部连接

ComfyUI 默认只监听回环地址(`127.0.0.1`)。远程部署需显式放开监听:

```bash
python main.py --listen 0.0.0.0 --port 8188
# 验证(从运行 EntryPoint 的机器):
curl -s http://192.168.1.20:8188/system_stats
```

### 5.3 安全提醒(务必阅读)

- **ComfyUI 自身没有任何认证机制**:任何能连上 8188 端口的人都可以提交任务、读取产物;
- 公网暴露前**必须**自行加鉴权(反向代理 + Basic Auth/Token 等)或仅做内网隔离,
  平台侧不会替你补这层防护;
- 本模块适配器对目标地址的代理策略:目标是回环地址(127.0.0.1/localhost)时绕过本机
  代理设置直连;远程地址则尊重环境代理配置——远程经代理可达时无需额外配置。

---

## 6. 限制与边界

### 6.1 取消是"尽力而为"

管线任务被取消(或被节点硬超时判死)时,平台断开 HTTP 连接;适配器会**尽力**调用 ComfyUI 的
`POST /interrupt` 中断队列中的任务。这是 best-effort:ComfyUI 是否真正中止、是否留下半成品文件,
平台**不保证**。被中断任务的工作目录产物由平台回收时清理。

### 6.2 长任务必须放宽节点 timeout

- 管线节点默认超时较短(约 300s)会把长任务杀掉;**执行 ComfyUI 的模块节点务必显式
  `timeout_secs = 1800`**(示例管线均已设置);
- 适配器自身的轮询超时默认同为 1800s,先到先杀;
- 执行期间引擎视该节点为活跃状态,不会被模块空闲回收误杀。

### 6.3 平台不保证工作流内部正确性(D6)

平台只承诺:向 ComfyUI 发送**合法数据包**(API 格式工作流 + 注入后的字段),
并在注入前校验**键存在性**。工作流**内部**是否连对线、引用的模型是否存在、参数是否自洽,
平台不校验、不修复——这类问题由 ComfyUI 拒绝提交(502)或执行报错(502)透传回来。
同样,上传即黑盒:首期不提供工作流可视化预览/编辑。

### 6.4 EP-PROGRESS 是粗粒度估算

轮询期间按"已完成输出节点数 / 预估总输出数"估算百分比并打印 `EP-PROGRESS:NN%`;
无输出阶段按队列状态给 5%~95% 的心跳值。**它不是精确进度**,偶尔停滞或跳变属预期。
精确到节点的进度需要引擎级方案(二期备选,见计划文档 §9),当前刻意不做。

### 6.5 其他

- 平台不拉起、不托管 ComfyUI 进程; EntryPoint 启动时 ComfyUI 可以不可达(`/health` 503);
- 模块 `module.toml` 不声明 `[[models]]`(桥接模块无权重文件),模型管理页不会出现模型下载项;
- 工作流上传文件名清洗规则与重名覆盖语义见 §2.3;
- 依赖:`fastapi` / `uvicorn` / `python-multipart` / `httpx`(与现役模块同款版本线,
  与 `config/constraints.txt` 无冲突)。
