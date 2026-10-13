#!/usr/bin/env bash
#
# Vox 的一屏审计：这个项目现在“几分”。（2026-10-12 建，借鉴 conformance 工作流——
# 规则表 + 可解析分数 + 退步检测；出处与取舍见 docs/architecture/ISSUES.md 头部。）
#
# 收什么（全部机器判定，不靠人判）：
#   规则 = 差分金样场景：golden.rs::SCENARIOS 每条一个 `*_matches_its_golden` 测试、
#         一份 `golden/<场景>.txt` 期望输出，cargo test 逐字节差分。挂一条列一条名。
#   门禁 = 仓库既有四件套：fmt / clippy / workspace test / app-ui npm verify。
#   历史 = 上次 GREEN 的读数存 tools/.audit-log/baseline.txt；掉了分数或红线变红 → regress。
#         baseline 只在 GREEN 时更新（红的时候留着当“高点”，下一轮继续比）。
#         验证 regress 检出本身：跑一次 GREEN 后加 --no-baseline 再跑，regress 会列假高分；
#         那只是为了证明检测器活着，正常使用永远只跑 `tools/audit.sh`。
#
# 不收什么（如实说明，不造假分数）：
#   AEC W4 / 局域网控制面 / S3 后半 NetIn（NetIn/NetOut 恒假）——
#   这些的验收标准含真板子、网络行为，脚本判不了。判不了还列就是假分数。
#   它们的人工台账：docs/architecture/DIRECTIONS.md §10.9“仍未落地”。
#   产品方向待拍板：docs/architecture/DECISIONS.md B 区（现存 11 条）。
#   施工期规约矛盾：docs/architecture/ISSUES.md。
#
# 用法：
#   tools/audit.sh              # 全量（含 npm verify）
#   tools/audit.sh --fast        # 跳过 npm verify（没改前端时省时间）
#   tools/audit.sh --no-baseline # 只读旧基线、不更新（验证 regress 检测用）
#
# 退出码：0 = GREEN，1 = RED（有 fail 或退步），2 = 脚本环境坏（工具缺失等）。
# 详细输出（cargo/npm 原文）在 tools/.audit-log/last.log。
set -euo pipefail
export LC_ALL=C
export TZ=UTC

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
GOLDEN_DIR="$REPO_ROOT/crates/vox-core/src/pipeline/golden"
LOG_DIR="$REPO_ROOT/tools/.audit-log"
BASELINE="$LOG_DIR/baseline.txt"
mkdir -p "$LOG_DIR"
: >"$LOG_DIR/last.log"

FAST=0
WRITE_BASELINE=1
for arg in "$@"; do
  case "$arg" in
    --fast)        FAST=1 ;;
    --no-baseline) WRITE_BASELINE=0 ;;
    *) echo "audit.sh: 未知参数: $arg（可选：--fast / --no-baseline）" >&2; exit 2 ;;
  esac
done

# set -e 下，cargo/grep“失败”是我们判定的输入，不是脚本错误：一律显式接住。
FAIL_IDS=()   # 本轮红线：哪项判定失败
REGRESS=()    # 本轮退步：与上次 GREEN 比掉了什么

# ---- 规则：差分金样 ----------------------------------------------------------

# 场景名单从测试二进制拿（golden.rs::SCENARIOS 为每条场景生成一个测试函数）。
# 不用 grep 源码数 SCENARIOS：文件经 rustfmt，跨行条目 `(\n "name",` grep 不到——
# 之前踩过，`^    \("` 只会数到单行条目。
scen=""
if list_out=$(cargo test -p vox-core -- --list 2>>"$LOG_DIR/last.log"); then
  scen=$(printf '%s\n' "$list_out" | sed -n 's/^.*::\(.*\)_matches_its_golden: test$/\1/p' | LC_ALL=C sort)
else
  FAIL_IDS+=("golden_list(编译不过，见 last.log)")
fi
golden_rules=$(printf '%s\n' "$scen" | grep -c . || true)

