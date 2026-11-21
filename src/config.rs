//! 勤務シフト自動生成システム 設定モジュール (`config.rs`)
//!
//! Version: v2.0.0
//! Updated: 2026-11-21 - v2.0.0 厳格探索＋緩和選択モデルへの刷新（Tier 2重み逆転解消・未使用定数削除・RelaxationKind/Proposal追加・診断用重み整理）
//! - v1.3.5 (2026-11-17): 特別枠（堀さん：必要に応じて週5）の超過出勤ペナルティ引き上げ (150k -> 10M)
//! - v1.3.1 (2026-11-10): Stage 2 所定勤務日数防衛重み (WEIGHT_TARGET_DAYS_SLACK=25M) および連勤上限緊急緩和重み (WEIGHT_CONSEC_WORK_SLACK=28M) 追加
//! - v1.3.0 (2026-11-08): 2階裏職種バランス・Ope推奨・石橋さん水曜公休・早上がり重複回避＆フルタイム増員ペナルティ定数追加
//! - v1.2.0 (2026-11-01): 全15設定項目・全固定ペナルティ重みの完全連動 (HIGH1推奨重み 10.2M 微調整で一般セクションブレ幅スラック 10M より上位を確定)
//! - v1.1.0 (2026-10-31): HIGH1推奨人数ペナルティ引き上げおよび主公休スライド許容スラック重み追加
//! - v1.0.0 (2026-10-31): Phase 1 初期実装: 10段階制約重み、固定ペナルティ、ShiftConfig (全15項目)
//!
//! SPECIFICATION.md セクション2 に基づく定数・ペナルティ重み・動的設定の完全実装。

use serde::{Deserialize, Serialize};

// ============================================================================
// 2.1 10段階制約レベル重み定数
// ============================================================================
pub const WEIGHT_LEVEL_HARD1: i64 = 20_000_000;
pub const WEIGHT_LEVEL_HARD2: i64 = 16_000_000;
pub const WEIGHT_LEVEL_HARD3: i64 = 14_000_000;
pub const WEIGHT_LEVEL_HIGH1: i64 = 12_000_000;
pub const WEIGHT_LEVEL_HIGH2: i64 = 9_000_000;
pub const WEIGHT_LEVEL_HIGH3: i64 = 6_000_000;
pub const WEIGHT_LEVEL_SOFT1: i64 = 400_000;
pub const WEIGHT_LEVEL_SOFT2: i64 = 100_000;
pub const WEIGHT_LEVEL_SOFT3: i64 = 50_000;
pub const WEIGHT_LEVEL_LOW: i64 = 20_000;

/// 制約レベル名から制約重みを取得（未定義キーのデフォルトは `SOFT2`: 100_000）
pub fn get_level_weight(level: &str) -> i64 {
    let normalized = level.trim().to_uppercase();
    match normalized.as_str() {
        "HARD1" => WEIGHT_LEVEL_HARD1,
        "HARD2" | "HARD" => WEIGHT_LEVEL_HARD2,
        "HARD3" => WEIGHT_LEVEL_HARD3,
        "HIGH1" => WEIGHT_LEVEL_HIGH1,
        "HIGH2" | "HIGH" => WEIGHT_LEVEL_HIGH2,
        "HIGH3" => WEIGHT_LEVEL_HIGH3,
        "SOFT1" => WEIGHT_LEVEL_SOFT1,
        "SOFT2" | "SOFT" => WEIGHT_LEVEL_SOFT2,
        "SOFT3" => WEIGHT_LEVEL_SOFT3,
        "LOW" | "LOW1" => WEIGHT_LEVEL_LOW,
        _ => {
            if normalized.contains("HARD") {
                WEIGHT_LEVEL_HARD2
            } else if normalized.contains("HIGH") {
                WEIGHT_LEVEL_HIGH2
            } else if normalized.contains("LOW") {
                WEIGHT_LEVEL_LOW
            } else {
                WEIGHT_LEVEL_SOFT2
            }
        }
    }
}

