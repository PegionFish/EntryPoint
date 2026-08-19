#!/usr/bin/env bash
# ============================================================================
# EntryPoint 部署脚本（解压目录自包含版 — Wave 2 用户裁决）
#
# 设计契约：
#   * 用户把发布 ZIP 解压到任意目录（如 /server/AnotherViewer），本脚本位于
#     解压目录根部，一切安装/配置/服务动作都发生在该目录内——不复制到 /opt、
#     不绑定任何发行版目录布局。
#   * systemd unit 由本脚本内嵌模板现场渲染（唯一权威来源），显式注入
#     Environment=EP_ROOT=<解压目录绝对路径>；daemon 依赖 EP_ROOT 定位根目录
#     （ep-core config.rs resolve_root）。
#   * unit 假设 SIGTERM 触发优雅回收（Wave 2 B3 修复），TimeoutStopSec=30 为
#     优雅回收窗口。
#   * 绝不执行 systemctl enable（开机自启由用户显式决定）。
#   * 与 B1 打包接口：发布包根部包含 bin/ep-daemon、config/app.toml、
#     modules/、webui/、VERSION.txt、start-daemon.sh 与本脚本（deploy.sh）。
#
# 用法: ./deploy.sh [子命令] [flags]
#   子命令: install(默认) uninstall status start stop logs configure check help
#   详见 ./deploy.sh help
# ============================================================================
set -euo pipefail

# ── 1. 路径常量与严格模式 ─────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_PATH="${SCRIPT_DIR}/$(basename "${BASH_SOURCE[0]}")"
# 自包含约定：deploy.sh 位于解压目录根部 → EP_ROOT 即脚本所在目录
EP_ROOT="$SCRIPT_DIR"
CONFIG_FILE="${EP_ROOT}/config/app.toml"
VERSION_FILE="${EP_ROOT}/VERSION.txt"
UNIT_NAME="entrypoint"
UNIT_PATH="/etc/systemd/system/${UNIT_NAME}.service"
HEALTH_URL_FMT="http://127.0.0.1:%s/api/health"   # %s=port
REAL_USER="${SUDO_USER:-$(id -un)}"
ORIG_ARGS=()
DISTRO_FAMILY=""

# ── 2. 彩色输出（仅 stdout 为 tty 时启用）─────────────────────────────────────
if [[ -t 1 ]]; then
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
    C_BLUE=$'\033[34m'; C_BOLD=$'\033[1m'; C_NC=$'\033[0m'
else
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_BOLD=""; C_NC=""
fi
info() { printf '%s\n' "$*"; }
ok()   { printf '%s[OK]%s %s\n' "$C_GREEN" "$C_NC" "$*"; }
warn() { printf '%s[警告]%s %s\n' "$C_YELLOW" "$C_NC" "$*" >&2; }
step() { printf '\n%s=== %s ===%s\n' "$C_BOLD$C_BLUE" "$*" "$C_NC"; }
die()  { printf '%s[失败]%s %s\n' "$C_RED" "$C_NC" "$*" >&2; exit 1; }

# ── 3. 全局 flags（默认值）────────────────────────────────────────────────────
FLAG_YES=0
FLAG_DISTRO=""
FLAG_SKIP_DEPS=0
FLAG_FFMPEG_SOURCE="fusion"
FLAG_HOST=""
FLAG_PORT=""
FLAG_ALLOW_PUBLIC=""
FLAG_API_TOKEN=""
FLAG_NO_TOKEN=0
FLAG_HTTP_PROXY_SET=0; FLAG_HTTP_PROXY=""
FLAG_HTTPS_PROXY_SET=0; FLAG_HTTPS_PROXY=""
FLAG_WITH_SERVICE=0
FLAG_NO_SERVICE=0
FLAG_USER=""
FLAG_NO_FIREWALL=0
FLAG_SKIP_SELINUX=0
FLAG_PURGE=0
LOGS_ARGS=()

# ── 4. 交互提问函数 ───────────────────────────────────────────────────────────
# ask_value <提示语> <缺省值> → stdout 输出答案
# --yes 或非交互（stdin 非 tty）时直接取缺省。
ask_value() {
    local prompt="$1" default="$2" ans=""
    if [[ "$FLAG_YES" == "1" ]] || [[ ! -t 0 ]]; then
        printf '%s\n' "$default"
        return 0
    fi
    read -r -p "$(printf '%s?%s %s [%s]: ' "$C_YELLOW" "$C_NC" "$prompt" "$default")" ans
    printf '%s\n' "${ans:-$default}"
}

# ask_yn <提示语> <y|n 缺省> → 返回 0=是 1=否（务必在 if 条件中调用）
ask_yn() {
    local prompt="$1" default="$2" ans="" hint
    if [[ "$FLAG_YES" == "1" ]] || [[ ! -t 0 ]]; then
        [[ "$default" == "y" ]] && return 0
        return 1
    fi
    if [[ "$default" == "y" ]]; then hint="Y/n"; else hint="y/N"; fi
    read -r -p "$(printf '%s?%s %s [%s]: ' "$C_YELLOW" "$C_NC" "$prompt" "$hint")" ans
    case "${ans:-$default}" in
        y|Y|yes|YES|是) return 0 ;;
        *) return 1 ;;
    esac
}

# ── 5. TOML 合并写入（只改指定键，保留文件其余内容）─────────────────────────
# get_toml_key <section> <key> → 打印去引号后的值（不存在则输出空）
get_toml_key() {
    local section="$1" key="$2"
    [[ -f "$CONFIG_FILE" ]] || return 0
    sed -n "/^[[:space:]]*\[${section}\]/,/^[[:space:]]*\[/p" "$CONFIG_FILE" \
        | grep -E "^[[:space:]]*${key}[[:space:]]*=" \
        | head -n 1 \
        | sed -E 's/^[^=]*=[[:space:]]*//' \
        | sed -E 's/[[:space:]]*(#.*)?$//' \
        | tr -d '"' || true
    return 0
}

