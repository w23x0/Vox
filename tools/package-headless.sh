#!/usr/bin/env bash
#
# Vox 无屏档的打包脚本：出一个可复现的 tar.gz（二进制 + systemd unit + 样例配置 + README）
# **或 deb**（S4-D，`docs/platform/EMBEDDED.md` §3.8 第 8 条；`DIRECTIONS.md` §10.9 第 6 行）。
#
# 为什么 tar.gz 还留着：无屏盒子多半是刷进去的镜像，装的是个目录树，不是包管理器；
# 要装成服务就用里面那两份 unit（步骤见 README.md §2）。
#
# 为什么加 deb：Debian 系（Debian / Raspberry Pi OS）上"一条命令装好"只有包管理器这条路
# （EMBEDDED §3.8）。deb 装的是**系统单元**那一档（User=vox、/var/lib/vox/settings.json、
# /usr/local/bin/vox-headless），装上只启用不启动；用户单元（推荐档）仍是手动装，见 README §2.1。
#
# 可复现：tar 的排序/时间戳/属主都钉死（`--sort=name --mtime=@0 --owner=0 --group=0`），
# 二进制用 `--locked` 编（锁文件就是输入的一部分）。同一份源码 + 同一套工具链 → 同一个哈希。
#   tar.gz：压缩由 tar 自己调（`-z`）：从管道读入时 gzip 头里没有文件名与时间戳，同样可复现。
#   deb：debian-binary / control.tar.gz / data.tar.gz 三个成员各自用同一套 tar 参数出字节
#    固定的 tarball，gzip -n 去名字与时间戳，最后 `ar rcD`（确定性模式）组装。
#    deb 的 Depends **不手抄**：由 `dpkg-shlibdeps` 从二进制 ELF 的 NEEDED 现解
#    （库表会随发行版变，抄的必腐）。交叉构建（target ≠ host）时 shlibdeps 解不了目标架构的
#    库，用 `--depends` 显式覆盖（Ubuntu 24.04 上 shlibdeps 会产出 libasound2t64，那是
#    Ubuntu 的包名；Debian / RPi OS 上叫 libasound2——给那些目标分发时请用 `--depends`）。
#
# 用法：
#   tools/package-headless.sh                                    # tar.gz（本机，推荐在板上原生编译）
#   tools/package-headless.sh --format deb                       # deb（host = target 时）
#   tools/package-headless.sh --target aarch64-unknown-linux-gnu # 交叉编译（要目标 sysroot + linker）
#   tools/package-headless.sh --format deb --target aarch64-unknown-linux-gnu \
#       --depends 'libc6 (>= 2.36), libasound2 (>= 1.2.6), libgcc-s1 (>= 12), adduser'
#   tools/package-headless.sh --profile debug                    # 冒烟用（编译快，二进制大）
#   tools/package-headless.sh --out /tmp/out
set -euo pipefail

# 排序、时间戳格式、语言都不该因为跑脚本的机器而变。
export LC_ALL=C
export TZ=UTC

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE_DIR="$REPO_ROOT/crates/vox-headless"
PACKAGE="vox-headless"

TARGET=""
# 输出目录名（`target/<三元组>/<PROFILE_DIR>`）只有 debug / release 两种。
PROFILE_DIR="release"
# 本机产物落 `tools/bundle/`：仓库的 `.gitignore` 已经把它排除在外（跟 `/tools/signing/` 同一类）。
OUT_DIR="$REPO_ROOT/tools/bundle"
# 产物格式：tar.gz（缺省，老行为不变）/ deb（S4-D）。
FORMAT="tar.gz"
# 仅 deb：显式 Depends（覆盖 shlibdeps；交叉构建时唯一的路）。
DEPENDS_OVERRIDE=""