// ============================================================================
// 2.2 推奨人数 (val_rec) 不足ペナルティ重み
// ============================================================================
pub const REC_WEIGHT_HARD1: i64 = 12_000_000;
pub const REC_WEIGHT_HARD2: i64 = 11_000_000;
pub const REC_WEIGHT_HARD3: i64 = 10_500_000;
pub const REC_WEIGHT_HIGH1: i64 = 10_200_000;
pub const REC_WEIGHT_HIGH2: i64 = 4_000_000;
pub const REC_WEIGHT_HIGH3: i64 = 2_000_000;
pub const REC_WEIGHT_SOFT1: i64 = 400_000;
pub const REC_WEIGHT_SOFT2: i64 = 200_000;
pub const REC_WEIGHT_SOFT3: i64 = 100_000;
pub const REC_WEIGHT_LOW: i64 = 50_000;

/// 制約レベル名から推奨人数不足ペナルティ重みを取得（デフォルト `SOFT2`: 200_000）
pub fn get_rec_weight(level: &str) -> i64 {
    let normalized = level.trim().to_uppercase();
    match normalized.as_str() {
        "HARD1" => REC_WEIGHT_HARD1,
        "HARD2" | "HARD" => REC_WEIGHT_HARD2,
        "HARD3" => REC_WEIGHT_HARD3,
        "HIGH1" => REC_WEIGHT_HIGH1,
        "HIGH2" | "HIGH" => REC_WEIGHT_HIGH2,
        "HIGH3" => REC_WEIGHT_HIGH3,
        "SOFT1" => REC_WEIGHT_SOFT1,
        "SOFT2" | "SOFT" => REC_WEIGHT_SOFT2,
        "SOFT3" => REC_WEIGHT_SOFT3,
        "LOW" | "LOW1" => REC_WEIGHT_LOW,
        _ => {
            if normalized.contains("HARD") {
                REC_WEIGHT_HARD2
            } else if normalized.contains("HIGH") {
                REC_WEIGHT_HIGH2
            } else if normalized.contains("LOW") {
                REC_WEIGHT_LOW
            } else {
                REC_WEIGHT_SOFT2
            }
        }
    }
}

// ============================================================================
// 2.3 システム固定ペナルティ定数（Tier 2 品質最適化 & 承認緩和最小化）
// ============================================================================
/// 承認済み緩和が複数日・週にまたがる場合に必要最小限の使用へ抑える内部ペナルティ
pub const WEIGHT_APPROVED_RELAX_PENALTY: i64 = 50_000_000;
/// 承認時/非常時: 条件付水曜公休（非Ope・非注射週）を緩和して水曜出勤させた場合のペナルティ
pub const WEIGHT_ISHIBASHI_WED_OFF_SLACK: i64 = 14_000_000;
/// 承認時/非常時: 「必要に応じて週5」スタッフの所定超過出勤1日あたりペナルティ
pub const WEIGHT_SPECIAL_EXTRA_WORK: i64 = 10_000_000;
/// 一般セクション（1階・2階表）の許容ブレ幅逸脱ペナルティ
pub const WEIGHT_GENERAL_SEC_SLACK: i64 = 10_000_000;
/// 週4日×固定1日スタッフの第2公休曜日スライド抑制ペナルティ
pub const WEIGHT_SECOND_OFF_SLIDE_SLACK: i64 = 8_500_000;
/// ① Ope日の2階裏推奨体制（Ope専従者以外に看護師2人＋検査2人＝計4人）不足ペナルティ（4名枠最優先）
pub const WEIGHT_OPE_2F_BACK_REC_PENALTY: i64 = 5_500_000;
/// ② 非Ope日に早上がりスタッフが重複した際のフルタイム増員（+1名）不足ペナルティ
pub const WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK: i64 = 3_500_000;
/// ③ 2階裏 通常週（非祝6稼働日）の4名体制最低日数不足ペナルティ
pub const WEIGHT_2F_BACK_WEEKLY_4_SLACK: i64 = 2_500_000;
/// 2階裏 5名出勤ペナルティ
pub const WEIGHT_2F_BACK_5_PENALTY: i64 = 2_000_000;
/// OR型スタッフの週次主公休曜日スライド許容ペナルティ（HIGH1/HIGH2推奨人数達成時に一時移動を許容）
pub const WEIGHT_MAIN_OFF_SLIDE_SLACK: i64 = 2_000_000;
/// 1階 `M_s >= 6.5` のとき 6名以下となるペナルティ
pub const WEIGHT_1F_UNDER_7_PENALTY: i64 = 1_500_000;
/// 祝日週（平日祝日を含む5診療日週）における公休優先配分ソフト重み（所定日数は割り込まない）
pub const WEIGHT_HOLIDAY_WEEK_OFF_PREF: i64 = 600_000;
/// 一般セクション（1階・2階表）人数平準化（2乗項）
pub const WEIGHT_SEC_BALANCE_SQ: i64 = 500_000;
/// 一般セクション（1階・2階表）人数平準化（平均 `round(M_s)` からの絶対誤差）
pub const WEIGHT_SEC_BALANCE_ABS: i64 = 500_000;
/// 非Ope日に早上がりスタッフが同時出勤する重複回避ペナルティ（ソフト制約）
pub const WEIGHT_EARLY_OVERLAP_PENALTY: i64 = 400_000;
/// 主公休曜日以外の平日に「公」が入る不一致ペナルティ
pub const WEIGHT_MAIN_OFF_MISMATCH: i64 = 300_000;
/// カレンダー週（ISO週）単位の公休日数平準化ペナルティ
pub const WEIGHT_WEEKLY_OFF_DIFF: i64 = 300_000;
/// 前月主公休継続ペナルティ基準値
pub const WEIGHT_PREV_OFF_REPEAT: i64 = 300_000;
/// 2階裏 非Ope日の看護師が推奨人数未満の日のペナルティ
pub const WEIGHT_NURSE_LT2_PENALTY: i64 = 200_000;
/// 7日間ローリング週休平準化ペナルティ
pub const WEIGHT_ROLLING_WINDOW_DIFF: i64 = 80_000;
/// 2階裏 軽い平準化ペナルティ（3〜4名枠を奪わないよう抑制）
pub const WEIGHT_2F_BACK_SOFT_BALANCE: i64 = 50_000;
/// 月末3稼働日の公休偏り抑制ペナルティ
pub const WEIGHT_TAIL_OFF_SUM: i64 = 30_000;

