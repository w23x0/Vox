#!/usr/bin/env bash
#
# Vox 无屏档的打包脚本：出一个可复现的 tar.gz（二进制 + systemd unit + 样例配置 + README）。
#
# 为什么是 tar.gz 而不是 deb（EMBEDDED §3.8）：无屏盒子多半是刷进去的镜像，装的是个目录树，
# 不是包管理器；要装成服务就用里面那两份 unit（步骤见 README.md §2）。
#
# 可复现：tar 的排序/时间戳/属主都钉死（`--sort=name --mtime=@0 --owner=0 --group=0`），
# 二进制用 `--locked` 编（锁文件就是输入的一部分）。同一份源码 + 同一套工具链 → 同一个哈希。
#
# 用法：
#   tools/package-headless.sh                                    # 本机（推荐：在板上原生编译）
#   tools/package-headless.sh --target aarch64-unknown-linux-gnu # 交叉编译（要目标 sysroot + linker）
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

usage() {
    cat <<'EOF'
用法：tools/package-headless.sh [--target <三元组>] [--profile release|debug] [--out <目录>]

  --target    目标三元组；缺省 = 本机宿主（用 rustc -vV 的 host:）
  --profile   构建 profile：release（缺省，strip + lto）/ debug（冒烟用）
  --out       产物目录；缺省 <仓库>/tools/bundle（已 gitignore）
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

# 版本从 cargo 自己那儿取（不另立一份版本号）：`cargo pkgid` 的输出以 @ 或 # 接版本。
VERSION="$(cd "$REPO_ROOT" && cargo pkgid -p "$PACKAGE" | sed 's/.*[@#]//')"
[ -n "$VERSION" ] || { echo "取不到 $PACKAGE 的版本" >&2; exit 2; }

# 编。`--locked`：锁文件是输入的一部分，出包时不许悄悄改依赖（改了就报错，不改就是可复现）。
echo "==> cargo build --locked --profile $CARGO_PROFILE -p $PACKAGE --target $TARGET"
(cd "$REPO_ROOT" && cargo build --locked --profile "$CARGO_PROFILE" -p "$PACKAGE" --target "$TARGET")

BIN="$REPO_ROOT/target/$TARGET/$PROFILE_DIR/$PACKAGE"
[ -f "$BIN" ] || { echo "没找到刚编出来的二进制：$BIN" >&2; exit 1; }

# 归档内的顶层目录：解包不会把文件撒到当前目录里。
NAME="$PACKAGE-$VERSION-$TARGET"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

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