usage() {
    cat <<'EOF'
用法：tools/package-headless.sh [--target <三元组>] [--profile release|debug] [--out <目录>]
                              [--format tar.gz|deb] [--depends "<包列表>"]

  --target    目标三元组；缺省 = 本机宿主（用 rustc -vV 的 host:）
  --profile   构建 profile：release（缺省，strip + lto）/ debug（冒烟用）
  --out       产物目录；缺省 <仓库>/tools/bundle（已 gitignore）
  --format    产物格式：tar.gz（缺省：二进制 + unit + 样例配置 + README 的目录树）
              / deb（安装到 /usr/local/bin + /usr/lib/systemd/{system,user} + /usr/share，
              系统单元装上即 enable；Depends 由 dpkg-shlibdeps 现解）
  --depends   仅 deb：Depends 用这一串，跳过 dpkg-shlibdeps（交叉/异构分发时给，
              例如 'libc6 (>= 2.36), libasound2 (>= 1.2.6), libgcc-s1 (>= 12), adduser'）
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --target)
            TARGET="${2:?--target 后面要给一个目标三元组}"
            shift 2
            ;;
        --profile)
            PROFILE_DIR="${2:?--profile 后面要给 release 或 debug}"
            shift 2
            ;;
        --out)
            OUT_DIR="${2:?--out 后面要给一个目录}"
            shift 2
            ;;
        --format)
            FORMAT="${2:?--format 后面要给 tar.gz 或 deb}"
            shift 2
            ;;
        --depends)
            DEPENDS_OVERRIDE="${2:?--depends 后面要给包列表}"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "不认识的参数：$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$FORMAT" in
    tar.gz | deb) ;;
    *) echo "--format 只认 tar.gz / deb，给的是 $FORMAT" >&2; exit 2 ;;
esac

# cargo 的 profile 名是 `dev`，产物目录叫 `debug`——把这两个名字的差别收在这一处。
case "$PROFILE_DIR" in
    release) CARGO_PROFILE="release" ;;
    debug | dev)
        PROFILE_DIR="debug"
        CARGO_PROFILE="dev"
        ;;
    *)
        echo "--profile 只认 release / debug，给的是 $PROFILE_DIR" >&2
        exit 2
        ;;
esac

# 目标三元组：缺省就是这台机器的宿主（在板上原生编译，EMBEDDED §3.1 的第一站建议）。
if [ -z "$TARGET" ]; then
    TARGET="$(rustc -vV | sed -n 's/^host: //p')"
    [ -n "$TARGET" ] || { echo "取不到宿主三元组（rustc -vV 里没有 host:）" >&2; exit 2; }
fi
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"

# 版本从 cargo 自己那儿取（不另立一份版本号）：`cargo pkgid` 的输出以 @ 或 # 接版本。
VERSION="$(cd "$REPO_ROOT" && cargo pkgid -p "$PACKAGE" | sed 's/.*[@#]//')"
[ -n "$VERSION" ] || { echo "取不到 $PACKAGE 的版本" >&2; exit 2; }

# 编。`--locked`：锁文件是输入的一部分，出包时不许悄悄改依赖（改了就报错，不改就是可复现）。
echo "==> cargo build --locked --profile $CARGO_PROFILE -p $PACKAGE --target $TARGET"
(cd "$REPO_ROOT" && cargo build --locked --profile "$CARGO_PROFILE" -p "$PACKAGE" --target "$TARGET")