// ============================================================================
// 2.3b 診断専用 MILP スラック重み定数（Tier 0 は絶対ロック・Tier 1 最小緩和探索用）
// ============================================================================
/// 最終手段：所定勤務日数 ±1日調整スラック重み
pub const DIAG_WEIGHT_TARGET_DAYS_ADJUST: i64 = 200_000_000;
/// Ope専従スタッフの Ope/注射日必須出勤緩和スラック重み
pub const DIAG_WEIGHT_OPE_DEDICATED: i64 = 90_000_000;
/// 固定公休曜日の振替/解除スラック重み
pub const DIAG_WEIGHT_FIXED_OFF_SWAP: i64 = 60_000_000;
/// 2階裏 日次総人数最低不足スラック重み
pub const DIAG_WEIGHT_2F_BACK_MIN: i64 = 45_000_000;
/// 2階裏 職種別（看護師・検査）最低不足スラック重み
pub const DIAG_WEIGHT_2F_BACK_ROLE: i64 = 40_000_000;
/// 週4祝日非連動スタッフの週次目標柔軟化スラック重み
pub const DIAG_WEIGHT_WEEKLY_FLEX: i64 = 25_000_000;
/// 非Ope・非注射週における条件付水曜公休の振替スラック重み
pub const DIAG_WEIGHT_CONDITIONAL_WED_OFF: i64 = 20_000_000;
/// 特別枠（必要に応じて週5）の超過出勤スラック重み
pub const DIAG_WEIGHT_SPECIAL_EXTRA_WORK: i64 = 15_000_000;

// ============================================================================
// 2.3c 承認制緩和モデル定義 (RelaxationScope / RelaxationKind / RelaxationProposal)
// ============================================================================
/// 緩和適用スコープ
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelaxationScope {
    /// 特定の日（1..=days_in_month）のみ
    Day(u32),
    /// 特定のISOカレンダー週（week_index）のみ
    Week(usize),
    /// 当月全体
    Month,
}

impl RelaxationScope {
    /// 指定日・週がこのスコープに含まれるか判定
    pub fn matches(&self, day: u32, week_index: usize) -> bool {
        match *self {
            RelaxationScope::Day(d) => d == day,
            RelaxationScope::Week(w) => w == week_index,
            RelaxationScope::Month => true,
        }
    }
}

