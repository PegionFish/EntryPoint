#!/usr/bin/env bash
# EntryPoint 编译打包脚本（Linux）— server 模式（桌面端已于 2026-08-13 退役）
# 用法: ./build.sh server [-t debug|release] [-d <distro>] [--skip-test] [--skip-clippy] [--skip-frontend] [--clean] [-o <dir>]
#   server — 服务器包（daemon + WebUI + deploy.sh 交互式部署脚本）
#            Linux: ZIP 主产物（自包含，解压即用）+ tar.gz 兜底 + 按发行版族产 deb/rpm/PKGBUILD（探测到工具则产）
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_ROOT"

# 版本单一来源：Cargo.toml [workspace.package] version（勿在此处另写死版本号）；
# 与 daemon 界面版本（env!("CARGO_PKG_VERSION")）同源，保证包名/VERSION.txt/界面一致。
# 解析失败必须显式报错而非回退 0.0.0（否则静默产出错误命名的包）。
VERSION="$(sed -n 's/^version = "\([^"]*\)".*/\1/p' "$PROJECT_ROOT/Cargo.toml" | head -1)"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
    echo "  [FAIL] 无法从 Cargo.toml 解析版本号，拒绝打包（请检查 [workspace.package] version）" >&2
    exit 1
fi
MODE=""
TARGET="release"
SKIP_TEST=0
SKIP_CLIPPY=0
SKIP_FRONTEND=0
CLEAN=0
OUTPUT_DIR="dist"
DISTRO="auto"

# ── 帮助与日志 ────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
用法: $0 server [选项]
  server 服务器包（daemon + WebUI + deploy.sh，Linux: ZIP 主产物/tar.gz/deb/rpm/PKGBUILD）
选项:
  -t, --target debug|release   构建类型（默认 release）
  -d, --distro <name>          目标发行版（默认 auto：自动检测当前发行版）
                               已适配 glibc 约束/依赖包名/包格式；未知发行版仅 ZIP + tar.gz
      --skip-test              跳过 cargo test
      --skip-clippy            跳过 cargo clippy
      --skip-frontend          跳过 WebUI 前端构建（使用现有 crates/ep-webui/static 产物）
      --clean                  cargo clean 后重建
  -o, --output-dir <dir>       输出目录（默认 dist）
  -h, --help                   显示帮助
示例:
  ./build.sh server --distro <name>   # 指定目标发行版打包服务器
EOF
    exit 0
}

die() { echo "  [FAIL] $*" >&2; exit 1; }
step() { echo; echo "=== $* ==="; }
ok()   { echo "  [OK] $*"; }
info() { echo "  $*"; }

# ── 参数解析 ──────────────────────────────────────────────────────────────────
[[ $# -lt 1 ]] && usage
MODE="$1"; shift
case "$MODE" in
    server) ;;
    gui)
        echo "  [FAIL] gui 模式已随桌面端退役（2026-08-13）。" >&2
        echo "  WebUI 为唯一 UI，请改用: ./build.sh server" >&2
        echo "  历史说明见 docs/DESKTOP_SUNSET_PLAN.md。" >&2
        exit 1 ;;
    -h|--help) usage ;;
    *) die "未知模式: $MODE（仅支持 server）" ;;
esac

while [[ $# -gt 0 ]]; do
    case "$1" in
        -t|--target) TARGET="$2"; shift 2 ;;
        -d|--distro) DISTRO="$2"; shift 2 ;;
        --skip-test) SKIP_TEST=1; shift ;;
        --skip-clippy) SKIP_CLIPPY=1; shift ;;
        --skip-frontend) SKIP_FRONTEND=1; shift ;;
        --clean) CLEAN=1; shift ;;
        -o|--output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) die "未知选项: $1（用 -h 查看帮助）" ;;
    esac
done

case "$TARGET" in debug|release) ;; *) die "无效 target: $TARGET" ;; esac

