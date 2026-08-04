//! `ep-pack new <dir>` — 脚手架一个整合包作者目录：
//!
//! ```text
//! <dir>/
//! ├── ep-pack.toml          # 清单模板（§4.2 示例注释）
//! ├── README.md             # 作者指南速览
//! ├── pipelines/
//! │   └── example.toml      # 最小可执行样例管线（§6.2 形状，通过 validate）
//! └── models/               # bundle 权重占位目录（随包携带时按 target_dir 布局）
//! ```
//!
//! 目标目录已存在且非空 → 用法错误（退出码 2）。

use std::path::Path;
use std::process::ExitCode;

use crate::args;
use crate::output::{self, EXIT_FAILURE, EXIT_OK, EXIT_USAGE};

const USAGE: &str = "usage: ep-pack new <dir> [--json]";

/// 清单模板 — §4.2 示例注释；默认内容本身通过 `ep-pack validate`。
const MANIFEST_TEMPLATE: &str = r#"# EntryPoint 模型整合包清单（ep-pack.toml）
# 格式契约：docs/PACK_UNIFY_PLAN.md §4.2；全限定模型 ID：§4.3
# 下面为模板缺省值，请按实际整合包替换。

[pack]
# 全局唯一键：<publisher>.<pack-name>，各段 ^[a-z0-9][a-z0-9-]*$
id = "your-name.my-pack"
# semver 版本号（正式版本比较）
version = "0.1.0"
name = "我的整合包"
description = "一句话描述整合包内容（模型 + 管线 + 运行约束）"
authors = ["your-name"]
license = "MIT"
# homepage = "https://github.com/your-name/my-pack"
# 最低兼容 EntryPoint 版本（semver）；注释掉 = 不设下限
# min_ep_version = "0.1.0"
tags = []

[compute]
# 包声明可利用的后端（导入时与本机设备比对，§4.6）
backends = ["cpu"]
# 每后端运行备注（自由文本，展示用），例如：
# notes = { rocm = "需 torch-rocm wheel" }

# ── 模型条目（可选：纯管线包可整段删除）────────────────────────────
# qualified_id 为全限定 ID `<publisher>.<vendor>.<model>`（§4.3）；
# 保留发布者 `ep` = 仓库内置模块。
# mode = "reference"  仅描述符，导入时按模块 manifest 声明的下载源获取权重
# mode = "bundle"     权重随包携带：置于本目录 models/<模块声明的 target_dir>/ 下
#
# [[models]]
# qualified_id = "ep.systran.faster-whisper"
# variant = "large-v3"
# mode = "reference"
# tags = ["字幕"]

[[pipelines]]
# 包内相对路径（/ 分隔，禁止 ..）
file = "pipelines/example.toml"
"#;

/// 样例管线 — §6.2 最小合法形状（含 file_input，通过 DAG 校验）。
const EXAMPLE_PIPELINE: &str = r#"# 样例管线 — 替换为你的实际管线，或删除本文件并同步修改 ep-pack.toml
# 节点 schema：docs/PACK_UNIFY_PLAN.md §6.2

[pipeline]
id = "example"
name = "示例管线"
description = "整合包脚手架自带的最小示例"

[[nodes]]
id = "input"
kind = "builtin"
builtin = "file_input"

[[nodes]]
id = "output"
kind = "builtin"
builtin = "file_output"

[[edges]]
from = ["input", "out"]
to = ["output", "in"]
"#;

/// 脚手架 README（作者指南速览；完整版由 C8 文档代理产出）。
const README_TEMPLATE: &str = r#"# 整合包作者目录

本目录由 `ep-pack new` 生成，是一个 EntryPoint 模型整合包（.epzip）的源目录。

## 布局

- `ep-pack.toml` — 包清单（契约见 docs/PACK_UNIFY_PLAN.md §4.2）
- `pipelines/` — 管线定义（§6.2 TOML），经清单 `[[pipelines]]` 引用
- `models/` — bundle 模式权重：置于 `models/<模块声明的 target_dir>/`

## 工作流

```text
ep-pack validate .            # 清单/schema/管线/CHECKSUMS 全项校验
ep-pack build . -o out.epzip  # 打包（自动生成 CHECKSUMS.toml，确定性字节）
ep-pack info out.epzip        # 查看归档摘要与 CHECKSUMS 状态
ep-pack import out.epzip --root <EP 应用根>   # 导入验证（--dry-run 先出报告）
```

构建产物（.epzip）可自行上传 GitHub Release 等平台分发。
"#;

pub fn run(argv: &[String]) -> ExitCode {
    let parsed = match args::parse(argv, &[]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("{USAGE}");
            return output::exit(EXIT_USAGE);
        }
    };
    let json = parsed.switch("json");
    if parsed.switch("help") {
        println!("{USAGE}");
        return output::exit(EXIT_OK);
    }
    let positional = match parsed.positional_exact(1, USAGE) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return output::exit(EXIT_USAGE);
        }
    };
    let dir = Path::new(&positional[0]);

    // 已存在且非空 → 拒绝（防覆盖作者既有内容）
    if dir.is_dir() {
        let non_empty = std::fs::read_dir(dir).map(|mut d| d.next().is_some());
        if non_empty.unwrap_or(true) {
            return output::fail(
                EXIT_USAGE,
                json,
                vec![format!(
                    "target dir already exists and is not empty: {}",
                    dir.display()
                )],
            );
        }
    } else if dir.exists() {
        return output::fail(
            EXIT_USAGE,
            json,
            vec![format!("target path exists and is not a directory: {}", dir.display())],
        );
    }

    let files: [(&str, &str); 3] = [
        ("ep-pack.toml", MANIFEST_TEMPLATE),
        ("README.md", README_TEMPLATE),
        ("pipelines/example.toml", EXAMPLE_PIPELINE),
    ];

    let mut written: Vec<String> = Vec::new();
    for (rel, content) in &files {
        let path = rel.split('/').fold(dir.to_path_buf(), |acc, seg| acc.join(seg));
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return output::fail(
                    EXIT_FAILURE,
                    json,
                    vec![format!("failed to create {}: {e}", parent.display())],
                );
            }
        }
        if let Err(e) = std::fs::write(&path, content) {
            return output::fail(
                EXIT_FAILURE,
                json,
                vec![format!("failed to write {}: {e}", path.display())],
            );
        }
        written.push(path.display().to_string());
    }
    // models/ 占位目录（bundle 权重落点）
    let models_dir = dir.join("models");
    if let Err(e) = std::fs::create_dir_all(&models_dir) {
        return output::fail(
            EXIT_FAILURE,
            json,
            vec![format!("failed to create {}: {e}", models_dir.display())],
        );
    }

    if json {
        output::print_json(&serde_json::json!({
            "ok": true,
            "dir": dir.display().to_string(),
            "created": written,
            "next": "ep-pack validate <dir>",
        }));
    } else {
        println!("created pack scaffold at {}", dir.display());
        for w in &written {
            println!("  + {w}");
        }
        println!("  + {}/", models_dir.display());
        println!("next: ep-pack validate {}", dir.display());
    }
    output::exit(EXIT_OK)
}