# set_toml_key <section> <key> <value字面量>
#   value 必须是合法 TOML 字面量（字符串含双引号，如 '"127.0.0.1"'；数字/布尔裸值）。
#   键已存在 → 整行替换（保留所在 section 位置）；
#   键不存在 → 追加到对应 section 末尾；section 不存在 → 文末新建 section。
#   通过 cat 回写保持原文件属主/权限（sudo 下不改属主）。
set_toml_key() {
    local section="$1" key="$2" value="$3"
    [[ -f "$CONFIG_FILE" ]] || die "配置文件不存在: $CONFIG_FILE"
    local tmp in_section=0 replaced=0 section_found=0 line name
    local header_re="^[[:space:]]*\[([^][]+)\][[:space:]]*(#.*)?$"
    local key_re="^[[:space:]]*${key}[[:space:]]*="
    tmp="$(mktemp)"
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ "$line" =~ $header_re ]]; then
            # 离开目标 section 前若尚未写入 → 追加到该 section 末尾
            if [[ "$in_section" == "1" && "$replaced" == "0" ]]; then
                printf '%s = %s\n' "$key" "$value" >> "$tmp"
                replaced=1
            fi
            name="${BASH_REMATCH[1]}"
            if [[ "$name" == "$section" ]]; then
                in_section=1; section_found=1
            else
                in_section=0
            fi
            printf '%s\n' "$line" >> "$tmp"
            continue
        fi
        if [[ "$in_section" == "1" && "$replaced" == "0" && "$line" =~ $key_re ]]; then
            printf '%s = %s\n' "$key" "$value" >> "$tmp"
            replaced=1
            continue
        fi
        printf '%s\n' "$line" >> "$tmp"
    done < "$CONFIG_FILE"
    # 文件末尾仍在目标 section 内 → 追加；section 完全不存在 → 新建
    if [[ "$in_section" == "1" && "$replaced" == "0" ]]; then
        printf '%s = %s\n' "$key" "$value" >> "$tmp"
    elif [[ "$section_found" == "0" ]]; then
        printf '\n[%s]\n%s = %s\n' "$section" "$key" "$value" >> "$tmp"
    fi
    cat "$tmp" > "$CONFIG_FILE"
    rm -f "$tmp"
    ok "config/app.toml: [${section}] ${key} = ${value}"
}

# ── 6. 通用工具 ───────────────────────────────────────────────────────────────
is_loopback() {
    case "$1" in
        127.0.0.1|localhost|::1) return 0 ;;
        *) return 1 ;;
    esac
}

require_config() {
    [[ -f "$CONFIG_FILE" ]] || die "未找到 $CONFIG_FILE —— 请确认在解压目录根部运行本脚本（自包含部署）"
}

config_port() {
    local p
    p="$(get_toml_key server port)"
    p="${p:-9800}"
    printf '%s\n' "$p"
}

config_host() {
    local h
    h="$(get_toml_key server host)"
    h="${h:-127.0.0.1}"
    printf '%s\n' "$h"
}

ensure_root() {
    [[ "$(id -u)" == "0" ]] && return 0
    info "此步骤需要 root 权限，使用 sudo 重新执行（保留参数与交互）..."
    exec sudo -E bash "$SCRIPT_PATH" "${ORIG_ARGS[@]}"
}

# ── 7. 发行版族探测（/etc/os-release 的 ID/ID_LIKE → deb|rpm|arch|unknown）──
detect_distro_family() {
    local id="" id_like="" tok
    if [[ -f /etc/os-release ]]; then
        id="$(grep -E '^ID=' /etc/os-release | head -n1 | cut -d= -f2- | tr -d '"' || true)"
        id_like="$(grep -E '^ID_LIKE=' /etc/os-release | head -n1 | cut -d= -f2- | tr -d '"' || true)"
    fi
    local toks=()
    read -ra toks <<< "${id} ${id_like}"
    for tok in "${toks[@]}"; do
        case "$tok" in
            debian|ubuntu|linuxmint|mint|pop)          echo "deb";  return 0 ;;
            fedora|rhel|centos|rocky|almalinux|ol)     echo "rpm";  return 0 ;;
            arch|manjaro|endeavouros)                  echo "arch"; return 0 ;;
        esac
    done
    echo "unknown"
}

# ── 8. 系统依赖安装（幂等：探测已装跳过）─────────────────────────────────────
pkg_missing() { ! command -v "$1" >/dev/null 2>&1; }

install_deb_deps() {
    local missing=() p
    for p in ffmpeg python3 curl; do
        if pkg_missing "$p"; then missing+=("$p"); else ok "已安装，跳过: $p"; fi
    done
    if [[ "${#missing[@]}" -eq 0 ]]; then ok "Debian 系依赖已齐备"; return 0; fi
    info "apt-get install -y ${missing[*]}"
    apt-get install -y "${missing[@]}" || die "apt-get 安装依赖失败: ${missing[*]}"
}