BIN="$REPO_ROOT/target/$TARGET/$PROFILE_DIR/$PACKAGE"
[ -f "$BIN" ] || { echo "没找到刚编出来的二进制：$BIN" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# ---- 共用的那几件：二进制 + 两份 unit + 样例配置 + README ----------------------
# tar.gz 里是解包即用的目录树（§4 的形状）；deb 里按 FHS 摆位。两处的**同一文件**各装一份。

if [ "$FORMAT" = tar.gz ]; then
    # 归档内的顶层目录：解包不会把文件撒到当前目录里。
    NAME="$PACKAGE-$VERSION-$TARGET"
    ROOT="$STAGE/$NAME"
    install -Dm755 "$BIN" "$ROOT/bin/$PACKAGE"
    install -Dm644 "$CRATE_DIR/systemd/user/vox-headless.service" "$ROOT/systemd/user/vox-headless.service"
    install -Dm644 "$CRATE_DIR/systemd/system/vox-headless.service" "$ROOT/systemd/system/vox-headless.service"
    install -Dm644 "$CRATE_DIR/settings.example.json" "$ROOT/settings.example.json"
    install -Dm644 "$CRATE_DIR/README.md" "$ROOT/README.md"

    mkdir -p "$OUT_DIR"
    ARCHIVE="$OUT_DIR/$NAME.tar.gz"
    rm -f "$ARCHIVE"

    # 归档参数逐个说清：
    #   --sort=name       目录遍历顺序不影响字节（否则同一份输入会出不同哈希）
    #   --mtime=@0        所有成员时间戳钉成 epoch（给了 SOURCE_DATE_EPOCH 就用它）
    #   --owner/--group/--numeric-owner  不把打包者的 uid/gid 写进去
    #   --format=gnu      gzip 之前的字节形状固定
    # 压缩由 tar 自己调（`-z`）：从管道读入时 gzip 头里没有文件名与时间戳，同样可复现。
    tar --create --gzip --file "$ARCHIVE" \
        --directory "$STAGE" \
        --sort=name \
        --mtime="@${SOURCE_DATE_EPOCH:-0}" \
        --owner=0 --group=0 --numeric-owner \
        --format=gnu \
        "$NAME"

    echo
    echo "==> 产物：$ARCHIVE"
    echo "==> 大小：$(wc -c <"$ARCHIVE") 字节"
    echo "==> sha256：$(sha256sum "$ARCHIVE" | cut -d' ' -f1)"
    echo
    echo "==> tar tzf $ARCHIVE"
    tar tzf "$ARCHIVE"
    exit 0
fi

# ---- deb（S4-D）---------------------------------------------------------------

# 三元组 → Debian 架构名。不认识的架构**明确报错**：瞎猜一个装上去就是坏包。
case "$TARGET" in
    x86_64-* | amd64-*) DEB_ARCH="amd64" ;;
    aarch64-*) DEB_ARCH="arm64" ;;
    armv7*-eabihf) DEB_ARCH="armhf" ;;
    *)
        echo "--format deb: 不认识的目标三元组 $TARGET（只认 x86_64→amd64 / aarch64→arm64 / armv7*-eabihf→armhf）" >&2
        exit 2
        ;;
esac

PAYLOAD="$STAGE/payload"
CONTROL="$STAGE/control"

# 二进制落 /usr/local/bin——系统单元的 ExecStart 就是它（§2.2）。
install -Dm755 "$BIN" "$PAYLOAD/usr/local/bin/$PACKAGE"
# 两份 unit：系统单元照 Debian 的约定落 /usr/lib/systemd/system（merged-usr 后 /lib 是它的软链），
# 用户单元落 /usr/lib/systemd/user。装的都是**仓库里那两份原文件**，不复制改。
install -Dm644 "$CRATE_DIR/systemd/system/vox-headless.service" "$PAYLOAD/usr/lib/systemd/system/vox-headless.service"
install -Dm644 "$CRATE_DIR/systemd/user/vox-headless.service" "$PAYLOAD/usr/lib/systemd/user/vox-headless.service"
# 样例配置与 README：安装后不在 PATH 里，放 /usr/share 这一档（doc 放 doc）。
install -Dm644 "$CRATE_DIR/settings.example.json" "$PAYLOAD/usr/share/$PACKAGE/settings.example.json"
install -Dm644 "$CRATE_DIR/README.md" "$PAYLOAD/usr/share/doc/$PACKAGE/README.md"

# copyright（Debian 政策要的一份；MIT 全文，license 来自 workspace 的 `license = "MIT"`）。
mkdir -p "$PAYLOAD/usr/share/doc/$PACKAGE"
cat >"$PAYLOAD/usr/share/doc/$PACKAGE/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: vox-headless
Upstream-Contact: w23x0@users.noreply.github.com
Source: https://github.com/w23x0/Vox

Files: *
Copyright: 2026 Vox Contributors
License: MIT
 Permission is hereby granted, free of charge, to any person obtaining a copy
 of this software and associated documentation files (the "Software"), to deal
 in the Software without restriction, including without limitation the rights
 to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 copies of the Software, and to permit persons to whom the Software is
 furnished to do so, subject to the following conditions:
 .
 The above copyright notice and this permission notice shall be included in all
 copies or substantial portions of the Software.
 .
 THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 SOFTWARE.
EOF
# umask 0002 的机器上 `cat >` 建出来是 0664——doc 一律 0644（Debian 政策）。
chmod 0644 "$PAYLOAD/usr/share/doc/$PACKAGE/copyright"

# Depends：默认让 dpkg-shlibdeps 从二进制的 ELF NEEDED 现解（库表手抄必腐）。
# 交叉（target ≠ host）时它解不了目标架构的库，必须 --depends 显式给。
if [ -n "$DEPENDS_OVERRIDE" ]; then
    DEPENDS="$DEPENDS_OVERRIDE"