/// ユーザーが承認して適用する緩和条件の種類
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RelaxationKind {
    /// 「必要に応じて週5」特別枠スタッフの週5出勤許可
    SpecialExtraWork {
        staff_name: String,
        scope: RelaxationScope,
    },
    /// 条件付水曜公休（非Ope・非注射週）の他曜日振替許可
    ConditionalWedOff {
        staff_name: String,
        scope: RelaxationScope,
    },
    /// 2階裏（外来）日次総人数最低ラインの引き下げ許容
    Sec2fBackMinTotal {
        scope: RelaxationScope,
        relaxed_min: i32,
    },
    /// 2階裏（外来）看護師最低ラインの引き下げ許容
    Sec2fBackMinNurse {
        scope: RelaxationScope,
        relaxed_min: i32,
    },
    /// 2階裏（外来）検査スタッフ最低ラインの引き下げ許容
    Sec2fBackMinExam {
        scope: RelaxationScope,
        relaxed_min: i32,
    },
    /// Ope専従スタッフのOpe・注射日必須出勤の免除許容
    OpeDedicatedWork {
        staff_name: String,
        scope: RelaxationScope,
    },
    /// 固定公休曜日の振替/出勤許可
    FixedOffDaySwap {
        staff_name: String,
        scope: RelaxationScope,
    },
    /// 週4祝日非連動スタッフの特定週出勤日数目標の柔軟化（±1日）
    WeeklyTargetFlex {
        staff_name: String,
        week_index: usize,
    },
    /// シフト制約条件シートの HARD ルール緩和
    SheetHardRule {
        rule_index: usize,
        rule_label: String,
        scope: RelaxationScope,
    },
    /// 最終手段：月間所定勤務日数の ±1日調整（delta<0: 減算許容, delta>0: 増加許容, delta==0: ±1許容）
    TargetDaysAdjust {
        staff_name: Option<String>,
        delta: i32,
    },
}

/// ユーザーに提示する緩和候補アイテム
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaxationProposal {
    pub kind: RelaxationKind,
    /// GUI/CLIで表示する分かりやすい項目名
    pub label: String,
    /// この緩和を選んだ場合の影響説明
    pub impact_desc: String,
    /// システムが最小緩和として推奨する候補か
    pub recommended: bool,
    /// 最終手段（所定勤務日数±1日調整等）か
    pub is_last_resort: bool,
}

/// 3連休抑制レベル重みマップ
pub fn get_consecutive_off_weight(level: &str) -> i64 {
    match level.trim() {
        "強" => 300_000,
        "中" => 100_000,
        "弱" => 30_000,
        "なし" | "無" | "オフ" | "off" => 0,
        _ => 100_000,
    }
}

/// 既存シフト維持レベル重みマップ
pub fn get_existing_shift_weight(level: &str) -> i64 {
    match level.trim() {
        "強" => 1_000_000,
        "中" => 300_000,
        "弱" => 50_000,
        _ => 300_000,
    }
}

/// ソルバー制限時間（秒）
pub const SOLVER_TIME_LIMIT_SECS: f64 = 120.0;

// ============================================================================
// 2.4 動的設定構造体 (ShiftConfig 全15項目)
// ============================================================================
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShiftConfig {
    /// 1. 2階裏 基本人数 (初期値: 3)
    pub sec_2f_back_base_cap: i32,
    /// 2. 2階裏 看護師最低人数 (初期値: 1)
    pub nurse_min_count: i32,
    /// 3. 2階裏 看護師推奨人数 (初期値: 2)
    pub nurse_rec_count: i32,
    /// 4. 連勤上限日数 (初期値: 5)
    pub max_consecutive_work_days: usize,
    /// 5. 固定公休 削減許容日数 (初期値: 2 ※v2.0.0では旧Excel互換読込のみ保持。所定割れ黙認には未使用)
    pub fixed_off_shortage_allowance: i32,
    /// 6. 3連休抑制レベル (初期値: "中")
    pub consecutive_off_level: String,
    /// 7. セクション人数ブレ幅 (初期値: 1)
    pub sec_capacity_tolerance: i32,
    /// 8. 週休配分目標 (初期値: 2)
    pub weekly_off_target: i32,
    /// 9. 既存シフト維持レベル (初期値: "中")
    pub existing_shift_weight_level: String,
    /// 10. 2階裏 警告表示人数 (初期値: 2)
    pub warn_2f_back_count: i32,
    /// 11. 祝日週 公休設定 (初期値: true)
    pub holiday_week_off_rule: bool,
    /// 12. 週単位 公休平準化 (初期値: true)
    pub weekly_off_balance_rule: bool,
    /// 13. 2階裏 週4名最低日数 (初期値: 2)
    pub sec_2f_back_weekly_4_min: i32,
    /// 14. 空欄スタッフ 公休固定ローテーション (初期値: true)
    pub auto_rotate_off_rule: bool,
    /// 15. 指定公休日 有効 (初期値: true)
    pub specified_off_rule: bool,
}