install_rpm_deps() {
    local missing=() p
    for p in python3 curl; do
        if pkg_missing "$p"; then missing+=("$p"); else ok "已安装，跳过: $p"; fi
    done
    if [[ "${#missing[@]}" -gt 0 ]]; then
        info "dnf install -y ${missing[*]}"
        dnf install -y "${missing[@]}" || die "dnf 安装依赖失败: ${missing[*]}"
    fi
    # ffmpeg：默认走 RPM Fusion free；--ffmpeg-source free 改装官方 ffmpeg-free
    if command -v ffmpeg >/dev/null 2>&1; then
        ok "已安装，跳过: ffmpeg"
        return 0
    fi
    if [[ "$FLAG_FFMPEG_SOURCE" == "free" ]]; then
        warn "使用发行版官方 ffmpeg-free：部分编解码器（如 H.264/AAC 编码）受限"
        dnf install -y ffmpeg-free || die "dnf 安装 ffmpeg-free 失败"
        return 0
    fi
    local fedora_ver el_ver
    fedora_ver="$(rpm -E %fedora 2>/dev/null || true)"
    el_ver="$(rpm -E %{rhel} 2>/dev/null || true)"
    if [[ "$fedora_ver" =~ ^[0-9]+$ ]]; then
        info "启用 RPM Fusion free（Fedora ${fedora_ver}）..."
        dnf install -y "https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-${fedora_ver}.noarch.rpm" \
            || die "RPM Fusion free 仓库启用失败"
    elif [[ "$el_ver" =~ ^[0-9]+$ ]]; then
        info "启用 EPEL + RPM Fusion free（EL ${el_ver}）..."
        dnf install -y epel-release || die "epel-release 安装失败"
        dnf install -y "https://mirrors.rpmfusion.org/free/el/rpmfusion-free-release-${el_ver}.noarch.rpm" \
            || die "RPM Fusion free 仓库启用失败"
    else
        die "无法识别 rpm 系发行版版本（rpm -E %fedora / %{rhel} 均非数字），请手动安装 ffmpeg 后重试"
    fi
    dnf install -y ffmpeg || die "dnf 安装 ffmpeg 失败（确认 RPM Fusion free 已启用）"
}

install_arch_deps() {
    info "pacman -S --needed --noconfirm ffmpeg python curl"
    pacman -S --needed --noconfirm ffmpeg python curl || die "pacman 安装依赖失败"
}

install_uv() {
    # 模块 venv 强依赖；已在 PATH 则跳过
    if command -v uv >/dev/null 2>&1; then
        ok "已安装，跳过: uv ($(command -v uv))"
        return 0
    fi
    if [[ "$DISTRO_FAMILY" == "arch" ]]; then
        info "pacman -S --needed --noconfirm uv"
        pacman -S --needed --noconfirm uv || die "pacman 安装 uv 失败"
    else
        info "通过 astral 官方安装器安装 uv（安装到 ~/.local/bin）..."
        curl -LsSf https://astral.sh/uv/install.sh | sh || die "uv 安装失败（astral installer）"
    fi
    if command -v uv >/dev/null 2>&1; then
        ok "uv 已就绪: $(command -v uv)"
    elif [[ -x "${HOME}/.local/bin/uv" ]]; then
        warn "uv 已安装到 ${HOME}/.local/bin，但当前 PATH 未包含该目录。请执行："
        warn "    export PATH=\"\$HOME/.local/bin:\$PATH\""
        warn "（建议写入 ~/.bashrc 持久化；systemd 服务侧由 unit 自动补充 PATH）"
    else
        die "uv 安装后仍未找到，请手动安装: https://docs.astral.sh/uv/"
    fi
}

install_system_deps() {
    step "安装系统依赖（发行版族: $DISTRO_FAMILY）"
    ensure_root
    case "$DISTRO_FAMILY" in
        deb)  install_deb_deps ;;
        rpm)  install_rpm_deps ;;
        arch) install_arch_deps ;;
        *)    warn "未识别的发行版族，跳过包管理器步骤；请手动安装: ffmpeg python3 curl uv" ;;
    esac
    install_uv
}

# ── 9. 配置向导（合并式写入 config/app.toml）─────────────────────────────────
generate_token() {
    if command -v openssl >/dev/null 2>&1; then
        openssl rand -hex 32
    else
        head -c 256 /dev/urandom | tr -dc 'a-f0-9' | head -c 64
        printf '\n'
    fi
}

run_config_wizard() {
    step "配置向导（合并式写入 $CONFIG_FILE）"
    require_config
    local cur_host cur_port cur_public cur_http cur_https
    cur_host="$(config_host)"
    cur_port="$(config_port)"
    cur_public="$(get_toml_key server allow_public)"; cur_public="${cur_public:-false}"
    cur_http="$(get_toml_key network http_proxy)"
    cur_https="$(get_toml_key network https_proxy)"

    # host —— 缺省沿用现值；提示局域网可改 0.0.0.0
    local host
    if [[ -n "$FLAG_HOST" ]]; then
        host="$FLAG_HOST"
    else
        host="$(ask_value "监听地址 host（仅本机访问保持 127.0.0.1；局域网访问可填 0.0.0.0）" "$cur_host")"
    fi
    [[ -n "$host" ]] || die "host 不能为空"
    set_toml_key server host "\"${host}\""

    # port —— 数字 1..65535 校验
    local port
    if [[ -n "$FLAG_PORT" ]]; then
        port="$FLAG_PORT"
    else
        port="$(ask_value "WebUI 端口 port" "$cur_port")"
    fi
    [[ "$port" =~ ^[0-9]+$ ]] || die "端口必须是数字: $port"
    if [[ "$port" -lt 1 || "$port" -gt 65535 ]]; then die "端口超出范围 1-65535: $port"; fi
    set_toml_key server port "$port"

    # allow_public —— 仅当 host 非回环时追问（回环时仅接受显式 flag）
    if is_loopback "$host"; then
        if [[ -n "$FLAG_ALLOW_PUBLIC" ]]; then
            set_toml_key server allow_public "$FLAG_ALLOW_PUBLIC"
            warn "host 为回环地址，allow_public=${FLAG_ALLOW_PUBLIC} 实际不会对外暴露"
        fi
    else
        local ap_ans ap_default="n"
        [[ "$cur_public" == "true" ]] && ap_default="y"
        if [[ -n "$FLAG_ALLOW_PUBLIC" ]]; then
            ap_ans="$FLAG_ALLOW_PUBLIC"
        elif ask_yn "检测到非回环地址 ${host}：是否允许公网访问（allow_public=true）？" "$ap_default"; then
            ap_ans="true"
        else
            ap_ans="false"
        fi
        set_toml_key server allow_public "$ap_ans"
    fi

    # [api] token —— 可选；对外暴露时强烈建议配置
    local token=""
    if [[ -n "$FLAG_API_TOKEN" ]]; then
        token="$FLAG_API_TOKEN"
    elif [[ "$FLAG_NO_TOKEN" == "1" ]]; then
        info "跳过 API token 配置（--no-token）"
    else
        local want_token="n"
        if ! is_loopback "$host"; then
            warn "host=${host} 对外暴露：强烈建议为统一推理 API（/api/v1/*）配置访问 token"
            want_token="y"
        fi
        if ask_yn "是否为统一推理 API（/api/v1/*）设置访问 token？" "$want_token"; then
            token="$(generate_token)"
            info "已生成随机 token（写入 [api] token）"
        fi
    fi
    if [[ -n "$token" ]]; then
        set_toml_key api token "\"${token}\""
        ok "API token 已配置（回显: ${token:0:6}********）"
    fi

    # [network] 代理 —— 可选，缺省不动
    local ans
    if [[ "$FLAG_HTTP_PROXY_SET" == "1" ]]; then
        set_toml_key network http_proxy "\"${FLAG_HTTP_PROXY}\""
    else
        ans="$(ask_value "HTTP 代理 http_proxy（留空保持现状不动）" "$cur_http")"
        [[ "$ans" != "$cur_http" ]] && set_toml_key network http_proxy "\"${ans}\""
    fi
    if [[ "$FLAG_HTTPS_PROXY_SET" == "1" ]]; then
        set_toml_key network https_proxy "\"${FLAG_HTTPS_PROXY}\""
    else
        ans="$(ask_value "HTTPS 代理 https_proxy（留空保持现状不动）" "$cur_https")"
        [[ "$ans" != "$cur_https" ]] && set_toml_key network https_proxy "\"${ans}\""
    fi

    ok "配置向导完成"
}