elif [ "$TARGET" != "$HOST_TARGET" ]; then
    {
        echo "--format deb --target $TARGET（交叉，host=$HOST_TARGET）：dpkg-shlibdeps 解不了" >&2
        echo "   目标架构的库，请显式 --depends，例如：" >&2
        echo "   --depends 'libc6 (>= 2.36), libasound2 (>= 1.2.6), libgcc-s1 (>= 12), adduser'" >&2
        echo "   （Ubuntu 24.04 上 ALSA 库叫 libasound2t64，Debian / RPi OS 叫 libasound2）" >&2
    } >&2
    exit 2
else
    # dpkg-shlibdeps 要在**有 debian/control 的目录**里跑（它从那儿读包名/架构）。
    # 这个目录单独建、不进 payload——data.tar 里多一个 debian/ 就是脏数据。
    mkdir -p "$STAGE/shlibdeps/debian"
    printf 'Source: vox\n\nPackage: %s\nArchitecture: any\n' "$PACKAGE" >"$STAGE/shlibdeps/debian/control"
    SHLIBS_DEP="$(cd "$STAGE/shlibdeps" && dpkg-shlibdeps -O "$PAYLOAD/usr/local/bin/$PACKAGE" | sed -n 's/^shlibs:Depends=//p')"
    [ -n "$SHLIBS_DEP" ] || { echo "dpkg-shlibdeps 没解出依赖（二进制编译产物不对？）" >&2; exit 1; }
    # adduser：postinst 要建系统服务用户 vox（系统单元的 User=vox）。
    DEPENDS="$SHLIBS_DEP, adduser"
fi

mkdir -p "$CONTROL"
{
    printf 'Package: %s\n' "$PACKAGE"
    printf 'Version: %s\n' "$VERSION"
    printf 'Section: sound\n'
    printf 'Priority: optional\n'
    printf 'Architecture: %s\n' "$DEB_ARCH"
    printf 'Maintainer: Vox Contributors <w23x0@users.noreply.github.com>\n'
    printf 'Homepage: https://github.com/w23x0/Vox\n'
    printf 'Depends: %s\n' "$DEPENDS"
    printf 'Description: Vox headless appliance entry (real-time speech translation)\n'
    printf ' 无屏档（小主板 Linux）入口：芯 + ALSA + 配置进/状态出 + 控制面，零 Tauri。\n'
    printf ' 对外说话与听人说话两条流水线照常装配；没有屏幕，字幕退化成日志与 MCP 订阅。\n'
    printf ' 装上启用 system 单元但不启动：把 /var/lib/vox/settings.json 填好再\n'
    printf ' systemctl enable --now vox-headless。用户单元（推荐档）见 /usr/share/doc/vox-headless/README.md。\n'
    printf 'Installed-Size: %s\n' "$(du -sk "$PAYLOAD" | cut -f1)"
} >"$CONTROL/control"

# md5sums：dpkg 校验用。路径不带 ./，排序固定（内容一样字节就一样）。
(cd "$PAYLOAD" && find . -type f -exec md5sum {} + | sed 's| \./| |' | LC_ALL=C sort -k2) >"$CONTROL/md5sums"

# postinst：建服务用户 + 把样例配置落到系统单元的 --config 路径 + 启用（**不启动**）单元。
# 处处 command -v 守卫：容器/极简系统上没有 adduser / systemctl 也不该让安装失败——
# 那些环境里用户本来就会自己管 unit，而二进制和样例文件已经装好了。
cat >"$CONTROL/postinst" <<'EOF'
#!/bin/sh
set -e
[ "$1" = configure ] || [ "$1" = triggered ] || exit 0

# 系统单元的 User=vox（见 /usr/lib/systemd/system/vox-headless.service 文件头）。
if ! getent passwd vox >/dev/null 2>&1; then
    if command -v adduser >/dev/null 2>&1; then
        adduser --system --group --home /var/lib/vox --no-create-home --quiet vox || true
    fi
fi

# 配置目录（= --config 指的那一份）。StateDirectory= 是另一个名字（/var/lib/vox-headless），
# 不会把这个目录建出来，而服务要往里写 usage.json / secret.json / control.json。
install -d -m755 /var/lib/vox
if getent passwd vox >/dev/null 2>&1; then
    chown vox:vox /var/lib/vox 2>/dev/null || true