# ── 目标发行版（默认自动检测当前发行版；显式指定时按发行版知识表适配）──
# 知识表：<distro> → family + 最低 glibc + 运行时依赖包名
# glibc 语义：Rust 二进制链接构建机 glibc，目标机 glibc 必须 ≥ 构建机。
#   故构建机 glibc ≤ 目标发行版最低 glibc 才安全；否则产出的二进制在目标
#   发行版可能无法启动（构建更高 glibc 发行版时需在 CI/容器中构建）。
DISTRO_GLIBC_MIN="
debian-11|2.31|ffmpeg, python3, python3-venv
debian-12|2.36|ffmpeg, python3, python3-venv
debian-13|2.41|ffmpeg, python3, python3-venv
ubuntu-20.04|2.31|ffmpeg, python3, python3-venv
ubuntu-22.04|2.35|ffmpeg, python3, python3-venv
ubuntu-23.04|2.37|ffmpeg, python3, python3-venv
ubuntu-23.10|2.38|ffmpeg, python3, python3-venv
ubuntu-24.04|2.39|ffmpeg, python3, python3-venv
ubuntu-24.10|2.39|ffmpeg, python3, python3-venv
linuxmint-21|2.35|ffmpeg, python3, python3-venv
linuxmint-22|2.39|ffmpeg, python3, python3-venv
mint-21|2.35|ffmpeg, python3, python3-venv
rhel-8|2.28|ffmpeg, python3
rhel-9|2.34|ffmpeg, python3
centos-7|2.17|ffmpeg, python3
centos-8|2.28|ffmpeg, python3
centos-9|2.34|ffmpeg, python3
fedora-38|2.37|ffmpeg, python3
fedora-39|2.38|ffmpeg, python3
fedora-40|2.38|ffmpeg, python3
fedora-41|2.39|ffmpeg, python3
rocky-8|2.28|ffmpeg, python3
rocky-9|2.34|ffmpeg, python3
almalinux-8|2.28|ffmpeg, python3
almalinux-9|2.34|ffmpeg, python3
oraclelinux-8|2.28|ffmpeg, python3
oraclelinux-9|2.34|ffmpeg, python3
arch|2.38|ffmpeg, python
manjaro|2.38|ffmpeg, python
endeavouros|2.38|ffmpeg, python
archlinux|2.38|ffmpeg, python
"

# 发行版知识查询：$1=<id>-<version> 或 <id>，输出 "family|glibc_min|deps"（未知 → "generic|"）
distro_profile_of() {
    local key="$1"
    local glibc=""
    local deps=""
    # 精确 <id>-<version> 匹配优先
    while IFS='|' read -r k g d; do
        [ -z "$k" ] && continue
        if [ "$k" = "$key" ]; then glibc="$g"; deps="$d"; break; fi
    done <<< "$DISTRO_GLIBC_MIN"
    # 无版本匹配 → 退化为 <id> 前缀（取同 ID 最后一行）
    if [ -z "$glibc" ]; then
        while IFS='|' read -r k g d; do
            [ -z "$k" ] && continue
            if [ "$k" = "$key" ] || [ "${k%%-*}" = "$key" ]; then glibc="$g"; deps="$d"; fi
        done <<< "$DISTRO_GLIBC_MIN"
    fi
    echo "$(distro_family_of "$key")|$glibc|$deps"
}

distro_family_of() {
    case "$1" in
        debian*|ubuntu*|linuxmint*|mint)      echo "deb" ;;
        rhel*|centos*|fedora*|rocky*|alma*|ol*)  echo "rpm" ;;
        arch*|manjaro*|endeavouros*)           echo "pkg" ;;
        *) echo "generic" ;;
    esac
}

detect_distro() {
    if [ -f /etc/os-release ]; then
        local ver=""
        . /etc/os-release 2>/dev/null || true
        # VERSION_ID 可能带次版本（如 Linux Mint 21.3）：按 . 截断为主版本再匹配知识表。
        # 注意：rolling 发行版（如 Arch）os-release 无 VERSION_ID，set -u 下必须
        # 默认展开为空，否则直接报 "unbound variable" 中断打包。
        ver="${VERSION_ID:-}"
        ver="${ver%%.*}"
        if [ -n "$ver" ]; then
            echo "${ID:-unknown}-${ver}"
        else
            echo "${ID:-unknown}"
        fi
    else
        echo "unknown"
    fi
}

# 语义化版本比较（a <= b?）：仅比较 X.Y.Z 前两段，patch 忽略
ver_le() { printf '%s\n%s\n' "$1" "$2" | sort -V -C; }