# ── 10. systemd 服务（unit 内嵌模板现场渲染；绝不 enable）──────────────────
resolve_service_user() {
    if [[ -n "$FLAG_USER" ]]; then
        printf '%s\n' "$FLAG_USER"
        return 0
    fi
    local owner
    owner="$(stat -c %U "$EP_ROOT")"
    # root 属主且经 sudo 运行 → 回退到发起 sudo 的真实用户
    if [[ "$owner" == "root" && -n "${SUDO_USER:-}" ]]; then
        owner="$SUDO_USER"
    fi
    printf '%s\n' "$owner"
}

render_unit() {
    # 唯一权威 unit 模板。注意：刻意不启用 ProtectHome ——
    # EP_ROOT 可能位于 /home 或任意挂载点下，ProtectHome 会导致服务无法访问。
    local user="$1" extra_env="$2"
    cat <<EOF
[Unit]
Description=EntryPoint AI Module Orchestrator
After=network.target

[Service]
Type=simple
User=${user}
Environment=EP_ROOT=${EP_ROOT}
Environment=RUST_LOG=info
${extra_env}WorkingDirectory=${EP_ROOT}
ExecStart=${EP_ROOT}/bin/ep-daemon
Restart=on-failure
RestartSec=5
TimeoutStopSec=30
StandardOutput=journal
StandardError=journal
SyslogIdentifier=entrypoint

# 安全加固（不启用 ProtectHome：EP_ROOT 可能在 /home 或任意挂载点下）
NoNewPrivileges=true
ProtectSystem=strict
ReadWritePaths=${EP_ROOT}
PrivateTmp=yes
UMask=0027

[Install]
WantedBy=multi-user.target
EOF
}

# uv 若装在非系统目录（如 ~/.local/bin），systemd 默认 PATH 找不到 →
# 在 unit 中显式补充 PATH（daemon 派生模块 venv 子进程依赖 uv 可发现）。
unit_extra_env() {
    local uv_bin uv_dir
    uv_bin="$(command -v uv || true)"
    [[ -n "$uv_bin" ]] || return 0
    uv_dir="$(dirname "$uv_bin")"
    case "$uv_dir" in
        /usr/local/sbin|/usr/local/bin|/usr/sbin|/usr/bin|/sbin|/bin) return 0 ;;
        *) printf 'Environment=PATH=%s:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n' "$uv_dir" ;;
    esac
}

wait_for_health() {
    # 轮询 /api/health 最多 30s
    local port="$1" i=0 url
    url="$(printf "$HEALTH_URL_FMT" "$port")"
    if ! command -v curl >/dev/null 2>&1; then
        warn "curl 不可用，跳过健康自检（请手动访问 ${url} 确认）"
        return 0
    fi
    while [[ "$i" -lt 30 ]]; do
        if curl -fsS "$url" >/dev/null 2>&1; then
            ok "健康自检通过: ${url}"
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    return 1
}