fi
# 样例配置：没有才放一份（已存在的是管理员的真配置，不碰）。
if [ ! -e /var/lib/vox/settings.json ]; then
    install -Dm640 /usr/share/vox-headless/settings.example.json /var/lib/vox/settings.json
    if getent passwd vox >/dev/null 2>&1; then
        chown vox:vox /var/lib/vox/settings.json 2>/dev/null || true
    fi
fi

# 启用 system 单元（**不启动**：没有密钥两条腿本来就只会空转，起不起来都是如实行为；
# 要立刻起：systemctl enable --now vox-headless）。
if command -v deb-systemd-helper >/dev/null 2>&1; then
    deb-systemd-helper enable vox-headless.service >/dev/null 2>&1 || true
fi
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

echo "vox-headless: enabled but not started. Fill /var/lib/vox/settings.json, then"
echo "vox-headless:   systemctl enable --now vox-headless"
exit 0
EOF

# prerm：remove 之前先停（dpkg 政策：文件被摘之前服务该停）。
cat >"$CONTROL/prerm" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    remove | deconfigure)
        if command -v systemctl >/dev/null 2>&1; then
            systemctl stop vox-headless.service >/dev/null 2>&1 || true
        fi
        ;;
esac
exit 0
EOF

# postrm：remove/purge 后摘掉 enable 状态并 daemon-reload。
# **不动 /var/lib/vox 下的数据**：settings.json / secret.json 里有管理员放的东西，
# purge 就删数据是 surprise，要清自己 rm（README §2.2 写了路径）。
cat >"$CONTROL/postrm" <<'EOF'
#!/bin/sh
set -e
case "$1" in
    remove | purge)
        if command -v deb-systemd-helper >/dev/null 2>&1; then
            deb-systemd-helper disable vox-headless.service >/dev/null 2>&1 || true
        fi
        if command -v systemctl >/dev/null 2>&1; then
            systemctl daemon-reload >/dev/null 2>&1 || true
        fi
        ;;
esac
exit 0
EOF
chmod 0755 "$CONTROL/postinst" "$CONTROL/prerm" "$CONTROL/postrm"

# ---- 组装：三个成员各自字节固定，ar 确定性模式收口 ------------------------------

DEB_FILE="$OUT_DIR/${PACKAGE}_${VERSION}_${DEB_ARCH}.deb"
mkdir -p "$OUT_DIR"
rm -f "$DEB_FILE"

printf '2.0\n' >"$STAGE/debian-binary"

# 两个 tarball 用与 tar.gz **同一套**钉死参数（排序/时间戳/属主/格式），随后 gzip -n
#（不要原作者名与时间戳）。ar rcD 的 D = deterministic：成员头里的 mtime/uid/gid/mode 归零。
# 结果：同一份源码 + 同一套工具链 → 同一个 deb 哈希。
tar --create --file "$STAGE/control.tar" \
    --directory "$CONTROL" \
    --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" \
    --owner=0 --group=0 --numeric-owner --format=gnu .
tar --create --file "$STAGE/data.tar" \
    --directory "$PAYLOAD" \
    --sort=name --mtime="@${SOURCE_DATE_EPOCH:-0}" \
    --owner=0 --group=0 --numeric-owner --format=gnu .
gzip -n9 -c "$STAGE/control.tar" >"$STAGE/control.tar.gz"
gzip -n9 -c "$STAGE/data.tar" >"$STAGE/data.tar.gz"

# ar 存成员名只取 basename（GNU ar 行为），路径可以给全；顺序固定：debian-binary 打头。
(cd "$STAGE" && ar rcD "$DEB_FILE" debian-binary control.tar.gz data.tar.gz)

echo
echo "==> 产物：$DEB_FILE"
echo "==> 大小：$(wc -c <"$DEB_FILE") 字节"
echo "==> sha256：$(sha256sum "$DEB_FILE" | cut -d' ' -f1)"
echo
echo "==> dpkg-deb -f（control 字段）"
dpkg-deb -f "$DEB_FILE"
echo
echo "==> dpkg-deb --contents（data 成员）"
dpkg-deb --contents "$DEB_FILE"
echo
echo "==> control 成员里的维护脚本"
tar tzf "$STAGE/control.tar.gz"
