#!/usr/bin/env bash
# EntryPoint 编译打包脚本（Linux / macOS）
# 用法: ./build.sh <gui|server> [-t debug|release] [-d <distro>] [--skip-test] [--skip-clippy] [--clean] [-o <dir>]
#   gui    — 桌面 GUI 客户端包
#            Linux: tar.gz 兑底 + deb/rpm/PKGBUILD（探测到工具则产）
#            macOS: EntryPoint.app 并压缩为 zip（仅支持 gui）
#   server — 服务器包（daemon + WebUI + systemd 安装脚本）
#            Linux: tar.gz 兑底 + deb/rpm/PKGBUILD（探测到工具则产）
set -euo pipefail

PROJECT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$PROJECT_ROOT"

VERSION="0.1.0"
MODE=""
TARGET="release"
SKIP_TEST=0
SKIP_CLIPPY=0
CLEAN=0
OUTPUT_DIR="dist"
DISTRO="auto"

# ── 帮助与日志 ────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
用法: $0 <gui|server> [选项]
  gui    桌面 GUI 客户端包（Linux: tar.gz/deb/rpm/PKGBUILD; macOS: .app+zip）
  server 服务器包（daemon + WebUI + systemd，Linux: tar.gz/deb/rpm/PKGBUILD）
选项:
  -t, --target debug|release   构建类型（默认 release）
  -d, --distro <name>          目标发行版（默认 auto：自动检测当前发行版）
                               具体发行版适配详见项目 TODO
      --skip-test              跳过 cargo test
      --skip-clippy            跳过 cargo clippy
      --clean                  cargo clean 后重建
  -o, --output-dir <dir>       输出目录（默认 dist）
  -h, --help                   显示帮助
示例:
  ./build.sh gui                # 按当前发行版打包 GUI 客户端
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
    gui|server) ;;
    -h|--help) usage ;;
    *) die "未知模式: $MODE（可选 gui|server）" ;;
esac

while [[ $# -gt 0 ]]; do
    case "$1" in
        -t|--target) TARGET="$2"; shift 2 ;;
        -d|--distro) DISTRO="$2"; shift 2 ;;
        --skip-test) SKIP_TEST=1; shift ;;
        --skip-clippy) SKIP_CLIPPY=1; shift ;;
        --clean) CLEAN=1; shift ;;
        -o|--output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) die "未知选项: $1（用 -h 查看帮助）" ;;
    esac
done

case "$TARGET" in debug|release) ;; *) die "无效 target: $TARGET" ;; esac

# ── 目标发行版（默认自动检测当前发行版；显式指定时仅决定包格式，具体适配见 TODO）──
distro_family_of() {
    case "$1" in
        debian*|ubuntu*|mint)                     echo "deb" ;;
        rhel*|centos*|fedora*|rocky*|alma*|ol*)  echo "rpm" ;;
        arch*|manjaro*|endeavouros*)              echo "pkg" ;;
        *) echo "generic" ;;
    esac
}

detect_distro() {
    if [ -f /etc/os-release ]; then
        local id=""
        . /etc/os-release 2>/dev/null || true
        echo "${ID:-unknown}-${VERSION_ID:-}"
    else
        echo "unknown"
    fi
}

if [ "$DISTRO" = "auto" ]; then
    DISTRO="$(detect_distro)"
fi
DISTRO_FAMILY="$(distro_family_of "$DISTRO")"
step "目标发行版: $DISTRO (family: $DISTRO_FAMILY)"
if [ "$DISTRO_FAMILY" = "generic" ]; then
    info "提示: 发行版 $DISTRO 的包格式暂未适配（详见项目 TODO），将仅产出 tar.gz 兑底包"
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

if [[ "$OS_ID" == "macos" && "$MODE" == "server" ]]; then
    die "macOS 仅支持 GUI 客户端打包（./build.sh gui）"
fi

CRATE="ep-desktop"
[[ "$MODE" == "server" ]] && CRATE="ep-daemon"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
PROFILE_DIR="$TARGET"
PACKAGE_BASE="EntryPoint-v${VERSION}-${OS_ID}-${ARCH_ID}-${MODE}"
DIST_DIR="$PROJECT_ROOT/$OUTPUT_DIR"
STAGING="$DIST_DIR/${PACKAGE_BASE}"
WORK_DIR="$PROJECT_ROOT/target/pkg-${MODE}"
MANIFEST="$WORK_DIR/manifest.txt"