install_service() {
    step "注册 systemd 服务（unit: ${UNIT_PATH}）"
    ensure_root
    [[ -x "${EP_ROOT}/bin/ep-daemon" ]] || die "未找到可执行的 ${EP_ROOT}/bin/ep-daemon —— 包不完整，拒绝注册必然启动失败的服务"

    local svc_user extra_env port
    svc_user="$(resolve_service_user)"
    if [[ "$svc_user" == "root" ]]; then
        warn "服务将以 root 用户运行（EP_ROOT 属主为 root 且无 SUDO_USER 可回退）。"
        warn "出于安全考虑，建议创建专用用户并以 --user <name> 重新安装。"
    fi
    extra_env="$(unit_extra_env)"
    port="$(config_port)"

    info "服务用户: ${svc_user} | EP_ROOT: ${EP_ROOT}"
    render_unit "$svc_user" "$extra_env" > "/etc/systemd/system/.${UNIT_NAME}.service.tmp"
    install -m 644 "/etc/systemd/system/.${UNIT_NAME}.service.tmp" "$UNIT_PATH"
    rm -f "/etc/systemd/system/.${UNIT_NAME}.service.tmp"
    systemctl daemon-reload
    ok "unit 已写入: ${UNIT_PATH}（未执行 enable —— 开机自启由您自行决定）"

    if systemctl is-active --quiet "$UNIT_NAME"; then
        info "服务已在运行，执行 restart 以应用新 unit..."
        systemctl restart "$UNIT_NAME" || {
            warn "systemctl restart 失败，最近日志："
            journalctl -u "$UNIT_NAME" -n 50 --no-pager || true
            die "服务重启失败"
        }
    else
        systemctl start "$UNIT_NAME" || {
            warn "systemctl start 失败，最近日志："
            journalctl -u "$UNIT_NAME" -n 50 --no-pager || true
            die "服务启动失败"
        }
    fi

    if wait_for_health "$port"; then
        ok "服务已启动并通过健康自检"
    else
        warn "服务启动后 30s 内健康自检未通过（http://127.0.0.1:${port}/api/health），最近日志："
        journalctl -u "$UNIT_NAME" -n 50 --no-pager || true
        die "健康自检失败（服务已注册但可能未正常工作，请用 journalctl -u ${UNIT_NAME} 排查）"
    fi
}

# ── 11. 防火墙 / SELinux ─────────────────────────────────────────────────────
configure_firewall() {
    local host="$1" port="$2"
    step "防火墙配置"
    if [[ "$FLAG_NO_FIREWALL" == "1" ]]; then
        info "跳过防火墙配置（--no-firewall）"
        return 0
    fi
    if is_loopback "$host"; then
        info "host=${host} 为回环地址，仅本机可访问，默认跳过防火墙配置"
        return 0
    fi
    ensure_root
    if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
        info "firewalld 活动，开放 ${port}/tcp ..."
        if firewall-cmd --permanent --add-port="${port}/tcp" && firewall-cmd --reload; then
            ok "firewalld 已开放 ${port}/tcp"
        elif firewall-cmd --list-ports | grep -qE "(^| )${port}/tcp( |$)"; then
            ok "端口 ${port}/tcp 已开放（ALREADY_ENABLED，视为成功）"
        else
            warn "firewalld 配置失败，请手动执行: firewall-cmd --permanent --add-port=${port}/tcp && firewall-cmd --reload"
        fi
    elif command -v ufw >/dev/null 2>&1; then
        info "检测到 ufw，开放 ${port}/tcp ..."
        if ufw allow "${port}/tcp"; then
            ok "ufw 已开放 ${port}/tcp"
        else
            warn "ufw 配置失败，请手动执行: ufw allow ${port}/tcp"
        fi
    else
        warn "未检测到 firewalld/ufw，请手动开放端口 ${port}/tcp（如 iptables/nftables 规则）"
    fi
}

configure_selinux() {
    local port="$1"
    [[ "$DISTRO_FAMILY" == "rpm" ]] || return 0
    if [[ "$FLAG_SKIP_SELINUX" == "1" ]]; then
        info "跳过 SELinux 配置（--skip-selinux）"
        return 0
    fi
    command -v getenforce >/dev/null 2>&1 || return 0
    [[ "$(getenforce 2>/dev/null || true)" == "Enforcing" ]] || {
        info "SELinux 非 Enforcing，跳过"
        return 0
    }
    step "SELinux 端口标签配置"
    ensure_root
    if command -v semanage >/dev/null 2>&1; then
        if semanage port -a -t http_port_t -p tcp "$port" 2>/dev/null; then
            ok "已添加 SELinux 端口标签 http_port_t: ${port}/tcp"
        elif semanage port -m -t http_port_t -p tcp "$port" 2>/dev/null; then
            ok "已更新 SELinux 端口标签为 http_port_t: ${port}/tcp"
        else
            warn "SELinux 端口标签配置失败，请手动检查: semanage port -l | grep ${port}"
        fi
    else
        warn "未找到 semanage。请安装 policycoreutils-python-utils 后手动执行:"
        warn "    semanage port -a -t http_port_t -p tcp ${port}"
    fi
    return 0
}

# ── 12. 摘要输出 ──────────────────────────────────────────────────────────────
print_summary() {
    local host="$1" port="$2" with_service="$3"
    step "安装完成"
    if is_loopback "$host"; then
        info "WebUI 地址:    http://localhost:${port}"
    else
        info "WebUI 地址:    http://${host}:${port}（host=${host} 时请用服务器实际 IP 访问）"
    fi
    info "配置文件:      ${CONFIG_FILE}"
    info "数据根目录:    ${EP_ROOT}（自包含：配置/模型/运行产物均在此目录内）"
    if [[ "$with_service" == "1" ]]; then
        info "常用操作:"
        info "    状态:  ./deploy.sh status    （或 systemctl status ${UNIT_NAME}）"
        info "    日志:  ./deploy.sh logs -f   （或 journalctl -u ${UNIT_NAME} -f）"
        info "    停止:  ./deploy.sh stop"
        info "    卸载:  ./deploy.sh uninstall"
        info "注意:      未设置开机自启（按项目约定不执行 enable）；如需自启请手动:"
        info "           systemctl enable ${UNIT_NAME}"
    else
        info "未注册 systemd 服务。前台启动: ./start-daemon.sh"
    fi
    info "模型下载:      请通过 WebUI 的模块页进行（models/ 目录自包含管理）"
}