# txt 名单：期望输出文件。少一个 = 有规则没凭证；多一个 = 孤儿金样。
shopt -s nullglob
txt=()
for f in "$GOLDEN_DIR"/*.txt; do txt+=("$(basename "$f" .txt)"); done
shopt -u nullglob
golden_files=${#txt[@]}

missing_txt=$(comm -23 <(printf '%s\n' "$scen") <(printf '%s\n' "${txt[@]:-}" | LC_ALL=C sort) | grep -v '^$' || true)
orphan_txt=$(comm -13 <(printf '%s\n' "$scen") <(printf '%s\n' "${txt[@]:-}" | LC_ALL=C sort) | grep -v '^$' || true)
[ -n "$missing_txt" ] && FAIL_IDS+=("golden_missing_txt: $(printf '%s' "$missing_txt" | tr '\n' ' ')")
[ -n "$orphan_txt" ]  && FAIL_IDS+=("golden_orphan_txt: $(printf '%s' "$orphan_txt" | tr '\n' ' ')")

# 跑规则。失败的测试名直接列（比“N failed”有用）。deterministic 那条元测试
# （goldens_are_deterministic_when_regenerated）会一并出现在 passed 里，不算规则分。
golden_failed_ids=()
if golden_out=$(cargo test -p vox-core golden 2>>"$LOG_DIR/last.log"); then
  :
else
  mapfile -t golden_failed_ids < <(printf '%s\n' "$golden_out" | sed -n 's/^test \(.*\) \.\.\. FAILED/\1/p' | LC_ALL=C sort -u)
  FAIL_IDS+=("golden: $(printf '%s' "${golden_failed_ids[*]:-}" | tr ' ' ',')")
fi
golden_failed=${#golden_failed_ids[@]}

# ---- 门禁：既有四件套 --------------------------------------------------------

if cargo fmt --all -- --check >>"$LOG_DIR/last.log" 2>&1; then
  fmt_state=clean
else
  fmt_state=dirty
  FAIL_IDS+=(fmt)
fi

# -D warnings：clippy 的 warning 就是失败。退出码即红绿灯，不数 warning 条数。
if cargo clippy --workspace --all-targets -- -D warnings >>"$LOG_DIR/last.log" 2>&1; then
  clippy_state=clean
else
  clippy_state=FAILED
  FAIL_IDS+=(clippy)
fi

if ws_out=$(cargo test --workspace 2>>"$LOG_DIR/last.log"); then
  # awk 的 END 不带 \n：read 到 EOF 会返回非零，set -e 会在这里误杀（第一轮跑通过）。
  # `|| true` 兜住；赋值本身已完成，值就是我们要的。
  read -r ws_pass ws_fail ws_ign < <(printf '%s\n' "$ws_out" | awk '
    /test result:/ { for (i = 1; i <= NF; i++) {
        if ($i == "passed;")  p += $(i-1);
        if ($i == "failed;")  f += $(i-1);
        if ($i == "ignored;") g += $(i-1) } }
    END { printf "%d %d %d\n", p+0, f+0, g+0 }') || true
  [ "${ws_fail:-1}" != "0" ] && FAIL_IDS+=("workspace_test")
else
  ws_pass=0; ws_fail=1; ws_ign=0
  FAIL_IDS+=("workspace_test(编译不过，见 last.log)")
fi

if [ "$FAST" = 1 ]; then
  ui_state="skipped(--fast)"
elif (cd "$REPO_ROOT/app/ui" && npm run verify >>"$LOG_DIR/last.log" 2>&1); then
  ui_state=ok
else
  ui_state=FAILED
  FAIL_IDS+=(ui_verify)
fi

# ---- 退步检测 ----------------------------------------------------------------

if [ -f "$BASELINE" ]; then
  b_rules=$(awk '$1=="golden_rules"{print $2}' "$BASELINE")
  b_files=$(awk '$1=="golden_files"{print $2}' "$BASELINE")
  b_gfail=$(awk '$1=="golden_failed"{print $2}' "$BASELINE")
  b_wpass=$(awk '$1=="workspace_passed"{print $2}' "$BASELINE")
  b_wfail=$(awk '$1=="workspace_failed"{print $2}' "$BASELINE")
  b_gate=$(awk '$1=="gate_ok"{print $2}' "$BASELINE")
  [ "$golden_rules" -lt "$b_rules" ] && REGRESS+=("rules $b_rules→$golden_rules")
  [ "$golden_files"  -lt "$b_files"  ] && REGRESS+=("golden_files $b_files→$golden_files")
  [ "$golden_failed" -gt "$b_gfail" ] && REGRESS+=("golden_failed $b_gfail→$golden_failed")
  [ "$ws_pass"      -lt "$b_wpass"  ] && REGRESS+=("workspace_passed $b_wpass→$ws_pass")
  [ "$ws_fail"      -gt "$b_wfail"  ] && REGRESS+=("workspace_failed $b_wfail→$ws_fail")
  [ "$b_gate" = "1" ] && [ "$fmt_state" != clean -o "$clippy_state" != clean -o "$ui_state" = FAILED ] \
    && REGRESS+=("gate：上次全绿，本轮 fmt=$fmt_state clippy=$clippy_state ui=$ui_state")
else
  b_rules=0; b_files=0; b_gfail=0; b_wpass=0; b_wfail=0; b_gate=0
fi

# ---- 判定 --------------------------------------------------------------------

gate_ok=0
[ "$fmt_state" = clean ] && [ "$clippy_state" = clean ] && [ "$ui_state" != FAILED ] && gate_ok=1

verdict=GREEN
[ "$golden_rules" -gt 0 ] || verdict=RED
[ "$golden_files" -eq "$golden_rules" ] || verdict=RED
[ "$golden_failed" -eq 0 ] || verdict=RED
[ "${#FAIL_IDS[@]}" -gt 0 ] && verdict=RED
[ "${#REGRESS[@]}" -gt 0 ] && verdict=RED
[ "$gate_ok" = 1 ] || verdict=RED
[ "$FAST" = 1 ] && [ "$ui_state" = "skipped(--fast)" ] && true  # --fast 允许 GREEN，上面已放行

# ---- 输出（短、确定、可解析） --------------------------------------------------

{
  echo "== vox audit =="
  echo "golden_rules:   $golden_rules            # golden.rs::SCENARIOS → *_matches_its_golden 测试数"
  if [ "$golden_failed" -eq 0 ]; then
    echo "golden_status:  $((golden_rules - golden_failed))/$golden_rules pass"
  else
    echo "golden_status:  $((golden_rules - golden_failed))/$golden_rules pass（挂: ${golden_failed_ids[*]}）"
  fi
  echo "golden_files:   $golden_files            # golden/*.txt 期望输出数，应等于 rules"
  echo "fmt:            $fmt_state"
  echo "clippy:         $clippy_state"
  echo "workspace_test: $ws_pass passed / $ws_fail failed / $ws_ign ignored"
  echo "ui_verify:      $ui_state"
  if [ "${#FAIL_IDS[@]}" -gt 0 ]; then
    echo "fail:           [$(printf '%s, ' "${FAIL_IDS[@]}")]"
  else
    echo "fail:           []"
  fi
  if [ "${#REGRESS[@]}" -gt 0 ]; then
    echo "regress:        [$(printf '%s, ' "${REGRESS[@]}")]"
  elif [ ! -f "$BASELINE" ]; then
    echo "regress:        (no baseline)"
  else
    echo "regress:        []"
  fi
  echo "verdict:        $verdict"
  echo "note: 不可机器判定的未落地项不在此表：AEC W4（真板子）/ 局域网控制面 / S3 后半 NetIn（NetIn、NetOut 恒假）"
  echo "note: 人工台账：DIRECTIONS.md §10.9“仍未落地” ｜ DECISIONS.md B 区（11 条） ｜ architecture/ISSUES.md"
} | tee "$LOG_DIR/summary.txt"
echo "log: $LOG_DIR/last.log"

# GREEN 才更新基线：红的时候留着“高点”，下一轮继续比。
if [ "$verdict" = GREEN ] && [ "$WRITE_BASELINE" = 1 ]; then
  {
    printf 'golden_rules %d\n'   "$golden_rules"
    printf 'golden_files %d\n'   "$golden_files"
    printf 'golden_failed %d\n'  "$golden_failed"
    printf 'workspace_passed %d\n' "$ws_pass"
    printf 'workspace_failed %d\n' "$ws_fail"
    printf 'gate_ok %d\n'        "$gate_ok"
  } >"$BASELINE"
fi

[ "$verdict" = GREEN ] || exit 1