# 构建机 glibc vs 目标发行版最低 glibc 兼容性检查
check_glibc_compat() {
    local target_min="$1"
    [ -z "$target_min" ] && return 0
    local build_glibc
    build_glibc="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
    if [ -z "$build_glibc" ]; then
        info "提示: 无法检测构建机 glibc 版本，跳过 glibc 兼容性检查"
        return 0
    fi
    if ver_le "$build_glibc" "$target_min"; then
        ok "glibc 兼容: 构建机 $build_glibc ≤ 目标 $target_min（二进制可在目标发行版运行）"
    else
        info "警告: 构建机 glibc $build_glibc > 目标发行版最低 $target_min — 产出的二进制"
        info "       在 $DISTRO 上可能因 glibc 版本不足无法启动；建议在 glibc ≤ $target_min 的"
        info "       环境构建（如对应发行版的容器/CI）"
    fi
}

if [ "$DISTRO" = "auto" ]; then
    DISTRO="$(detect_distro)"
fi
DISTRO_PROFILE="$(distro_profile_of "$DISTRO")"
DISTRO_FAMILY="${DISTRO_PROFILE%%|*}"
DISTRO_GLIBC_MIN="${DISTRO_PROFILE#*|}"
DISTRO_DEPS="${DISTRO_GLIBC_MIN#*|}"
DISTRO_GLIBC_MIN="${DISTRO_GLIBC_MIN%%|*}"
step "目标发行版: $DISTRO (family: $DISTRO_FAMILY)"
if [ "$DISTRO_FAMILY" = "generic" ]; then
    info "提示: 发行版 $DISTRO 的包格式/依赖/glibc 约束暂未适配（可在 build.sh DISTRO_GLIBC_MIN 知识表补充），仅产出 ZIP + tar.gz 通用包"
else
    check_glibc_compat "$DISTRO_GLIBC_MIN"
fi

# ── 平台检测 ──────────────────────────────────────────────────────────────────
OS_ID="$(uname -s)"
case "$OS_ID" in
    Linux)  OS_ID="linux" ;;
    Darwin) OS_ID="macos" ;;
    *) die "不支持的操作系统: $OS_ID" ;;
esac
ARCH_ID="$(uname -m)"
case "$ARCH_ID" in
    x86_64)            ARCH_ID="x86_64" ;;
    aarch64|arm64)     ARCH_ID="aarch64" ;;
    *) die "不支持的架构: $ARCH_ID" ;;
esac
# 包格式架构映射（无交叉编译假设，按本机架构出包）：
# deb 用 amd64/arm64，rpm 用 x86_64/aarch64
DEB_ARCH="amd64"
RPM_ARCH="x86_64"
if [[ "$ARCH_ID" == "aarch64" ]]; then
    DEB_ARCH="arm64"
    RPM_ARCH="aarch64"
fi

if [[ "$OS_ID" == "macos" ]]; then
    die "macOS 不再支持（桌面端已于 2026-08-13 退役，仅 Linux server 模式）"
fi

CRATE="ep-daemon"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
PROFILE_DIR="$TARGET"
PACKAGE_BASE="EntryPoint-v${VERSION}-${OS_ID}-${ARCH_ID}-${MODE}"
DIST_DIR="$PROJECT_ROOT/$OUTPUT_DIR"
STAGING="$DIST_DIR/${PACKAGE_BASE}"
WORK_DIR="$PROJECT_ROOT/target/pkg-${MODE}"
MANIFEST="$WORK_DIR/manifest.txt"

# ── 工具检查 ──────────────────────────────────────────────────────────────────
step "环境检查"
# cargo 缺失时先尝试加载 rustup 环境（~/.cargo/env 存在才 source，避免
# 原 `|| { ...; }` 组在 env 缺失时返回非零、被 set -e 静默终止、die 提示不执行）
if ! command -v cargo >/dev/null 2>&1 && [ -f "$HOME/.cargo/env" ]; then
    # shellcheck disable=SC1090
    . "$HOME/.cargo/env"