# ── 13. 子命令: install ───────────────────────────────────────────────────────
cmd_install() {
    step "EntryPoint 安装（自包含目录: ${EP_ROOT}）"
    require_config
    [[ -x "${EP_ROOT}/bin/ep-daemon" ]] || die "未找到可执行的 ${EP_ROOT}/bin/ep-daemon —— 请在完整解压目录内运行"

    # 1) 发行版族探测
    if [[ -n "$FLAG_DISTRO" ]]; then
        DISTRO_FAMILY="$FLAG_DISTRO"
        info "使用指定的发行版族: ${DISTRO_FAMILY}（--distro）"
    else
        DISTRO_FAMILY="$(detect_distro_family)"
        info "探测到发行版族: ${DISTRO_FAMILY}"
    fi
    case "$DISTRO_FAMILY" in
        deb|rpm|arch|unknown) ;;
        *) die "无效的 --distro 值: ${DISTRO_FAMILY}（可选: deb rpm arch unknown）" ;;
    esac
    if [[ "$DISTRO_FAMILY" == "unknown" ]]; then
        warn "未识别发行版：将跳过包管理器步骤（文件安装照常）"
    fi

    # 2) 系统依赖安装
    if [[ "$FLAG_SKIP_DEPS" == "1" ]]; then
        info "跳过系统依赖安装（--skip-deps）"
    else
        install_system_deps
    fi

    # 3) 配置向导
    run_config_wizard
    local host port
    host="$(config_host)"
    port="$(config_port)"

    # 4) systemd 服务（可选；交互缺省 Y；--yes 缺省注册；绝不 enable）
    local with_service=0
    if [[ "$FLAG_NO_SERVICE" == "1" ]]; then
        info "跳过 systemd 服务注册（--no-service）"
    elif [[ "$FLAG_WITH_SERVICE" == "1" ]]; then
        with_service=1
    elif ask_yn "是否注册为 systemd 服务？" "y"; then
        with_service=1
    else
        info "跳过 systemd 服务注册"
    fi
    if [[ "$with_service" == "1" ]]; then
        install_service
    fi

    # 5) 防火墙（仅非回环）6) SELinux（仅 rpm 族 Enforcing）
    configure_firewall "$host" "$port"
    configure_selinux "$port"

    # 7) 摘要
    print_summary "$host" "$port" "$with_service"
}

# ── 14. 子命令: uninstall ─────────────────────────────────────────────────────
cmd_uninstall() {
    step "卸载 EntryPoint"
    if [[ -f "$UNIT_PATH" ]]; then
        ensure_root
        info "停止并移除 systemd 服务..."
        systemctl stop "$UNIT_NAME" 2>/dev/null || true
        rm -f "$UNIT_PATH"
        systemctl daemon-reload
        systemctl reset-failed "$UNIT_NAME" 2>/dev/null || true
        ok "systemd 服务已移除（unit: ${UNIT_PATH}）"
    else
        info "未检测到 systemd 服务（${UNIT_PATH} 不存在），跳过服务移除"
    fi

    if [[ "$FLAG_PURGE" == "1" ]]; then
        warn "--purge 将永久删除整个解压目录: ${EP_ROOT}"
        warn "（包括配置、已下载模型、运行产物与本脚本自身，不可恢复）"
        if [[ "$FLAG_YES" != "1" ]]; then
            local confirm=""
            if [[ -t 0 ]]; then
                read -r -p "$(printf '%s请输入 yes 确认删除 %s: %s' "$C_RED" "$EP_ROOT" "$C_NC")" confirm
            fi
            [[ "$confirm" == "yes" ]] || die "已取消（未输入 yes）"
        fi
        # rm 会删除本脚本所在目录：用单条复合命令完成删除并立即退出，
        # 避免 bash 继续从已删除文件读取后续指令。
        rm -rf -- "$EP_ROOT" && { printf '%s[OK]%s 已删除 %s\n' "$C_GREEN" "$C_NC" "$EP_ROOT"; exit 0; }
        die "删除失败: ${EP_ROOT}（可能有文件权限问题，请用 sudo 重试）"
    fi
    info "已保留数据目录: ${EP_ROOT}（如需彻底清除: ./deploy.sh uninstall --purge）"
    ok "卸载完成"
}

# ── 15. 子命令: status ────────────────────────────────────────────────────────
cmd_status() {
    step "EntryPoint 状态"
    info "EP_ROOT: ${EP_ROOT}"
    if [[ -f "$VERSION_FILE" ]]; then
        info "版本信息（VERSION.txt）:"
        sed 's/^/    /' "$VERSION_FILE"
    else
        warn "VERSION.txt 不存在（非标准发布包？）"
    fi

    if [[ -f "$UNIT_PATH" ]]; then
        info "systemd 服务:"
        systemctl status "$UNIT_NAME" --no-pager 2>/dev/null | sed 's/^/    /' || true
    else
        info "systemd 服务: 未注册（${UNIT_PATH} 不存在）"
    fi

    local port url
    port="$(config_port 2>/dev/null || echo 9800)"
    url="$(printf "$HEALTH_URL_FMT" "$port")"
    if command -v curl >/dev/null 2>&1 && curl -fsS "$url" >/dev/null 2>&1; then
        ok "健康探测: ${url} 响应正常"
    else
        warn "健康探测: ${url} 无响应（服务未运行或端口不符）"
    fi

    info "依赖体检:"
    local t
    for t in ffmpeg python3 uv curl; do
        if command -v "$t" >/dev/null 2>&1; then
            ok "    ${t}: $(command -v "$t")"
        else
            warn "    ${t}: 未找到"
        fi
    done
    return 0
}

# ── 16. 子命令: start / stop ──────────────────────────────────────────────────
cmd_start() {
    if [[ -f "$UNIT_PATH" ]]; then
        ensure_root
        if systemctl is-active --quiet "$UNIT_NAME"; then
            info "服务已在运行，转为 restart..."
            systemctl restart "$UNIT_NAME"
        else
            systemctl start "$UNIT_NAME"
        fi
        ok "服务已启动: systemctl status ${UNIT_NAME}"
    else
        info "未注册 systemd 服务。请前台运行: ./start-daemon.sh"
        info "（或先 ./deploy.sh install 注册服务）"
    fi
}