impl Default for ShiftConfig {
    fn default() -> Self {
        Self {
            sec_2f_back_base_cap: 3,
            nurse_min_count: 1,
            nurse_rec_count: 2,
            max_consecutive_work_days: 5,
            fixed_off_shortage_allowance: 2,
            consecutive_off_level: "中".to_string(),
            sec_capacity_tolerance: 1,
            weekly_off_target: 2,
            existing_shift_weight_level: "中".to_string(),
            warn_2f_back_count: 2,
            holiday_week_off_rule: true,
            weekly_off_balance_rule: true,
            sec_2f_back_weekly_4_min: 2,
            auto_rotate_off_rule: true,
            specified_off_rule: true,
        }
    }
}

impl ShiftConfig {
    /// 3連休抑制ペナルティ重みを取得
    pub fn consecutive_off_weight(&self) -> i64 {
        get_consecutive_off_weight(&self.consecutive_off_level)
    }

    /// 既存シフト維持ペナルティ重みを取得
    pub fn existing_shift_weight(&self) -> i64 {
        get_existing_shift_weight(&self.existing_shift_weight_level)
    }

    /// 文字列から真偽値を判定
    pub fn parse_bool_value(val: &str) -> Option<bool> {
        let s = val.trim();
        match s {
            "有効" | "オン" | "True" | "true" | "1" | "する" | "あり" | "◯" | "○" => Some(true),
            "無効" | "オフ" | "False" | "false" | "0" | "しない" | "なし" | "×" => Some(false),
            _ => None,
        }
    }