# ── 工具检查 ──────────────────────────────────────────────────────────────────
step "环境检查"
command -v cargo >/dev/null 2>&1 || { [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"; }
command -v cargo >/dev/null 2>&1 || die "cargo 未找到，请先安装 Rust: https://rustup.rs"
command -v rustc >/dev/null 2>&1 || die "rustc 未找到"
command -v git >/dev/null 2>&1 || die "git 未找到"
ok "平台: $OS_ID/$ARCH_ID | 模式: $MODE | cargo: $(cargo --version)"

GIT_HASH="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
GIT_BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
RUSTC_VER="$(rustc --version)"

# ── Clean ─────────────────────────────────────────────────────────────────────
if [[ "$CLEAN" == "1" ]]; then
    step "清理构建产物"
    cargo clean --manifest-path "$PROJECT_ROOT/Cargo.toml"
    ok "cargo clean 完成"
fi

# ── Clippy ────────────────────────────────────────────────────────────────────
if [[ "$SKIP_CLIPPY" != "1" ]]; then
    step "Clippy 检查"
    clippy_out="$(cargo clippy --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace --all-targets 2>&1 || true)"
    if ! cargo clippy --manifest-path "$PROJECT_ROOT/Cargo.toml" --workspace --all-targets >/dev/null 2>&1; then
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

BIN_SRC="$PROJECT_ROOT/target/$PROFILE_DIR"
EXE_NAME="entrypoint"
[[ "$MODE" == "server" ]] && EXE_NAME="ep-daemon"
[[ -f "$BIN_SRC/$EXE_NAME" ]] || die "二进制不存在: $BIN_SRC/$EXE_NAME"

if [[ "$OS_ID" == "macos" ]]; then
    # macOS: 二进制直接放入 .app 的 MacOS 目录（macOS 仅 gui 模式）
    APP_DIR="$STAGING/EntryPoint.app"
    mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
    cp "$BIN_SRC/$EXE_NAME" "$APP_DIR/Contents/MacOS/entrypoint"
    chmod +x "$APP_DIR/Contents/MacOS/entrypoint"
    RES="$APP_DIR/Contents/Resources"
else
    # 包内二进制名与角色一致：gui=entrypoint / server=ep-daemon
    # （与 start-*.sh、systemd ExecStart=/opt/entrypoint/bin/ep-daemon 对齐）
    cp "$BIN_SRC/$EXE_NAME" "$STAGING/bin/$EXE_NAME"
    chmod +x "$STAGING/bin/$EXE_NAME"
    RES="$STAGING"
fi
ok "二进制已就位: bin/$EXE_NAME"

# ep-pack CLI（仲裁 #36：gui/server 包均附带，bin 名 ep-pack）
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

# 服务器包附加内容
if [[ "$MODE" == "server" ]]; then
    mkdir -p "$RES/webui"
    cp -a "$PROJECT_ROOT/crates/ep-webui/static/." "$RES/webui/" 2>/dev/null || \
        info "警告: crates/ep-webui/static 不存在（请先构建 WebUI 前端）"
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
    cat > "$RES/install.sh" <<'EOF'
#!/usr/bin/env bash
# 安装 EntryPoint 服务器到 /opt/entrypoint 并注册 systemd 服务
# 用法: bash install.sh   （需 root 或 sudo）
set -euo pipefail
SRC="$(cd "$(dirname "$0")" && pwd)"
DEST="${DEST:-/opt/entrypoint}"
if [[ "$(id -u)" != "0" ]]; then exec sudo -E "$0" "$@"; fi
mkdir -p "$DEST"
cp -a "$SRC/bin" "$SRC/webui" "$SRC/config" "$SRC/modules" "$SRC/workspace" "$DEST/"
# runtime/（含可选 cuda-libs，§3.1）：存在才复制
if [[ -d "$SRC/runtime" ]]; then cp -a "$SRC/runtime" "$DEST/"; fi
install -m644 "$SRC/entrypoint.service" /etc/systemd/system/entrypoint.service
systemctl daemon-reload
systemctl enable entrypoint
systemctl start entrypoint || true
PORT="$(grep -E '^[[:space:]]*port[[:space:]]*=' "$DEST/config/app.toml" 2>/dev/null | grep -oE '[0-9]+' | head -1 || echo 9800)"
echo "==> EntryPoint 服务器已安装并启动: http://localhost:$PORT"
echo "    日志: journalctl -u entrypoint -f | 状态: systemctl status entrypoint"
EOF
    chmod +x "$RES/install.sh"
    ok "服务器 systemd 服务 + install.sh 已生成"
fi

# 启动脚本
if [[ "$MODE" == "gui" ]]; then
    cat > "$RES/start-desktop.sh" <<'EOF'
#!/usr/bin/env bash
cd "$(dirname "$0")"
exec bin/entrypoint
EOF
    chmod +x "$RES/start-desktop.sh"
    if [[ -f "$PROJECT_ROOT/packaging/entrypoint.desktop" ]]; then
        cp "$PROJECT_ROOT/packaging/entrypoint.desktop" "$RES/"
    fi
else
    cat > "$RES/start-daemon.sh" <<'EOF'
#!/usr/bin/env bash
# 前台启动 daemon（生产环境建议用 install.sh 注册 systemd 服务）
cd "$(dirname "$0")"
exec bin/ep-daemon
EOF
    chmod +x "$RES/start-daemon.sh"
fi
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

pkg_tgz() {
    local name="${PACKAGE_BASE}.tar.gz"
    tar -C "$DIST_DIR" -czf "$DIST_DIR/$name" "$(basename "$STAGING")"
    echo "$name" >> "$MANIFEST"
    ok "tar.gz: $DIST_DIR/$name ($(du -h "$DIST_DIR/$name" | cut -f1))"
}

pkg_macos_zip() {
    local name="${PACKAGE_BASE}.zip"
    if command -v ditto >/dev/null 2>&1; then
        (cd "$STAGING" && ditto -c -k --sequesterRsrc --keepParent EntryPoint.app "$DIST_DIR/$name")
    else
        (cd "$STAGING" && zip -r "$DIST_DIR/$name" EntryPoint.app)
    fi
    echo "$name" >> "$MANIFEST"
    ok "zip: $DIST_DIR/$name ($(du -h "$DIST_DIR/$name" | cut -f1))"
}

# 统一文件树 → /opt/entrypoint + /usr/bin 包装器（供 deb/rpm 使用）
stage_fhs() {
    local root="$1"
    mkdir -p "$root/opt/entrypoint" "$root/usr/bin" "$root/usr/lib/systemd/system"
    cp -a "$STAGING/." "$root/opt/entrypoint/"
    # ep-pack CLI 包装器（仲裁 #36：gui/server 均附带）
    cat > "$root/usr/bin/ep-pack" <<'EOF'
#!/bin/sh
export EP_ROOT=/opt/entrypoint
exec /opt/entrypoint/bin/ep-pack "$@"
EOF
    chmod +x "$root/usr/bin/ep-pack"
    if [[ "$MODE" == "server" ]]; then
        cat > "$root/usr/bin/ep-daemon" <<'EOF'
#!/bin/sh
export EP_ROOT=/opt/entrypoint
exec /opt/entrypoint/bin/ep-daemon "$@"
EOF
        chmod +x "$root/usr/bin/ep-daemon"
        cp "$STAGING/entrypoint.service" "$root/usr/lib/systemd/system/entrypoint.service"
    else
        cat > "$root/usr/bin/entrypoint" <<'EOF'
#!/bin/sh
export EP_ROOT=/opt/entrypoint
exec /opt/entrypoint/bin/entrypoint "$@"
EOF
        chmod +x "$root/usr/bin/entrypoint"
    fi
}

pkg_deb() {
    command -v dpkg-deb >/dev/null 2>&1 || { info "跳过 deb（未找到 dpkg-deb）"; return 0; }
    info "生成 deb ..."
    local root="$WORK_DIR/deb-root"
    stage_fhs "$root"
    mkdir -p "$root/DEBIAN"
    local pkgname="entrypoint-${MODE}"
    local desc="EntryPoint AI 模块编排平台"
    [[ "$MODE" == "server" ]] && desc="EntryPoint AI 模块编排平台（服务器）"
    cat > "$root/DEBIAN/control" <<EOF
Package: $pkgname
Version: $VERSION
Section: utils
Priority: optional
Architecture: amd64
Maintainer: EntryPoint <https://github.com/PegionFish/EntryPoint>
Description: $desc
EOF
    if [[ "$MODE" == "server" ]]; then
        cat > "$root/DEBIAN/postinst" <<'EOF'
#!/bin/sh
systemctl daemon-reload 2>/dev/null || true
systemctl enable entrypoint 2>/dev/null || true
EOF
        chmod +x "$root/DEBIAN/postinst"
    fi
    dpkg-deb --build --root-owner-group "$root" "$DIST_DIR/${pkgname}_${VERSION}-1_amd64.deb" >/dev/null
    echo "${pkgname}_${VERSION}-1_amd64.deb" >> "$MANIFEST"
    ok "deb: $DIST_DIR/${pkgname}_${VERSION}-1_amd64.deb"
}

pkg_rpm() {
    command -v rpmbuild >/dev/null 2>&1 || { info "跳过 rpm（未找到 rpmbuild）"; return 0; }
    info "生成 rpm ..."
    local top="$WORK_DIR/rpmbuild"
    local br="$top/BUILDROOT/entrypoint-${MODE}-${VERSION}-1.x86_64"
    mkdir -p "$top/BUILD" "$top/RPMS" "$top/SOURCES" "$top/SPECS"
    stage_fhs "$br"
    local desc="EntryPoint AI 模块编排平台"
    [[ "$MODE" == "server" ]] && desc="EntryPoint AI 模块编排平台（服务器）"
    cat > "$top/SPECS/entrypoint-${MODE}.spec" <<EOF
Name: entrypoint-${MODE}
Version: ${VERSION}
Release: 1
Summary: $desc
License: MIT
BuildArch: x86_64

%description
$desc

%prep
%build
%install

%files
/opt/entrypoint
/usr/bin/ep-pack
EOF
    if [[ "$MODE" == "server" ]]; then
        cat >> "$top/SPECS/entrypoint-${MODE}.spec" <<'EOF'
/usr/bin/ep-daemon
/usr/lib/systemd/system/entrypoint.service

%post
systemctl daemon-reload 2>/dev/null || true
systemctl enable entrypoint 2>/dev/null || true
EOF
    else
        printf '/usr/bin/entrypoint\n' >> "$top/SPECS/entrypoint-${MODE}.spec"
    fi
    rpmbuild -bb --define "_topdir $top" "$top/SPECS/entrypoint-${MODE}.spec" >/dev/null
    cp "$top"/RPMS/x86_64/entrypoint-${MODE}-${VERSION}-1.x86_64.rpm "$DIST_DIR/"
    echo "entrypoint-${MODE}-${VERSION}-1.x86_64.rpm" >> "$MANIFEST"
    ok "rpm: $DIST_DIR/entrypoint-${MODE}-${VERSION}-1.x86_64.rpm"
}

pkg_arch() {
    local src="$PROJECT_ROOT/packaging/PKGBUILD"
    [[ "$MODE" == "gui" ]] && src="$PROJECT_ROOT/packaging/PKGBUILD.gui"
    if [[ ! -f "$src" ]]; then info "跳过 Arch（未找到 $src）"; return 0; fi
    mkdir -p "$DIST_DIR/arch-$MODE"
    cp "$src" "$DIST_DIR/arch-$MODE/PKGBUILD"
    # server PKGBUILD 声明 install=entrypoint.install，需随附才能 makepkg
    if [[ "$MODE" == "server" && -f "$PROJECT_ROOT/packaging/entrypoint.install" ]]; then
        cp "$PROJECT_ROOT/packaging/entrypoint.install" "$DIST_DIR/arch-$MODE/"
    fi
    echo "arch-$MODE/PKGBUILD" >> "$MANIFEST"
    ok "Arch PKGBUILD: dist/arch-$MODE/PKGBUILD"
}

if [[ "$OS_ID" == "macos" ]]; then
    pkg_macos_zip
else
    pkg_tgz
    # 按目标发行版产出对应包格式（工具缺失则跳过，tar.gz 兜底保证有产物）
    case "$DISTRO_FAMILY" in
        deb) pkg_deb ;;
        rpm) pkg_rpm ;;
        pkg) pkg_arch ;;
        *)   info "仅产出 tar.gz 兑底包" ;;
    esac
fi

# ── 清单 ─────────────────────────────────────────────────────────────────────
step "打包清单"
cat "$MANIFEST" | while read -r f; do info "  $f"; done

step "完成"
info "产物目录: $DIST_DIR"