cmd_stop() {
    if [[ -f "$UNIT_PATH" ]]; then
        ensure_root
        systemctl stop "$UNIT_NAME"
        ok "服务已停止"
    else
        info "未注册 systemd 服务。若通过 start-daemon.sh 前台运行，请直接 Ctrl-C 结束。"
    fi
}

# ── 17. 子命令: logs ──────────────────────────────────────────────────────────
cmd_logs() {
    [[ -f "$UNIT_PATH" ]] || die "未注册 systemd 服务（${UNIT_PATH} 不存在），无 journal 日志可看"
    # -f / -n 已在参数解析阶段收集到 LOGS_ARGS
    exec journalctl -u "$UNIT_NAME" "${LOGS_ARGS[@]}"
}

# ── 18. 子命令: configure ─────────────────────────────────────────────────────
cmd_configure() {
    run_config_wizard
    if [[ -f "$UNIT_PATH" ]] && systemctl is-active --quiet "$UNIT_NAME" 2>/dev/null; then
        info "配置已落盘。重启服务以生效: ./deploy.sh start（运行中会自动 restart）"
    else
        info "配置已落盘。下次启动服务/daemon 时生效。"
    fi
}

# ── 19. 子命令: check（只读诊断，不变更任何系统状态）────────────────────────
cmd_check() {
    step "部署自检（只读诊断）"
    local fails=0
    local family port="" host

    # 发行版族
    family="$(detect_distro_family)"
    info "发行版族: ${family}"

    # 依赖存在性
    local t
    for t in ffmpeg python3 uv curl; do
        if command -v "$t" >/dev/null 2>&1; then
            ok "依赖 ${t}: $(command -v "$t")"
        else
            warn "依赖 ${t}: 未找到（install 会自动安装）"
            fails=$((fails + 1))
        fi
    done

    # EP_ROOT 可写
    if [[ -w "$EP_ROOT" ]]; then
        ok "EP_ROOT 可写: ${EP_ROOT}"
    else
        warn "EP_ROOT 不可写: ${EP_ROOT}"
        fails=$((fails + 1))
    fi

    # 配置文件可读 + 端口键可解析
    if [[ -r "$CONFIG_FILE" ]]; then
        ok "配置文件可读: ${CONFIG_FILE}"
        port="$(get_toml_key server port)"
        if [[ "$port" =~ ^[0-9]+$ ]] && [[ "$port" -ge 1 ]] && [[ "$port" -le 65535 ]]; then
            ok "[server] port = ${port}（可解析）"
        else
            warn "[server] port 缺失或非法: '${port}'"
            fails=$((fails + 1))
        fi
    else
        warn "配置文件不存在或不可读: ${CONFIG_FILE}"
        fails=$((fails + 1))
    fi
    host="$(config_host 2>/dev/null || true)"

    # bin/ep-daemon 存在且可执行
    if [[ -x "${EP_ROOT}/bin/ep-daemon" ]]; then
        ok "daemon 二进制: ${EP_ROOT}/bin/ep-daemon"
    else
        warn "daemon 二进制缺失或不可执行: ${EP_ROOT}/bin/ep-daemon"
        fails=$((fails + 1))
    fi

    # unit 一致性（若存在）
    if [[ -f "$UNIT_PATH" ]]; then
        local exec_start
        exec_start="$(grep -E '^ExecStart=' "$UNIT_PATH" | head -n1 | cut -d= -f2-)"
        if [[ "$exec_start" == "${EP_ROOT}/bin/ep-daemon" ]]; then
            ok "unit ExecStart 与本目录一致: ${exec_start}"
        else
            warn "unit ExecStart 与本目录不一致: unit=${exec_start:-<缺失>} 本目录=${EP_ROOT}/bin/ep-daemon"
            fails=$((fails + 1))
        fi
    else
        info "systemd unit 不存在（尚未注册服务）"
    fi

    # 端口占用探测
    port="${port:-9800}"
    if [[ "$port" =~ ^[0-9]+$ ]]; then
        if (exec 3<>"/dev/tcp/127.0.0.1/${port}") 2>/dev/null; then
            local url
            url="$(printf "$HEALTH_URL_FMT" "$port")"
            if command -v curl >/dev/null 2>&1 && curl -fsS "$url" >/dev/null 2>&1; then
                ok "端口 ${port} 已被占用：是本服务在运行（健康探测通过）"
            else
                warn "端口 ${port} 已被其他进程占用（健康探测非本服务），启动将失败"
                fails=$((fails + 1))
            fi
        else
            ok "端口 ${port} 空闲（host=${host}）"
        fi
    fi

    if [[ "$fails" -gt 0 ]]; then
        warn "自检发现 ${fails} 个问题"
        return 1
    fi
    ok "自检全部通过"
    return 0
}