    /// Excelの設定キー名（全エイリアス対応）と値を受け取って対応する設定を更新
    pub fn update_from_key_value(&mut self, raw_key: &str, raw_val: &str) {
        let key = raw_key.trim().replace(' ', "").replace('_', "");
        let val = raw_val.trim();

        match key.as_str() {
            // 1. 2階裏 基本人数
            "2階裏基本人数" | "2階裏基準人数" | "2階裏ベース人数" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.sec_2f_back_base_cap = v;
                }
            }
            // 2. 看護師最低人数
            "2階裏看護師最低人数" | "看護師最低人数" | "2階裏看護師最低" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.nurse_min_count = v;
                }
            }
            // 3. 看護師推奨人数
            "2階裏看護師推奨人数" | "看護師推奨人数" | "2階裏看護師推奨" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.nurse_rec_count = v;
                }
            }
            // 4. 連勤上限日数
            "連勤上限日数" | "連勤上限" | "最大連勤数" | "連勤日数上限" => {
                if let Ok(v) = val.parse::<usize>() {
                    self.max_consecutive_work_days = v;
                }
            }
            // 5. 固定公休削減許容日数
            "固定公休削減許容日数" | "固定公休削減許容" | "固定公休不足許容" | "固定公休不足許容日数" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.fixed_off_shortage_allowance = v;
                }
            }
            // 6. 3連休抑制レベル
            "3連休抑制レベル" | "3連休抑制" | "3連休ペナルティ" => {
                self.consecutive_off_level = val.to_string();
            }
            // 7. セクション人数ブレ幅
            "セクション人数ブレ幅" | "人数ブレ幅" | "許容ブレ幅" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.sec_capacity_tolerance = v;
                }
            }
            // 8. 週休配分目標
            "週休配分目標" | "週休目標" | "週休配分" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.weekly_off_target = v;
                }
            }
            // 9. 既存シフト維持レベル
            "既存シフト維持レベル" | "シフト維持レベル" | "前回シフト維持" => {
                self.existing_shift_weight_level = val.to_string();
            }
            // 10. 2階裏 警告表示人数
            "2階裏警告表示人数" | "2階裏警告人数" | "警告表示人数" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.warn_2f_back_count = v;
                }
            }
            // 11. 祝日週 公休設定
            "祝日週公休設定" | "祝日週最低1日公休" | "祝日週固定公休振替" | "固定公休振替" | "祝日週振替公休" => {
                if let Some(b) = Self::parse_bool_value(val) {
                    self.holiday_week_off_rule = b;
                }
            }
            // 12. 週単位 公休平準化
            "週単位公休平準化" | "週休平準化" | "週単位週休平準化" => {
                if let Some(b) = Self::parse_bool_value(val) {
                    self.weekly_off_balance_rule = b;
                }
            }
            // 13. 2階裏 週4名最低日数
            "2階裏週4名最低日数" | "2階裏通常週4名最低日数" | "2階裏週4名日数" => {
                if let Ok(v) = val.parse::<i32>() {
                    self.sec_2f_back_weekly_4_min = v;
                }
            }
            // 14. 空欄スタッフ 公休固定ローテーション
            "空欄スタッフ公休固定ローテーション"
            | "空欄スタッフ公休ローテーション"
            | "空欄公休ローテーション"
            | "公休曜日固定ローテーション"
            | "公休ローテーション"
            | "空欄公休固定" => {
                if let Some(b) = Self::parse_bool_value(val) {
                    self.auto_rotate_off_rule = b;
                }
            }
            // 15. 指定公休日 有効
            "指定公休日有効" | "指定公休有効" | "指定公休日HARD適用" | "指定公休HARD適用" | "指定公休日" => {
                if let Some(b) = Self::parse_bool_value(val) {
                    self.specified_off_rule = b;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_level_weights() {
        assert_eq!(get_level_weight("HARD1"), 20_000_000);
        assert_eq!(get_level_weight("HARD"), 16_000_000);
        assert_eq!(get_level_weight("HARD2"), 16_000_000);
        assert_eq!(get_level_weight("HARD3"), 14_000_000);
        assert_eq!(get_level_weight("HIGH1"), 12_000_000);
        assert_eq!(get_level_weight("HIGH"), 9_000_000);
        assert_eq!(get_level_weight("SOFT"), 100_000);
        assert_eq!(get_level_weight("LOW"), 20_000);
        assert_eq!(get_level_weight("UNKNOWN"), 100_000);
    }

    #[test]
    fn test_rec_weights() {
        assert_eq!(get_rec_weight("HARD1"), 12_000_000);
        assert_eq!(get_rec_weight("HARD"), 11_000_000);
        assert_eq!(get_rec_weight("HARD3"), 10_500_000);
        assert_eq!(get_rec_weight("HIGH1"), 10_200_000);
        assert_eq!(get_rec_weight("HIGH2"), 4_000_000);
        assert!(get_rec_weight("HIGH1") > WEIGHT_GENERAL_SEC_SLACK);
        assert!(get_rec_weight("HIGH1") > get_level_weight("HIGH2"));
        assert!(WEIGHT_OPE_2F_BACK_REC_PENALTY > WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK);
        assert!(WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK > WEIGHT_2F_BACK_WEEKLY_4_SLACK);
        assert_eq!(get_rec_weight("SOFT2"), 200_000);
        assert_eq!(get_rec_weight("LOW"), 50_000);
    }

    #[test]
    fn test_shift_config_defaults_and_updates() {
        let mut cfg = ShiftConfig::default();
        assert_eq!(cfg.sec_2f_back_base_cap, 3);
        assert_eq!(cfg.nurse_min_count, 1);
        assert_eq!(cfg.nurse_rec_count, 2);
        assert_eq!(cfg.max_consecutive_work_days, 5);
        assert_eq!(cfg.fixed_off_shortage_allowance, 2);
        assert_eq!(cfg.consecutive_off_level, "中");
        assert_eq!(cfg.consecutive_off_weight(), 100_000);
        assert!(cfg.holiday_week_off_rule);
        assert!(cfg.auto_rotate_off_rule);
        assert!(cfg.specified_off_rule);

        // キー更新
        cfg.update_from_key_value("2階裏_基準人数", "4");
        assert_eq!(cfg.sec_2f_back_base_cap, 4);

        cfg.update_from_key_value("連勤上限", "6");
        assert_eq!(cfg.max_consecutive_work_days, 6);

        cfg.update_from_key_value("3連休抑制", "強");
        assert_eq!(cfg.consecutive_off_level, "強");
        assert_eq!(cfg.consecutive_off_weight(), 300_000);

        cfg.update_from_key_value("祝日週_固定公休振替", "オフ");
        assert!(!cfg.holiday_week_off_rule);
    }
}
