//! token 用量统计。
//!
//! **只统计已用量**，不做价格表、不换算成钱、不算剩余、不查账户余额。
//! 模型每轮回包带 usage，累加进来即可。
//!
//! 持久化格式沿用旧版，按模型分桶：
//! ```json
//! { "qwen3.8-livetranslate-flash-realtime": { "total_tokens": 0, ... } }
//! ```
//! 在这个基础上多存了 `daily` / `monthly` 两个桶，用来显示"今日 / 本月"。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 一轮回包里的 usage。字段名跟服务端一致，缺的当 0。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl TurnUsage {
    /// 服务端有时只给分项不给总数，补上。
    pub fn resolved_total(&self) -> u64 {
        if self.total_tokens > 0 {
            self.total_tokens
        } else {
            self.input_tokens + self.output_tokens
        }
    }

    pub fn is_zero(&self) -> bool {
        self.input_tokens == 0 && self.output_tokens == 0 && self.total_tokens == 0
    }
}

/// 一个桶的累计量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageTotals {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// 累计了多少轮，用来算平均。
    #[serde(default)]
    pub turns: u64,
}

impl UsageTotals {
    fn add(&mut self, usage: &TurnUsage) {
        self.input_tokens += usage.input_tokens;
        self.output_tokens += usage.output_tokens;
        self.total_tokens += usage.resolved_total();
        self.turns += 1;
    }
}

/// 桶的滚动规则：键一变（跨天 / 跨月）就把这个桶清零重来，并记下新键。
fn roll(totals: &mut UsageTotals, key: &mut String, current: String) {
    if *key != current {
        *totals = UsageTotals::default();
        *key = current;
    }
}

/// 单个模型的用量：累计 + 今日 + 本月。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(flatten)]
    pub total: UsageTotals,
    /// 今日累计，跨天自动清零。
    #[serde(default)]
    pub daily: UsageTotals,
    /// 今日是哪天，格式 `YYYY-MM-DD`。
    #[serde(default)]
    pub daily_date: String,
    /// 本月累计，跨月自动清零。
    #[serde(default)]
    pub monthly: UsageTotals,
    /// 本月是哪个月，格式 `YYYY-MM`。
    #[serde(default)]
    pub monthly_month: String,
    /// 最后更新时间，Unix 秒。
    #[serde(default)]
    pub updated_at: u64,
}

/// 调用时刻的日期，由外壳传进来（内核不读系统时钟）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// Unix 秒。
    pub unix_secs: u64,
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