# ── 20. help ──────────────────────────────────────────────────────────────────
usage() {
    cat <<'EOF'
EntryPoint 部署脚本（解压目录自包含 —— 在解压目录根部运行）

用法: ./deploy.sh [子命令] [flags]

子命令:
  install     完整安装（默认）: 系统依赖 → 配置向导 → 可选 systemd 服务 → 防火墙/SELinux
  uninstall   卸载: 移除 systemd 服务（保留数据目录）; --purge 连数据目录一并删除
  status      查看状态: 服务/健康探测/版本/依赖体检
  start       启动服务（已运行时转 restart）; 未注册服务则提示用 start-daemon.sh
  stop        停止服务
  logs        查看 journal 日志（透传 -f 跟随 / -n N 行数）
  configure   重新运行配置向导（合并式修改 config/app.toml）
  check       只读诊断（依赖/配置/二进制/unit 一致性/端口占用），退出码 0/1
  help        显示本帮助

全局 flags:
  --yes                  非交互模式，所有提问取缺省值
  --distro <family>      指定发行版族: deb | rpm | arch | unknown（默认自动探测）
  --skip-deps            跳过系统依赖安装步骤
  --ffmpeg-source <src>  rpm 族 ffmpeg 来源: fusion(默认, RPM Fusion free) | free(官方 ffmpeg-free, 编解码受限)
  --host <addr>          监听地址（缺省 127.0.0.1; 局域网访问用 0.0.0.0）
  --port <n>             WebUI 端口（缺省 9800）
  --allow-public         [server] allow_public=true（仅非回环 host 时询问/生效）
  --api-token <s>        为统一推理 API（/api/v1/*）设置访问 token
  --no-token             跳过 token 配置
  --http-proxy <url>     设置 [network] http_proxy（空字符串=清空）
  --https-proxy <url>    设置 [network] https_proxy（空字符串=清空）
  --with-service         注册 systemd 服务（跳过提问）
  --no-service           不注册 systemd 服务
  --user <name>          服务运行用户（缺省=EP_ROOT 目录属主; root 属主经 sudo 时取 SUDO_USER）
  --no-firewall          跳过防火墙配置
  --skip-selinux         跳过 SELinux 配置
  --purge                (uninstall) 删除整个解压目录（危险！含配置与已下载模型）
  -f                     (logs) 跟随日志
  -n <N>                 (logs) 显示最近 N 行
  -h, --help             显示帮助

示例:
  ./deploy.sh                                   # 交互式完整安装
  ./deploy.sh install --yes                     # 全自动: 缺省配置 + 注册服务
  ./deploy.sh --host 0.0.0.0 --port 8080 --yes  # 局域网部署
  ./deploy.sh configure --port 9999 --yes       # 只改端口
  ./deploy.sh uninstall --purge --yes           # 彻底清除（危险）

约定: 本脚本绝不执行 systemctl enable（开机自启由您显式决定）。
EOF
}

# ── 21. 参数解析与主入口 ──────────────────────────────────────────────────────
main() {
    ORIG_ARGS=("$@")
    local cmd="install"
    if [[ $# -gt 0 && "$1" != -* ]]; then
        case "$1" in
            install|uninstall|status|start|stop|logs|configure|check) cmd="$1"; shift ;;
            help|-h|--help) usage; exit 0 ;;
            *) die "未知子命令: $1（./deploy.sh help 查看用法）" ;;
        esac
    fi

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --yes)            FLAG_YES=1; shift ;;
            --distro)         FLAG_DISTRO="${2:?--distro 需要参数}"; shift 2 ;;
            --skip-deps)      FLAG_SKIP_DEPS=1; shift ;;
            --ffmpeg-source)  FLAG_FFMPEG_SOURCE="${2:?--ffmpeg-source 需要参数}"; shift 2 ;;
            --host)           FLAG_HOST="${2:?--host 需要参数}"; shift 2 ;;
            --port)           FLAG_PORT="${2:?--port 需要参数}"; shift 2 ;;
            --allow-public)   FLAG_ALLOW_PUBLIC="true"; shift ;;
            --api-token)      FLAG_API_TOKEN="${2:?--api-token 需要参数}"; shift 2 ;;
            --no-token)       FLAG_NO_TOKEN=1; shift ;;
            --http-proxy)     FLAG_HTTP_PROXY_SET=1; FLAG_HTTP_PROXY="${2:-}"; shift 2 ;;
            --https-proxy)    FLAG_HTTPS_PROXY_SET=1; FLAG_HTTPS_PROXY="${2:-}"; shift 2 ;;
            --with-service)   FLAG_WITH_SERVICE=1; shift ;;
            --no-service)     FLAG_NO_SERVICE=1; shift ;;
            --user)           FLAG_USER="${2:?--user 需要参数}"; shift 2 ;;
            --no-firewall)    FLAG_NO_FIREWALL=1; shift ;;
            --skip-selinux)   FLAG_SKIP_SELINUX=1; shift ;;
            --purge)          FLAG_PURGE=1; shift ;;
            -h|--help)        usage; exit 0 ;;
            -f)
                [[ "$cmd" == "logs" ]] || die "-f 仅 logs 子命令支持"
                LOGS_ARGS+=("-f"); shift ;;
            -n)
                [[ "$cmd" == "logs" ]] || die "-n 仅 logs 子命令支持"
                LOGS_ARGS+=("-n" "${2:?-n 需要行数参数}"); shift 2 ;;
            *) die "未知选项: $1（./deploy.sh help 查看用法）" ;;
        esac
    done

    # flag 组合校验
    if [[ "$FLAG_WITH_SERVICE" == "1" && "$FLAG_NO_SERVICE" == "1" ]]; then
        die "--with-service 与 --no-service 互斥"
    fi
    if [[ -n "$FLAG_API_TOKEN" && "$FLAG_NO_TOKEN" == "1" ]]; then
        die "--api-token 与 --no-token 互斥"
    fi
    case "$FLAG_FFMPEG_SOURCE" in
        fusion|free) ;;
        *) die "无效的 --ffmpeg-source: ${FLAG_FFMPEG_SOURCE}（可选: fusion free）" ;;
    esac
    if [[ "$cmd" == "logs" && "${#LOGS_ARGS[@]}" -eq 0 ]]; then
        LOGS_ARGS+=("--no-pager")
    fi

    case "$cmd" in
        install)    cmd_install ;;
        uninstall)  cmd_uninstall ;;
        status)     cmd_status ;;
        start)      cmd_start ;;
        stop)       cmd_stop ;;
        logs)       cmd_logs ;;
        configure)  cmd_configure ;;
        check)      cmd_check ;;
    esac
}

main "$@"