fi
command -v cargo >/dev/null 2>&1 || die "cargo 未找到，请先安装 Rust: https://rustup.rs"
command -v rustc >/dev/null 2>&1 || die "rustc 未找到"
command -v git >/dev/null 2>&1 || die "git 未找到"
# deploy.sh 是 ZIP/tar.gz 包内唯一部署入口（Wave 2）：缺失则产物不可部署，
# 在编译之前 fail-fast，避免浪费整轮构建时间。
[[ -f "$PROJECT_ROOT/scripts/deploy.sh" ]] || die "scripts/deploy.sh 缺失——请先完成 deploy.sh 开发（包内不允许产出无部署脚本的 ZIP/tar.gz）"
ok "平台: $OS_ID/$ARCH_ID | 模式: $MODE | cargo: $(cargo --version)"

GIT_HASH="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
GIT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
RUSTC_VER="$(rustc --version)"

# ── WebUI 前端构建（fail-fast：前移至 cargo 编译之前）────────────────────────────
# npm 环境异常/前端构建失败不再浪费整轮 release 编译时间。
# 注意：static 为 git 跟踪文件且 vite 配置 emptyOutDir——构建会整体改写该目录。
WEBUI_STATIC="$PROJECT_ROOT/crates/ep-webui/static"
if [[ "$SKIP_FRONTEND" == "1" ]]; then
    info "跳过 WebUI 前端构建 (--skip-frontend)，使用现有 static 产物"
elif command -v npm >/dev/null 2>&1; then
    step "构建 WebUI 前端"
    ok "npm: $(command -v npm)"
    (cd "$PROJECT_ROOT/crates/ep-webui/frontend" && npm ci && npm run build) || die "WebUI 前端构建失败（npm ci / npm run build）——可加 --skip-frontend 使用现有 static 产物重试"
    ok "WebUI 前端构建完成"
    info "static 产物已更新（git 跟踪文件），如有变更请随仓库提交"
elif [[ -f "$WEBUI_STATIC/index.html" ]]; then
    info "警告: npm 不可用，使用现有 static 产物（可能陈旧）"
else
    die "npm 不可用且 crates/ep-webui/static 产物缺失——请安装 Node.js/npm，或先手动构建前端（--skip-frontend 仅适用于已有产物）"
fi

# ── Clean ─────────────────────────────────────────────────────────────────────
if [[ "$CLEAN" == "1" ]]; then
    step "清理构建产物"
    cargo clean --manifest-path "$PROJECT_ROOT/Cargo.toml"
    ok "cargo clean 完成"
fi

# ── Clippy ────────────────────────────────────────────────────────────────────
if [[ "$SKIP_CLIPPY" != "1" ]]; then
    step "Clippy 检查"
    # 单次运行（`if ! cmd; then` 中命令失败不触发 set -e），避免 clippy 编译两遍
    if ! clippy_out="$(cargo clippy --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace --all-targets 2>&1)"; then
        echo "$clippy_out" | grep -E "warning:|error" | head -30
        die "Clippy 失败"
    fi
    warn_count="$(echo "$clippy_out" | grep -c "warning:" || true)"
    if [[ "$warn_count" -gt 0 ]]; then info "Clippy 警告: $warn_count 个"; else ok "Clippy 零警告"; fi
else
    info "跳过 Clippy (--skip-clippy)"
fi

# ── 测试 ──────────────────────────────────────────────────────────────────────
if [[ "$SKIP_TEST" != "1" ]]; then
    step "运行测试"
    if ! cargo test --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace >/dev/null 2>&1; then
        cargo test --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace 2>&1 | grep -E "FAILED|failures:" | head -30
        die "测试失败"
    fi
    ok "所有测试通过"
else
    info "跳过测试 (--skip-test)"
fi

# ── 编译 ──────────────────────────────────────────────────────────────────────
# 仲裁 #36：ep-pack-cli（bin 名 ep-pack）随主 crate 一并构建并纳入打包
step "编译 ($TARGET) — $CRATE + ep-pack-cli"
BUILD_ARGS=(build --manifest-path "$PROJECT_ROOT/Cargo.toml" -p "$CRATE" -p ep-pack-cli)
[[ "$TARGET" == "release" ]] && BUILD_ARGS+=(--release)
if ! cargo "${BUILD_ARGS[@]}" >/dev/null 2>&1; then
    cargo "${BUILD_ARGS[@]}" 2>&1 | grep -E "^error" | head -30
    die "编译失败"
fi
ok "编译成功"