impl Stamp {
    fn date_key(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn month_key(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

/// 全部模型的用量账本。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UsageLedger {
    pub models: BTreeMap<String, ModelUsage>,
}

impl UsageLedger {
    pub fn from_json(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 记一轮用量。全零的 usage 直接忽略（有些回包不带 usage）。
    pub fn record(&mut self, model: &str, usage: &TurnUsage, at: Stamp) {
        if usage.is_zero() {
            return;
        }
        let entry = self.models.entry(model.to_string()).or_default();

        roll(&mut entry.daily, &mut entry.daily_date, at.date_key());
        roll(&mut entry.monthly, &mut entry.monthly_month, at.month_key());

        entry.total.add(usage);
        entry.daily.add(usage);
        entry.monthly.add(usage);
        entry.updated_at = at.unix_secs;
    }

    pub fn get(&self, model: &str) -> Option<&ModelUsage> {
        self.models.get(model)
    }

    /// 所有模型的累计总和，给概览用。
    pub fn grand_total(&self) -> UsageTotals {
        let mut sum = UsageTotals::default();
        for usage in self.models.values() {
            sum.input_tokens += usage.total.input_tokens;
            sum.output_tokens += usage.total.output_tokens;
            sum.total_tokens += usage.total.total_tokens;
            sum.turns += usage.total.turns;
        }
        sum
    }

    /// 清空全部计数（设置里那个「重置计数」）。
    pub fn reset(&mut self) {
        self.models.clear();
    }

    /// 只清一个模型的计数。
    pub fn reset_model(&mut self, model: &str) {
        self.models.remove(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &str = "qwen3.8-livetranslate-flash-realtime";

    fn stamp(day: u32) -> Stamp {
        Stamp {
            unix_secs: 1_700_000_000 + day as u64 * 86_400,
            year: 2026,
            month: 8,
            day,
        }
    }

    fn usage(input: u64, output: u64) -> TurnUsage {
        TurnUsage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
        }
    }

    #[test]
    fn total_is_derived_when_server_omits_it() {
        let u = TurnUsage {
            input_tokens: 30,
            output_tokens: 12,
            total_tokens: 0,
        };
        assert_eq!(u.resolved_total(), 42);
    }

    #[test]
    fn records_accumulate_across_buckets() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &usage(100, 50), stamp(5));
        ledger.record(MODEL, &usage(10, 5), stamp(5));

        let entry = ledger.get(MODEL).unwrap();
        assert_eq!(entry.total.total_tokens, 165);
        assert_eq!(entry.total.input_tokens, 110);
        assert_eq!(entry.total.output_tokens, 55);
        assert_eq!(entry.total.turns, 2);
        assert_eq!(entry.daily.total_tokens, 165);
        assert_eq!(entry.monthly.total_tokens, 165);
    }

    #[test]
    fn daily_bucket_resets_across_days_but_total_does_not() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &usage(100, 0), stamp(5));
        ledger.record(MODEL, &usage(7, 0), stamp(6));

        let entry = ledger.get(MODEL).unwrap();
        assert_eq!(entry.total.total_tokens, 107);
        assert_eq!(entry.daily.total_tokens, 7, "跨天今日要清零");
        assert_eq!(entry.monthly.total_tokens, 107, "同月月度不清");
        assert_eq!(entry.daily_date, "2026-08-06");
    }

    #[test]
    fn monthly_bucket_resets_across_months() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &usage(100, 0), stamp(5));
        ledger.record(
            MODEL,
            &usage(9, 0),
            Stamp {
                unix_secs: 1_800_000_000,
                year: 2026,
                month: 9,
                day: 1,
            },
        );
        let entry = ledger.get(MODEL).unwrap();
        assert_eq!(entry.total.total_tokens, 109);
        assert_eq!(entry.monthly.total_tokens, 9);
        assert_eq!(entry.monthly_month, "2026-09");
    }

    #[test]
    fn zero_usage_is_ignored() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &TurnUsage::default(), stamp(5));
        assert!(ledger.get(MODEL).is_none(), "空 usage 不该建桶");
    }

    #[test]
    fn models_are_tracked_separately_and_summed() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &usage(10, 1), stamp(5));
        ledger.record("legacy-model", &usage(20, 2), stamp(5));
        assert_eq!(ledger.grand_total().total_tokens, 33);
        assert_eq!(ledger.grand_total().turns, 2);

        ledger.reset_model(MODEL);
        assert!(ledger.get(MODEL).is_none());
        assert_eq!(ledger.grand_total().total_tokens, 22);

        ledger.reset();
        assert_eq!(ledger.grand_total().total_tokens, 0);
    }

    #[test]
    fn roundtrips_through_json_and_reads_old_shape() {
        let mut ledger = UsageLedger::default();
        ledger.record(MODEL, &usage(10, 1), stamp(5));
        let restored = UsageLedger::from_json(&ledger.to_json());
        assert_eq!(restored, ledger);

        // 旧版只有 total_tokens/input_tokens/output_tokens/updated_at，也要读得进来。
        let old = r#"{"m":{"total_tokens":5,"input_tokens":4,"output_tokens":1,"updated_at":123}}"#;
        let parsed = UsageLedger::from_json(old);
        let entry = parsed.get("m").unwrap();
        assert_eq!(entry.total.total_tokens, 5);
        assert_eq!(entry.updated_at, 123);
        assert_eq!(entry.daily.total_tokens, 0);
    }

    #[test]
    fn garbage_json_yields_empty_ledger() {
        assert_eq!(UsageLedger::from_json("???"), UsageLedger::default());
    }
}
