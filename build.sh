#!/usr/bin/env bash
# EntryPoint 编译打包脚本（Linux）— server 模式（桌面端已于 2026-08-13 退役）
# 用法: ./build.sh server [-t debug|release] [-d <distro>] [--skip-test] [--skip-clippy] [--clean] [-o <dir>]
#   server — 服务器包（daemon + WebUI + systemd 安装脚本）
#            Linux: tar.gz 兑底 + deb/rpm/PKGBUILD（探测到工具则产）
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
CLEAN=0
OUTPUT_DIR="dist"
DISTRO="auto"

# ── 帮助与日志 ────────────────────────────────────────────────────────────────
usage() {
    cat <<EOF
用法: $0 server [选项]
  server 服务器包（daemon + WebUI + systemd，Linux: tar.gz/deb/rpm/PKGBUILD）
选项:
  -t, --target debug|release   构建类型（默认 release）
  -d, --distro <name>          目标发行版（默认 auto：自动检测当前发行版）
                               已适配 glibc 约束/依赖包名/包格式；未知发行版仅 tar.gz
      --skip-test              跳过 cargo test
      --skip-clippy            跳过 cargo clippy
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
        # VERSION_ID 可能带次版本（如 Linux Mint 21.3）：按 . 截断为主版本再匹配知识表
        ver="${VERSION_ID%%.*}"
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
    info "提示: 发行版 $DISTRO 的包格式/依赖/glibc 约束暂未适配（可在 build.sh DISTRO_GLIBC_MIN 知识表补充），仅产出 tar.gz 兑底包"
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

BIN_SRC="$PROJECT_ROOT/target/$PROFILE_DIR"
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
cat > "$RES/start-daemon.sh" <<'EOF'
#!/usr/bin/env bash
# 前台启动 daemon（生产环境建议用 install.sh 注册 systemd 服务）
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

pkg_tgz
# 按目标发行版产出对应包格式（工具缺失则跳过，tar.gz 兜底保证有产物）
case "$DISTRO_FAMILY" in
    deb) pkg_deb ;;
    rpm) pkg_rpm ;;
    pkg) pkg_arch ;;
    *)   info "仅产出 tar.gz 兑底包" ;;
esac

# ── 清单 ─────────────────────────────────────────────────────────────────────
step "打包清单"
cat "$MANIFEST" | while read -r f; do info "  $f"; done

step "完成"
info "产物目录: $DIST_DIR"