# ── 组装 staging 目录 ─────────────────────────────────────────────────────────
step "组装打包目录: $STAGING"
rm -rf "$STAGING" "$WORK_DIR"
mkdir -p "$STAGING/bin" "$STAGING/config/pipelines" "$STAGING/modules" "$STAGING/workspace"
mkdir -p "$WORK_DIR"

# 支持 CARGO_TARGET_DIR 环境变量覆盖构建目录（worktree/CI 场景；cargo 自身亦读同名变量）
BIN_SRC="${CARGO_TARGET_DIR:-$PROJECT_ROOT/target}/$PROFILE_DIR"
EXE_NAME="ep-daemon"
[[ -f "$BIN_SRC/$EXE_NAME" ]] || die "二进制不存在: $BIN_SRC/$EXE_NAME"

# 包内二进制名与角色一致：ep-daemon
# （与 start-daemon.sh、systemd ExecStart=/opt/entrypoint/bin/ep-daemon 对齐）
cp "$BIN_SRC/$EXE_NAME" "$STAGING/bin/$EXE_NAME"
chmod +x "$STAGING/bin/$EXE_NAME"
RES="$STAGING"
ok "二进制已就位: bin/$EXE_NAME"

# ep-pack CLI（仲裁 #36：server 包附带，bin 名 ep-pack）
[[ -f "$BIN_SRC/ep-pack" ]] || die "ep-pack 二进制不存在: $BIN_SRC/ep-pack"
mkdir -p "$RES/bin"
cp "$BIN_SRC/ep-pack" "$RES/bin/ep-pack"
chmod +x "$RES/bin/ep-pack"
ok "ep-pack CLI 已就位: bin/ep-pack"

# 配置
cp -a "$PROJECT_ROOT/config/." "$RES/config/"
ok "config/ 已复制"

# 模块（跳过 __pycache__）
for m in "$PROJECT_ROOT"/modules/*/; do
    name="$(basename "$m")"
    cp -a "$m" "$RES/modules/$name"
    rm -rf "$RES/modules/$name/__pycache__"
done
ok "modules/ 已复制"

# 共享 CUDA 库目录（§3.1）：可选资产，存在才随包附带（缺失不报错）。
# runtime/ 不入 git（.gitignore），由部署者自备 libcublas 等库文件；
# LD_LIBRARY_PATH 前置注入由 start.sh / systemd Environment / daemon 代码完成。
if [[ -d "$PROJECT_ROOT/runtime/cuda-libs" ]]; then
    mkdir -p "$RES/runtime"
    cp -a "$PROJECT_ROOT/runtime/cuda-libs" "$RES/runtime/cuda-libs"
    ok "runtime/cuda-libs 已随包附带（可选目录）"
else
    info "runtime/cuda-libs 不存在，跳过（可选目录）"
fi

# WebUI 前端 → webui/：构建动作已前移至 cargo 编译之前（fail-fast），
# 此处 static 必然已就绪，仅做校验后复制。

# 服务器包附加内容
if [[ "$MODE" == "server" ]]; then
    [[ -f "$WEBUI_STATIC/index.html" ]] || die "crates/ep-webui/static/index.html 不存在（前端构建不完整，请检查 npm run build 输出）"
    mkdir -p "$RES/webui"
    cp -a "$WEBUI_STATIC/." "$RES/webui/"
    ok "webui/ 已复制"
    cat > "$RES/entrypoint.service" <<EOF
[Unit]
Description=EntryPoint AI Module Orchestrator
After=network.target

[Service]
Type=simple
WorkingDirectory=/opt/entrypoint
ExecStart=/opt/entrypoint/bin/ep-daemon
Restart=on-failure
RestartSec=5
Environment=RUST_LOG=info
Environment=EP_ROOT=/opt/entrypoint
# 共享 CUDA 库目录（§3.1）：可选目录，缺失无副作用；
# 模块子进程的 LD_LIBRARY_PATH 前置注入由 daemon 代码负责（ep-core process.rs）
Environment=LD_LIBRARY_PATH=/opt/entrypoint/runtime/cuda-libs

[Install]
WantedBy=multi-user.target
EOF
    # deploy.sh（Wave 2 交互式部署脚本，源码见 scripts/deploy.sh）置于包根，0755。
    # 取代旧的内嵌 install.sh heredoc；缺失已在环境检查阶段 fail-fast，此处必然存在。
    # entrypoint.service 仍随包保留，供高级用户手装 systemd（deb/rpm 经 stage_fhs 复用）。
    install -m 0755 "$PROJECT_ROOT/scripts/deploy.sh" "$RES/deploy.sh"
    ok "服务器 systemd 单元 + deploy.sh 已入包"
fi

# 启动脚本
cat > "$RES/start-daemon.sh" <<'EOF'
#!/usr/bin/env bash
# 前台启动 daemon（生产环境建议运行包根 deploy.sh 交互式部署）
cd "$(dirname "$0")"
exec bin/ep-daemon
EOF
chmod +x "$RES/start-daemon.sh"
ok "启动脚本已生成"

# 文档 + 版本信息
[[ -f "$PROJECT_ROOT/README.md" ]] && cp "$PROJECT_ROOT/README.md" "$RES/README.md"
cat > "$RES/VERSION.txt" <<EOF
EntryPoint $VERSION ($MODE, $OS_ID/$ARCH_ID)
构建时间: $TIMESTAMP
Git 分支: $GIT_BRANCH
Git Commit: $GIT_HASH
构建类型: $TARGET
Rust 版本: $RUSTC_VER
EOF
ok "VERSION.txt 已生成"

# ── 打包 ─────────────────────────────────────────────────────────────────────
step "打包 ($MODE)"
rm -rf "$MANIFEST"

# ZIP 主产物（Wave 2）：自包含包，解压到任意目录后运行包根 deploy.sh 部署，
# 不绑定 /opt 与发行版布局。工具链降级顺序：zip → python3 zipfile → bsdtar → 跳过。
pkg_zip() {
    local name="${PACKAGE_BASE}.zip"
    local stage_base
    stage_base="$(basename "$STAGING")"
    if command -v zip >/dev/null 2>&1; then
        # 首选 zip：-r 递归、-y 保留符号链接；unix 可执行位写入 external_attr，解压即还原
        (cd "$DIST_DIR" && zip -rqy "$name" "$stage_base") || die "zip 打包失败: $name"
    elif command -v python3 >/dev/null 2>&1; then
        info "zip 缺失，降级 python3 -m zipfile（写入不保留 unix 权限位，随后按 staging 磁盘权限修复）"
        (cd "$DIST_DIR" && python3 -m zipfile -c "$name" "$stage_base") || die "python3 zipfile 打包失败: $name"
        # 修复：以 staging 磁盘权限重建归档，逐条目补 external_attr（可执行位等）
        python3 - "$DIST_DIR/$name" "$DIST_DIR" <<'PY' || die "zip 权限位修复失败（python3 降级路径）"
import os, stat, sys, zipfile
arc, base = sys.argv[1], sys.argv[2]
tmp = arc + '.fixtmp'
with zipfile.ZipFile(arc) as zin, zipfile.ZipFile(tmp, 'w', zipfile.ZIP_DEFLATED) as zout:
    for item in zin.infolist():
        src = os.path.join(base, item.filename)
        if item.is_dir():
            mode = 0o755
        elif os.path.exists(src):
            mode = stat.S_IMODE(os.stat(src).st_mode)
        else:
            mode = 0o644
        item.external_attr = ((mode & 0xFFFF) << 16) | (0x10 if item.is_dir() else 0)
        item.create_system = 3  # 标记 unix，确保 unzip 还原权限位
        zout.writestr(item, zin.read(item.filename), compress_type=zipfile.ZIP_DEFLATED)
os.replace(tmp, arc)
PY
    elif command -v bsdtar >/dev/null 2>&1; then
        info "zip/python3 缺失，降级 bsdtar --format zip（libarchive 保留 unix 权限位）"
        bsdtar --format zip -cf "$DIST_DIR/$name" -C "$DIST_DIR" "$stage_base" || die "bsdtar zip 打包失败: $name"
    else
        echo "  [WARN] zip / python3 / bsdtar 全部缺失：跳过 ZIP 主产物，本次仅产出 tar.gz 兜底包！" >&2
        echo "  [WARN] 安装 zip（如 pacman -S zip）后重新打包，方可获得自包含 ZIP 主产物。" >&2
        return 0
    fi
    echo "$name" >> "$MANIFEST"
    ok "zip: $DIST_DIR/$name ($(du -h "$DIST_DIR/$name" | cut -f1))"
}

pkg_tgz() {
    local name="${PACKAGE_BASE}.tar.gz"
    tar -C "$DIST_DIR" -czf "$DIST_DIR/$name" "$(basename "$STAGING")"
    echo "$name" >> "$MANIFEST"
    ok "tar.gz: $DIST_DIR/$name ($(du -h "$DIST_DIR/$name" | cut -f1))"
}

# 统一文件树 → /opt/entrypoint + /usr/bin 包装器（供 deb/rpm 使用）
stage_fhs() {
    local root="$1"
    mkdir -p "$root/opt/entrypoint" "$root/usr/bin" "$root/usr/lib/systemd/system"
    cp -a "$STAGING/." "$root/opt/entrypoint/"
    # ep-pack CLI 包装器（仲裁 #36：server 附带）
    cat > "$root/usr/bin/ep-pack" <<'EOF'
#!/bin/sh
export EP_ROOT=/opt/entrypoint
exec /opt/entrypoint/bin/ep-pack "$@"
EOF
    chmod +x "$root/usr/bin/ep-pack"
    cat > "$root/usr/bin/ep-daemon" <<'EOF'
#!/bin/sh
export EP_ROOT=/opt/entrypoint
exec /opt/entrypoint/bin/ep-daemon "$@"
EOF
    chmod +x "$root/usr/bin/ep-daemon"
    cp "$STAGING/entrypoint.service" "$root/usr/lib/systemd/system/entrypoint.service"
}

pkg_deb() {
    command -v dpkg-deb >/dev/null 2>&1 || { info "跳过 deb（未找到 dpkg-deb）"; return 0; }
    info "生成 deb ..."
    local root="$WORK_DIR/deb-root"
    stage_fhs "$root"
    mkdir -p "$root/DEBIAN"
    local pkgname="entrypoint-${MODE}"
    local desc="EntryPoint AI 模块编排平台（服务器）"
    cat > "$root/DEBIAN/control" <<EOF
Package: $pkgname
Version: $VERSION
Section: utils
Priority: optional
Architecture: $DEB_ARCH
Depends: ${DISTRO_DEPS:-ffmpeg, python3, python3-venv}
Maintainer: EntryPoint <https://github.com/PegionFish/EntryPoint>
Description: $desc
EOF
    cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
systemctl daemon-reload 2>/dev/null || true
systemctl enable entrypoint 2>/dev/null || true
EOF
    chmod +x "$root/DEBIAN/postinst"
    dpkg-deb --build --root-owner-group "$root" "$DIST_DIR/${pkgname}_${VERSION}-1_${DEB_ARCH}.deb" >/dev/null
    echo "${pkgname}_${VERSION}-1_${DEB_ARCH}.deb" >> "$MANIFEST"
    ok "deb: $DIST_DIR/${pkgname}_${VERSION}-1_${DEB_ARCH}.deb"
}

pkg_rpm() {
    command -v rpmbuild >/dev/null 2>&1 || { info "跳过 rpm（未找到 rpmbuild）"; return 0; }
    info "生成 rpm ..."
    local top="$WORK_DIR/rpmbuild"
    local br="$top/BUILDROOT/entrypoint-${MODE}-${VERSION}-1.${RPM_ARCH}"
    mkdir -p "$top/BUILD" "$top/RPMS" "$top/SOURCES" "$top/SPECS"
    stage_fhs "$br"
    local desc="EntryPoint AI 模块编排平台（服务器）"
    cat > "$top/SPECS/entrypoint-${MODE}.spec" <<EOF
Name: entrypoint-${MODE}
Version: ${VERSION}
Release: 1
Summary: $desc
License: MIT
BuildArch: ${RPM_ARCH}
Requires: $(echo "${DISTRO_DEPS:-ffmpeg, python3}" | tr ',' ' ')

%description
$desc

%prep
%build
%install

%files
/opt/entrypoint
/usr/bin/ep-pack
/usr/bin/ep-daemon
/usr/lib/systemd/system/entrypoint.service

%post
systemctl daemon-reload 2>/dev/null || true
systemctl enable entrypoint 2>/dev/null || true
EOF
    rpmbuild -bb --define "_topdir $top" "$top/SPECS/entrypoint-${MODE}.spec" >/dev/null
    cp "$top"/RPMS/${RPM_ARCH}/entrypoint-${MODE}-${VERSION}-1.${RPM_ARCH}.rpm "$DIST_DIR/"
    echo "entrypoint-${MODE}-${VERSION}-1.${RPM_ARCH}.rpm" >> "$MANIFEST"
    ok "rpm: $DIST_DIR/entrypoint-${MODE}-${VERSION}-1.${RPM_ARCH}.rpm"
}

pkg_arch() {
    local src="$PROJECT_ROOT/packaging/PKGBUILD"
    if [[ ! -f "$src" ]]; then info "跳过 Arch（未找到 $src）"; return 0; fi
    local dir="$DIST_DIR/arch-$MODE"
    mkdir -p "$dir"
    # PKGBUILD 声明 source=("$pkgname-$pkgver.tar.gz")：必须随附该源包 makepkg 才能构建。
    # 此前只拷 PKGBUILD 不产源 tar，makepkg 会因缺源包直接失败（修复 P1-4）。
    local pkgname pkgver stage src_tar sha
    pkgname="$(awk -F= '/^pkgname=/{print $2; exit}' "$src")"
    pkgver="$(awk -F= '/^pkgver=/{print $2; exit}' "$src")"
    stage="$dir/$pkgname-$pkgver"
    mkdir -p "$stage"
    # makepkg 构建所需文件（daemon + ep-pack-cli + WebUI 前端 + package() 引用资源）；
    # 含 Cargo.lock 以支持 PKGBUILD 的 cargo build --locked
    for f in Cargo.toml Cargo.lock crates modules config packaging LICENSE README.md; do
        [[ -e "$PROJECT_ROOT/$f" ]] && cp -a "$PROJECT_ROOT/$f" "$stage/"
    done
    # 剔除不应进入源包的构建残留/本地产物
    find "$stage" -name target -type d -prune -exec rm -rf {} + 2>/dev/null || true
    find "$stage" -name __pycache__ -type d -prune -exec rm -rf {} + 2>/dev/null || true
    [[ -d "$stage/modules/test-ffmpeg" ]] && rm -rf "$stage/modules/test-ffmpeg"
    src_tar="$dir/$pkgname-$pkgver.tar.gz"
    tar -C "$dir" -czf "$src_tar" "$pkgname-$pkgver"
    rm -rf "$stage"
    sha="$(sha256sum "$src_tar" | cut -d' ' -f1)"
    # 回填真实 sha256sums（与刚产出的源包一一对应；原 'SKIP' 属弱校验，此处做实）
    sed "s/sha256sums=('SKIP')/sha256sums=('$sha')/" "$src" > "$dir/PKGBUILD"
    # PKGBUILD 声明 install=entrypoint.install，需随附才能 makepkg
    if [[ -f "$PROJECT_ROOT/packaging/entrypoint.install" ]]; then
        cp "$PROJECT_ROOT/packaging/entrypoint.install" "$dir/"
    fi
    echo "arch-$MODE/PKGBUILD" >> "$MANIFEST"
    echo "arch-$MODE/$pkgname-$pkgver.tar.gz" >> "$MANIFEST"
    ok "Arch: $dir/PKGBUILD (sha256=$sha) + 源包 $pkgname-$pkgver.tar.gz"
}

pkg_zip   # 主产物置顶（自包含 ZIP：解压到任意目录后运行 deploy.sh 部署）
pkg_tgz   # 兜底
# 按目标发行版产出对应包格式（工具缺失则跳过，ZIP/tar.gz 保证有产物）
case "$DISTRO_FAMILY" in
    deb) pkg_deb ;;
    rpm) pkg_rpm ;;
    pkg) pkg_arch ;;
    *)   info "仅产出 ZIP + tar.gz 通用包（不产发行版专属包）" ;;
esac

# ── 清单 ─────────────────────────────────────────────────────────────────────
step "打包清单"
cat "$MANIFEST" | while read -r f; do info "  $f"; done

step "完成"
info "产物目录: $DIST_DIR"
