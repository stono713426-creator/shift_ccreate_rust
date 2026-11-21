//! 勤務シフト自動生成システム 厳格数理最適化＆原因診断エンジン (`solver.rs`)
//!
//! Version: v2.0.0
//! Updated: 2026-11-21 - v2.0.0 全面ブラッシュアップ刷新
//!   - 安易な事前クランプ・暗黙スラック降格・無条件Stage 2フォールバックを完全廃止
//!   - Tier 0（絶対不変: 日祝・休診・確定休暇・除外・月跨ぎ含む連勤上限）と
//!     Tier 1（厳格ハード: 所定勤務日数・2階裏最低/職種人数・Ope専従・週4基準・シートHARD）を分離
//!   - 厳格解なし時は「適切解なし」として明示停止し、診断用MILPにより原因とピンポイント/一括緩和候補を提示
//!   - 固定公休5回月の振替出勤調整、祝日週公休のソフト最適化、2階裏4名枠優先順位是正（Ope推奨 > 早上がり補填 > 通常週4名）、
//!     暦3連休抑制・7日ローリング窓の祝日/有休除外・除外/同時休みバグ修正・属性ベース化を実装
//!
//! - v1.3.5 (2026-11-17): 2階裏の日次最低出勤人数（有休1名まで減算なし・2名以上減算）、特別枠（堀さん）の週4基準維持
//! - v1.3.4 (2026-11-17): 「vit」（および「Vit,cat」）の勤務シフト条件上での「cat」同一条件扱い対応
//! - v1.3.0 (2026-11-08): 2階裏人数・職種バランス強化および個別勤務ルール対応
//! - v1.0.0 (2026-10-31): good_lp + HiGHS (MILP) 実装

use crate::config::*;
use crate::parser::*;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use good_lp::{
    constraint, default_solver, variable, Constraint, Expression, ProblemVariables, ResolutionError,
    Solution, SolutionStatus, SolverModel, Variable,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// ソルバーの解ステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SolveStatus {
    Optimal,
    Feasible,
    Infeasible,
    Timeout,
    Error,
}

/// 適切解なし時の競合原因項目
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictCause {
    pub category: String,
    pub scope_label: String,
    pub detail: String,
}

/// 適切解なし時の原因診断結果と緩和候補一覧
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosisResult {
    /// Tier 0（絶対不変制約・書式エラー等）により緩和選択肢でも解決不能であるか
    pub tier0_unsolvable: bool,
    pub summary: String,
    pub causes: Vec<ConflictCause>,
    pub proposals: Vec<RelaxationProposal>,
}

/// 生成結果の品質監査レポート（実務妥当性チェック用）
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShiftAuditReport {
    pub applied_relaxations: Vec<String>,
    pub target_mismatches: Vec<String>,
    pub fixed_off_swaps: Vec<String>,
    pub rec_misses: Vec<String>,
    pub main_off_slides: Vec<String>,
    pub early_overlaps: Vec<String>,
    pub warn_2f_back_days: Vec<String>,
    pub special_extra_work_days: Vec<String>,
}

/// ソルバーの実行結果
#[derive(Debug, Clone)]
pub struct SolveResult {
    pub success: bool,
    pub status: SolveStatus,
    /// 1: 厳格達成（緩和0件）, 2: 承認済み緩和を適用して達成, 0: 適切解なし/未生成
    pub stage: u8,
    /// スタッフ名 -> 日付 -> 1(出勤) / 0(休み)
    pub shifts: HashMap<String, HashMap<u32, u8>>,
    /// スタッフ名 -> 当月主公休曜日 (0=月..5=土)
    pub main_offs: HashMap<String, u32>,
    /// スタッフ名 -> 当月所定勤務日数
    pub target_days: HashMap<String, i32>,
    pub execution_time_secs: f64,
    pub logs: Vec<String>,
    pub approved_relaxations: Vec<RelaxationKind>,
    pub diagnosis: Option<DiagnosisResult>,
    pub audit: ShiftAuditReport,
}

/// カレンダー週（ISO週）情報
#[derive(Debug, Clone)]
pub struct CalendarWeek {
    pub week_index: usize,
    /// 当月の日付リスト
    pub days: Vec<u32>,
    /// 診療日（月〜土、日曜・祝日除く）のリスト
    pub clinic_days: Vec<u32>,
    /// 祝日が含まれているか
    pub has_holiday: bool,
    /// 平日（月〜金）に祝日が含まれているか
    pub has_weekday_holiday: bool,
}

/// 日付コンテキスト
#[derive(Debug, Clone)]
pub struct MonthContext {
    pub year: i32,
    pub month: u32,
    pub days_in_month: u32,
    /// 全日付 (1..=days_in_month)
    pub all_days: Vec<u32>,
    /// 診療日 (日曜および祝日を除く月〜土の日付)
    pub clinic_days: Vec<u32>,
    /// 各日付 -> NaiveDate
    pub date_map: HashMap<u32, NaiveDate>,
    /// 各日付 -> 曜日 (0=月..6=日)
    pub weekday_map: HashMap<u32, u32>,
    /// 祝日判定マップ
    pub is_holiday_map: HashMap<u32, bool>,
    /// カレンダー週リスト
    pub weeks: Vec<CalendarWeek>,
}

impl MonthContext {
    pub fn new(year: i32, month: u32, holidays: &HashMap<NaiveDate, String>) -> Self {
        let safe_year = year.clamp(1970, 2100);
        let safe_month = month.clamp(1, 12);
        let first_day = NaiveDate::from_ymd_opt(safe_year, safe_month, 1)
            .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        let next_month = if safe_month == 12 {
            NaiveDate::from_ymd_opt(safe_year + 1, 1, 1)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2027, 1, 1).unwrap())
        } else {
            NaiveDate::from_ymd_opt(safe_year, safe_month + 1, 1)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 2, 1).unwrap())
        };
        let days_in_month = (next_month - first_day).num_days().max(1) as u32;

        let mut all_days = Vec::new();
        let mut clinic_days = Vec::new();
        let mut date_map = HashMap::new();
        let mut weekday_map = HashMap::new();
        let mut is_holiday_map = HashMap::new();

        for d in 1..=days_in_month {
            let date = NaiveDate::from_ymd_opt(safe_year, safe_month, d).unwrap_or(first_day);
            let w = date.weekday().num_days_from_monday(); // 0=Mon..6=Sun
            let is_holiday = holidays.contains_key(&date);

            all_days.push(d);
            date_map.insert(d, date);
            weekday_map.insert(d, w);
            is_holiday_map.insert(d, is_holiday);

            if w != 6 && !is_holiday {
                clinic_days.push(d);
            }
        }

        // ISO週によるカレンダー週分割
        let mut weeks_map: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut week_order = Vec::new();
        for &d in &all_days {
            let date = date_map[&d];
            let iso_week = date.iso_week().week();
            if !weeks_map.contains_key(&iso_week) {
                week_order.push(iso_week);
            }
            weeks_map.entry(iso_week).or_default().push(d);
        }

        let mut weeks = Vec::new();
        for (w_idx, iso_w) in week_order.into_iter().enumerate() {
            let days = weeks_map[&iso_w].clone();
            let mut c_days = Vec::new();
            let mut has_holiday = false;
            let mut has_weekday_holiday = false;

            for &d in &days {
                if is_holiday_map[&d] {
                    has_holiday = true;
                    if weekday_map[&d] < 5 {
                        has_weekday_holiday = true;
                    }
                }
                if weekday_map[&d] != 6 && !is_holiday_map[&d] {
                    c_days.push(d);
                }
            }

            weeks.push(CalendarWeek {
                week_index: w_idx,
                days,
                clinic_days: c_days,
                has_holiday,
                has_weekday_holiday,
            });
        }

        Self {
            year: safe_year,
            month: safe_month,
            days_in_month,
            all_days,
            clinic_days,
            date_map,
            weekday_map,
            is_holiday_map,
            weeks,
        }
    }
}

/// 曜日番号 (0..=6) を日本語1文字に変換
fn weekday_kanji(w: u32) -> &'static str {
    match w {
        0 => "月",
        1 => "火",
        2 => "水",
        3 => "木",
        4 => "金",
        5 => "土",
        6 => "日",
        _ => "",
    }
}

/// 日付ラベル（例: "11/17(火)"）
fn format_day_label(ctx: &MonthContext, d: u32) -> String {
    let w = ctx.weekday_map.get(&d).copied().unwrap_or(0);
    format!("{}/{}({})", ctx.month, d, weekday_kanji(w))
}

/// 週ラベル（例: "第3週(11/16〜11/21)"）
fn format_week_label(ctx: &MonthContext, w_idx: usize) -> String {
    if let Some(week) = ctx.weeks.get(w_idx) {
        if let (Some(&first_d), Some(&last_d)) = (week.days.first(), week.days.last()) {
            return format!(
                "第{}週({}/{}〜{}/{})",
                w_idx + 1,
                ctx.month,
                first_d,
                ctx.month,
                last_d
            );
        }
    }
    format!("第{}週", w_idx + 1)
}

/// スタッフの個人休暇（有休・特有・希望休・指定公休）該当判定
fn is_personal_leave_on_date(staff: &Staff, date: &NaiveDate, config: &ShiftConfig) -> bool {
    staff.vacation_dates.contains(date)
        || staff.special_vacation_dates.contains(date)
        || staff.preferred_off_dates.contains(date)
        || (config.specified_off_rule && staff.specified_off_dates.contains(date))
}

/// コアスタッフ全員共通の特別有休（全期間分：月跨ぎ週の年末年始・夏季休診判定用）を算出
pub fn compute_all_common_sp_vac(data: &ParsedExcelData) -> HashSet<NaiveDate> {
    let core_staffs: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| {
            (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();

    let mut common_sp_vac = HashSet::new();
    if let Some(first) = core_staffs.first() {
        common_sp_vac = first.special_vacation_dates.clone();
        for s in core_staffs.iter().skip(1) {
            common_sp_vac = common_sp_vac
                .intersection(&s.special_vacation_dates)
                .cloned()
                .collect();
        }
    }
    common_sp_vac
}

/// コアスタッフ全員共通の特別有休（当月分：年末年始・夏季休診等）の日付集合を算出
pub fn compute_common_sp_vac(data: &ParsedExcelData, ctx: &MonthContext) -> HashSet<NaiveDate> {
    compute_all_common_sp_vac(data)
        .into_iter()
        .filter(|d| d.year() == ctx.year && d.month() == ctx.month)
        .collect()
}

/// 週4祝日非連動スタッフ用の各ISOカレンダー週の勤務計画情報
#[derive(Debug, Clone)]
pub struct HolidayIndependentWeek4Plan {
    pub week_index: usize,
    /// 当月内のこの週で出勤すべき日数（通常週・祝日週ともに原則4日、端数週・大型連休・有休取得時は整合補正）
    pub week_target: i32,
    /// 当月内の水曜日の日付（1..=days_in_month、当月外や休診日の場合は None）
    pub wed_day: Option<u32>,
    /// 水曜日に Ope・注射がなく、水曜以外で週4日出勤を満たせるため原則公休とすべきか
    pub wed_should_be_off: bool,
}

/// 後方互換エイリアス
pub type IshibashiWeekPlan = HolidayIndependentWeek4Plan;

/// 前月末尾シフト列を忠実に取得する（v2.0.0: 前月実績の無断書き換えは絶対に行わない）
fn extract_prev_tail_for_staff(staff: &Staff, data: &ParsedExcelData, ctx: &MonthContext) -> Vec<u8> {
    let max_k = data.config.max_consecutive_work_days;
    if max_k == 0 {
        return Vec::new();
    }
    let prev_tail_matches_month = match data.prev_sheet_year_month {
        Some((py, pm)) => {
            let next_ym = if pm == 12 { (py + 1, 1) } else { (py, pm + 1) };
            next_ym == (ctx.year, ctx.month)
        }
        None => true,
    };
    if !prev_tail_matches_month {
        return Vec::new();
    }
    let prev_tail = data
        .prev_month_tail_shifts
        .get(&staff.name)
        .cloned()
        .unwrap_or_default();
    if prev_tail.is_empty() {
        return Vec::new();
    }
    let tail_take = max_k.min(prev_tail.len());
    prev_tail[prev_tail.len() - tail_take..]
        .iter()
        .map(|&v| if v > 0 { 1 } else { 0 })
        .collect()
}

/// 週4祝日非連動スタッフ（石橋さん相当）の週次勤務計画を算出
pub fn compute_holiday_independent_week4_plans(
    staff: &Staff,
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
) -> Vec<HolidayIndependentWeek4Plan> {
    if staff.pattern == "除外" {
        return Vec::new();
    }

    let all_common_sp_vac = compute_all_common_sp_vac(data);
    let first_day_of_month = NaiveDate::from_ymd_opt(ctx.year, ctx.month, 1).unwrap();
    let last_day_of_month =
        NaiveDate::from_ymd_opt(ctx.year, ctx.month, ctx.days_in_month).unwrap();
    let prev_end_date = first_day_of_month - Duration::days(1);
    let prev_tail = extract_prev_tail_for_staff(staff, data, ctx);
    let trailing_prev_ones = prev_tail.iter().rev().take_while(|&&v| v == 1).count();

    let mut plans = Vec::with_capacity(ctx.weeks.len());

    for week in &ctx.weeks {
        if week.days.is_empty() {
            plans.push(HolidayIndependentWeek4Plan {
                week_index: week.week_index,
                week_target: 0,
                wed_day: None,
                wed_should_be_off: false,
            });
            continue;
        }

        let sample_date = ctx.date_map[&week.days[0]];
        let mon_date =
            sample_date - Duration::days(sample_date.weekday().num_days_from_monday() as i64);

        let mut in_curr = [false; 6];
        let mut in_prev = [false; 6];
        let mut in_next = [false; 6];
        let mut is_avail = [false; 6];
        let mut is_forced = [false; 6];
        let mut personal_vac_cnt = 0i32;
        let mut dates_6 = [mon_date; 6];

        for w in 0..6usize {
            let dt = mon_date + Duration::days(w as i64);
            dates_6[w] = dt;
            in_curr[w] = dt >= first_day_of_month && dt <= last_day_of_month;
            in_prev[w] = dt < first_day_of_month;
            in_next[w] = dt > last_day_of_month;

            let is_closed = holidays.contains_key(&dt) || all_common_sp_vac.contains(&dt);
            let is_personal_vac = !is_closed
                && (staff.vacation_dates.contains(&dt)
                    || staff.special_vacation_dates.contains(&dt));
            let is_other_leave = !is_closed
                && (staff.preferred_off_dates.contains(&dt)
                    || (data.config.specified_off_rule
                        && staff.specified_off_dates.contains(&dt)));
            let is_leave = is_personal_vac || is_other_leave;

            let is_basic_off = staff.basic_off_days.contains(&(w as u32));
            if !is_basic_off && is_personal_vac {
                personal_vac_cnt += 1;
            }

            let has_ope_or_inj = staff.is_ope_dedicated
                && !is_closed
                && !is_leave
                && (is_ope_date(&data.ope_dict, &dt) || is_injection_date(&data.ope_dict, &dt));

            // 当月1日が前月末尾からの連勤上限に達している場合はTier 0により出勤不可
            let blocked_by_prev_consec = dt == first_day_of_month
                && data.config.max_consecutive_work_days > 0
                && trailing_prev_ones >= data.config.max_consecutive_work_days;

            // Ope専従者のOpe日・注射日は基本公休曜日（土曜）を問わず出勤対象（ただしTier 0連勤不可日は除く）
            is_avail[w] = !is_closed
                && !is_leave
                && !blocked_by_prev_consec
                && (!is_basic_off || has_ope_or_inj);
            is_forced[w] = has_ope_or_inj && !blocked_by_prev_consec;
        }

        let avail_all = is_avail.iter().filter(|&&b| b).count() as i32;
        let forced_all = is_forced.iter().filter(|&&b| b).count() as i32;
        let avail_curr = (0..6).filter(|&w| in_curr[w] && is_avail[w]).count() as i32;
        let forced_curr = (0..6).filter(|&w| in_curr[w] && is_forced[w]).count() as i32;

        // 週4日出勤基準（個人の有休・夏休取得日がある週はその日数分を控除）
        let desired_full = (4 - personal_vac_cnt).max(0);
        let max_consec = if data.config.max_consecutive_work_days > 0 {
            data.config.max_consecutive_work_days as i32
        } else {
            7
        };
        let full_week_target = desired_full
            .min(avail_all)
            .max(forced_all)
            .min(max_consec);

        // 標準優先選択（1. Ope/注射必須日 -> 2. 水曜以外の平日[月,火,木,金] -> 3. 水曜 -> 4. 土曜）
        let mut std_selected = Vec::new();
        for (w, &forced) in is_forced.iter().enumerate() {
            if forced && std_selected.len() < full_week_target as usize {
                std_selected.push(w);
            }
        }
        for w in [0usize, 1, 3, 4, 2, 5] {
            if std_selected.len() >= full_week_target as usize {
                break;
            }
            if is_avail[w] && !std_selected.contains(&w) {
                std_selected.push(w);
            }
        }

        let has_prev = in_prev.iter().any(|&b| b);
        let has_next = in_next.iter().any(|&b| b);

        let week_target = if avail_curr == 0 {
            0
        } else if has_prev {
            let mut prev_worked = 0i32;
            for w in 0..6usize {
                if in_prev[w] {
                    let days_back = (prev_end_date - dates_6[w]).num_days();
                    if days_back >= 0 {
                        let db = days_back as usize;
                        if prev_tail.len() > db {
                            if prev_tail[prev_tail.len() - 1 - db] == 1 {
                                prev_worked += 1;
                            }
                            continue;
                        }
                    }
                    if std_selected.contains(&w) {
                        prev_worked += 1;
                    }
                }
            }
            (full_week_target - prev_worked)
                .max(0)
                .min(avail_curr)
                .max(forced_curr)
                .min(max_consec)
        } else if has_next {
            let sel_curr = std_selected.iter().filter(|&&w| in_curr[w]).count() as i32;
            sel_curr
                .min(avail_curr)
                .max(forced_curr)
                .min(max_consec)
        } else {
            full_week_target
                .min(avail_curr)
                .max(forced_curr)
                .min(max_consec)
        };

        let wed_day = if in_curr[2] && is_avail[2] {
            Some(dates_6[2].day())
        } else {
            None
        };
        let wed_should_be_off = staff.has_conditional_wed_off
            && in_curr[2]
            && is_avail[2]
            && !is_forced[2]
            && (avail_curr - 1) >= week_target;

        plans.push(HolidayIndependentWeek4Plan {
            week_index: week.week_index,
            week_target,
            wed_day,
            wed_should_be_off,
        });
    }

    plans
}

/// 後方互換ラッパー（石橋さん向け）
pub fn compute_ishibashi_week_plans(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
) -> Vec<IshibashiWeekPlan> {
    let Some(staff) = data
        .staffs
        .iter()
        .find(|s| (s.is_holiday_independent_week4 || s.name == "石橋") && s.pattern != "除外")
    else {
        return Vec::new();
    };
    compute_holiday_independent_week4_plans(staff, data, ctx, holidays)
}

/// Tier 0（日祝・休診日・確定休暇・月跨ぎ含む連勤上限）のみを考慮した物理的最大出勤可能日数
fn compute_tier0_max_workable_days(
    staff: &Staff,
    data: &ParsedExcelData,
    ctx: &MonthContext,
    common_sp_vac: &HashSet<NaiveDate>,
) -> i32 {
    if staff.pattern == "除外" {
        return 0;
    }
    let max_k = data.config.max_consecutive_work_days;
    let tail_slice = extract_prev_tail_for_staff(staff, data, ctx);
    let mut streak = tail_slice.iter().rev().take_while(|&&v| v == 1).count();
    let mut max_workable = 0i32;

    for &d in &ctx.all_days {
        let date = ctx.date_map[&d];
        let w = ctx.weekday_map[&d];

        let is_closed = w == 6 || ctx.is_holiday_map[&d] || common_sp_vac.contains(&date);
        let is_leave = is_personal_leave_on_date(staff, &date, &data.config);

        if is_closed || is_leave {
            streak = 0;
        } else if max_k > 0 && streak >= max_k {
            streak = 0;
        } else {
            streak += 1;
            max_workable += 1;
        }
    }

    max_workable
}

/// 5.2 所定勤務日数の計算（v2.0.0: 希望休超過や固定公休5回月でも無断クランプせず契約上の所定日数を算出）
pub fn calculate_target_days(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
) -> (HashMap<String, i32>, HashMap<String, f64>) {
    let mut target_days = HashMap::new();
    let mut ideal_mean = HashMap::new();

    let common_sp_vac = compute_common_sp_vac(data, ctx);

    // 共通公休日数（月間共通公休数ベース）
    let mut common_off_w5 = 0;
    let mut common_off_w4 = 0;

    for &d in &ctx.all_days {
        let date = ctx.date_map[&d];
        let w = ctx.weekday_map[&d];
        let is_holiday = ctx.is_holiday_map[&d];
        let is_common_sp = common_sp_vac.contains(&date);

        if !is_holiday && !is_common_sp {
            if w == 5 {
                common_off_w5 += 1;
                common_off_w4 += 1;
            } else if w == 4 {
                common_off_w4 += 1;
            }
        }
    }

    // basic_slots = 暦日数 - 日曜数 - 日曜以外の祝日数
    let sunday_count = ctx.all_days.iter().filter(|&&d| ctx.weekday_map[&d] == 6).count() as i32;
    let holiday_not_sun_count = ctx
        .all_days
        .iter()
        .filter(|&&d| ctx.is_holiday_map[&d] && ctx.weekday_map[&d] != 6)
        .count() as i32;
    let basic_slots = ctx.days_in_month as i32 - sunday_count - holiday_not_sun_count;

    for staff in &data.staffs {
        if staff.pattern == "除外" {
            target_days.insert(staff.name.clone(), 0);
            continue;
        }

        // 週4祝日非連動（石橋さん相当）
        if staff.is_holiday_independent_week4 {
            let plans = compute_holiday_independent_week4_plans(staff, data, ctx, holidays);
            let w4_target: i32 = plans.iter().map(|p| p.week_target).sum();
            target_days.insert(staff.name.clone(), w4_target);
            continue;
        }

        if !staff.is_core {
            // パート等非コア: 希望出勤診療日数をそのまま所定日数とする（連勤上限超過も隠蔽しない）
            let mut work_days_count = 0;
            for &d in &ctx.clinic_days {
                let date = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                let is_basic_off = staff.basic_off_days.contains(&w);
                if !common_sp_vac.contains(&date)
                    && !is_basic_off
                    && !is_personal_leave_on_date(staff, &date, &data.config)
                {
                    work_days_count += 1;
                }
            }
            target_days.insert(staff.name.clone(), work_days_count);
        } else {
            // 通常コアスタッフ（無断クランプ廃止）
            let common_off = if staff.pattern.contains("週4") {
                common_off_w4
            } else {
                common_off_w5
            };

            let spec_off_count = if data.config.specified_off_rule {
                staff
                    .specified_off_dates
                    .iter()
                    .filter(|d| {
                        d.year() == ctx.year
                            && d.month() == ctx.month
                            && d.weekday() != Weekday::Sun
                            && !holidays.contains_key(d)
                    })
                    .count() as i32
            } else {
                0
            };

            let sp_vac_count = staff
                .special_vacation_dates
                .iter()
                .filter(|d| {
                    d.year() == ctx.year
                        && d.month() == ctx.month
                        && d.weekday() != Weekday::Sun
                        && !holidays.contains_key(d)
                })
                .count() as i32;

            let vac_count = staff
                .vacation_dates
                .iter()
                .filter(|d| {
                    d.year() == ctx.year
                        && d.month() == ctx.month
                        && d.weekday() != Weekday::Sun
                        && !holidays.contains_key(d)
                })
                .count() as i32;

            let effective_off = common_off.max(spec_off_count);
            let raw_target = (basic_slots - effective_off - sp_vac_count - vac_count).max(0);
            target_days.insert(staff.name.clone(), raw_target);
        }
    }

    // セクション別理想平均人数 M_s (全員共通特別有休の休診日を除いた実稼働日数で算出)
    let work_days_len = ctx
        .clinic_days
        .iter()
        .filter(|&&d| !common_sp_vac.contains(&ctx.date_map[&d]))
        .count() as f64;
    if work_days_len > 0.0 {
        let sum_1f: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "1階" && s.pattern != "除外")
            .map(|s| target_days.get(&s.name).copied().unwrap_or(0))
            .sum();
        ideal_mean.insert("1階".to_string(), sum_1f as f64 / work_days_len);

        let sum_2f_front: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "2階表" && s.is_core && s.pattern != "除外")
            .map(|s| target_days.get(&s.name).copied().unwrap_or(0))
            .sum();
        ideal_mean.insert("2階表".to_string(), sum_2f_front as f64 / work_days_len);

        let sum_2f_back_raw: i32 = data
            .staffs
            .iter()
            .filter(|s| {
                s.section == "2階裏"
                    && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                    && s.pattern != "除外"
            })
            .map(|s| target_days.get(&s.name).copied().unwrap_or(0))
            .sum();

        // Ope日の Ope専従スタッフ出勤分は 2階裏（外来）人数から除外して平均を算出
        let ope_ded_days: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "2階裏" && s.is_ope_dedicated && s.pattern != "除外")
            .map(|ded| {
                ctx.clinic_days
                    .iter()
                    .filter(|&&d| {
                        let dt = ctx.date_map[&d];
                        !common_sp_vac.contains(&dt)
                            && is_ope_date(&data.ope_dict, &dt)
                            && !is_personal_leave_on_date(ded, &dt, &data.config)
                    })
                    .count() as i32
            })
            .sum();

        let sum_2f_back = (sum_2f_back_raw - ope_ded_days).max(0);
        ideal_mean.insert("2階裏".to_string(), sum_2f_back as f64 / work_days_len);
    }

    (target_days, ideal_mean)
}

/// ルール適用対象日リストの抽出（v2.0.0: 複数曜日対応、未知文字列の全日誤爆防止）
pub fn parse_rule_target_dates(
    desc: &str,
    ctx: &MonthContext,
    ope_dict: &HashMap<NaiveDate, String>,
) -> Vec<u32> {
    if !validate_rule_condition_desc(desc) {
        return Vec::new();
    }

    let desc_trim = desc.trim();
    let desc_lower = desc_trim.to_lowercase();
    let mut target_days = Vec::new();

    // 複数曜日の同時指定に対応 (例: "火曜・水曜")
    let mut matched_weekdays: Vec<u32> = Vec::new();
    for (kw, w_num) in [
        ("月曜", 0u32),
        ("火曜", 1),
        ("水曜", 2),
        ("木曜", 3),
        ("金曜", 4),
        ("土曜", 5),
    ] {
        if desc_trim.contains(kw) && !matched_weekdays.contains(&w_num) {
            matched_weekdays.push(w_num);
        }
    }

    let base_days_by_weekday = || -> Vec<u32> {
        if matched_weekdays.is_empty() {
            ctx.clinic_days.clone()
        } else {
            ctx.clinic_days
                .iter()
                .filter(|&&d| matched_weekdays.contains(&ctx.weekday_map[&d]))
                .copied()
                .collect()
        }
    };

    if desc_trim.contains("全日") {
        target_days = ctx.clinic_days.clone();
    } else if desc_trim.contains("注射") {
        for d in base_days_by_weekday() {
            if is_injection_date(ope_dict, &ctx.date_map[&d]) {
                target_days.push(d);
            }
        }
    } else if desc_trim.contains("手術のある日") || desc_trim.contains("日曜日以外の手術のある日") {
        target_days = base_days_by_weekday()
            .into_iter()
            .filter(|&d| is_ope_date(ope_dict, &ctx.date_map[&d]))
            .collect();
    } else if desc_trim.contains("手術") || desc_lower.contains("ope") || desc_trim.contains("オペ") {
        let base_days = base_days_by_weekday();

        let mut kw = String::new();
        if let Some(start) = desc_trim.find('「') {
            let content_start = start + '「'.len_utf8();
            if let Some(rel_end) = desc_trim[content_start..].find('」') {
                kw = desc_trim[content_start..content_start + rel_end]
                    .trim()
                    .to_string();
            }
        }
        if kw.is_empty() {
            if desc_trim.contains("硝子体") || desc_lower.contains("vit") || desc_lower.contains("viit") {
                kw = "vit".to_string();
            } else if desc_trim.contains("下垂") || desc_trim.contains("眼瞼") {
                kw = "下垂".to_string();
            } else if desc_trim.contains("白内障") || desc_lower.contains("cat") || desc_lower.contains("cay") {
                kw = "cat".to_string();
            } else if desc_trim.contains("緑内障") || desc_lower.contains("gla") {
                kw = "gla".to_string();
            }
        }

        let kw_lower = kw.to_lowercase();
        for d in base_days {
            let date = ctx.date_map[&d];
            if kw.is_empty() {
                if is_ope_date(ope_dict, &date) {
                    target_days.push(d);
                }
            } else if let Some(ope_text) = ope_dict.get(&date) {
                let norm_ope = normalize_ope_type_text(ope_text);
                let ope_lower = norm_ope.to_lowercase();
                let matched = if kw_lower == "vit" || kw_lower == "viit" || kw == "硝子体" {
                    ope_lower.contains("vit") || ope_lower.contains("viit") || norm_ope.contains("硝子体")
                } else if kw_lower == "cat" || kw_lower == "cay" || kw == "白内障" {
                    ope_lower.contains("cat")
                        || ope_lower.contains("cay")
                        || norm_ope.contains("白内障")
                        || ope_lower.contains("vit")
                        || ope_lower.contains("viit")
                        || norm_ope.contains("硝子体")
                } else if kw_lower == "gla" || kw == "緑内障" {
                    ope_lower.contains("gla") || norm_ope.contains("緑内障")
                } else if kw == "下垂" || kw == "眼瞼" || kw == "眼瞼下垂" {
                    norm_ope.contains("下垂") || norm_ope.contains("眼瞼")
                } else {
                    ope_lower.contains(&kw_lower)
                };
                if matched {
                    target_days.push(d);
                }
            }
        }
    } else if !matched_weekdays.is_empty() {
        target_days = base_days_by_weekday();
    }

    target_days
}

/// 2階裏（外来）の日次最低必要人数を算出（v2.0.0: 物理上限による無言クランプを一切行わない）
/// 戻り値: `(req_total_min, req_nurse_min, req_exam_min)`
fn compute_2f_back_day_requirements(
    core_2f_back: &[&Staff],
    outpatient_2f_b: &[&Staff],
    date: NaiveDate,
    w: u32,
    is_ope: bool,
    config: &ShiftConfig,
) -> (i32, i32, i32) {
    let vac_2f_b_on_d = outpatient_2f_b
        .iter()
        .filter(|s| is_personal_leave_on_date(s, &date, config))
        .count() as i32;
    // 非Ope日は有休・指定公休1名までは減算せず3名維持、2名以上の日は(人数-1)分を減算
    let vac_deduct = (vac_2f_b_on_d - 1).max(0);
    let non_ope_base_min = (3.max(config.sec_2f_back_base_cap) - vac_deduct).max(0);

    let (mut req_total_min, mut req_nurse_min, mut req_exam_min) = if is_ope {
        (
            3.max(config.sec_2f_back_base_cap),
            1.max(config.nurse_min_count),
            if w == 1 { 1 } else { 2 }, // 火曜Ope日は検査>=1、火曜以外のOpe日は検査>=2
        )
    } else {
        (
            (config.sec_2f_back_base_cap - vac_deduct).max(0),
            config.nurse_min_count.max(0),
            0,
        )
    };

    match w {
        1 => {
            // 火曜: 看護師>=2, 検査>=1
            req_nurse_min = req_nurse_min.max(2);
            req_exam_min = req_exam_min.max(1);
            if is_ope {
                req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
            } else {
                req_total_min = req_total_min.max(non_ope_base_min);
            }
        }
        3 => {
            // 木曜: 看護師>=1, 検査>=2
            req_nurse_min = req_nurse_min.max(1);
            req_exam_min = req_exam_min.max(2);
            if is_ope {
                req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
            } else {
                req_total_min = req_total_min.max(non_ope_base_min);
            }
        }
        5 => {
            // 土曜: 通常3名以上（非Ope日かつ2階裏メンバーにPM有休ありの土曜のみ2名許容）
            let has_pm_leave_2f_b = core_2f_back.iter().any(|s| s.pm_leave_dates.contains(&date));
            if has_pm_leave_2f_b && !is_ope {
                req_total_min = 2.min(non_ope_base_min.max(2));
            } else if is_ope {
                req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
            } else {
                req_total_min = req_total_min.max(non_ope_base_min);
            }
        }
        _ => {}
    }

    (req_total_min, req_nurse_min, req_exam_min)
}

/// 緩和種別を日本語で分かりやすくフォーマット
pub fn format_relaxation_kind(kind: &RelaxationKind, ctx: &MonthContext) -> String {
    let scope_str = |scope: &RelaxationScope| -> String {
        match *scope {
            RelaxationScope::Day(d) => format_day_label(ctx, d),
            RelaxationScope::Week(w) => format_week_label(ctx, w),
            RelaxationScope::Month => format!("{}年{}月全体", ctx.year, ctx.month),
        }
    };

    match kind {
        RelaxationKind::SpecialExtraWork { staff_name, scope } => {
            format!("{} の {} 週5出勤を許可", staff_name, scope_str(scope))
        }
        RelaxationKind::ConditionalWedOff { staff_name, scope } => {
            format!("{} の {} 非Ope水曜公休を他曜日へ振替", staff_name, scope_str(scope))
        }
        RelaxationKind::Sec2fBackMinTotal { scope, relaxed_min } => {
            format!("{} 2階裏・最低総人数を {}名 へ緩和", scope_str(scope), relaxed_min)
        }
        RelaxationKind::Sec2fBackMinNurse { scope, relaxed_min } => {
            format!("{} 2階裏・看護師最低人数を {}名 へ緩和", scope_str(scope), relaxed_min)
        }
        RelaxationKind::Sec2fBackMinExam { scope, relaxed_min } => {
            format!("{} 2階裏・検査最低人数を {}名 へ緩和", scope_str(scope), relaxed_min)
        }
        RelaxationKind::OpeDedicatedWork { staff_name, scope } => {
            format!("{} の {} Ope/注射必須出勤を免除", staff_name, scope_str(scope))
        }
        RelaxationKind::FixedOffDaySwap { staff_name, scope } => {
            format!("{} の {} 固定公休を別日へ振替", staff_name, scope_str(scope))
        }
        RelaxationKind::WeeklyTargetFlex { staff_name, week_index } => {
            format!("{} の {} 週次出勤目標を柔軟化(±1日)", staff_name, format_week_label(ctx, *week_index))
        }
        RelaxationKind::SheetHardRule { rule_label, scope, .. } => {
            format!("{} 制約『{}』の最低人数を緩和", scope_str(scope), rule_label)
        }
        RelaxationKind::TargetDaysAdjust { staff_name, delta } => {
            let who = staff_name.as_deref().unwrap_or("全体");
            let d_str = if *delta < 0 {
                format!("{delta}日")
            } else if *delta > 0 {
                format!("+{delta}日")
            } else {
                "±1日".to_string()
            };
            format!("【最終手段】{} の所定勤務日数を {} 調整", who, d_str)
        }
    }
}

/// モデル構築モード
enum SolveMode<'a> {
    /// 厳格探索（承認済み緩和のみ反映し、それ以外の Tier 0 / Tier 1 はすべてハード厳守）
    Strict(&'a HashSet<RelaxationKind>),
    /// 診断用MILP（Tier 0 は絶対ハード固定、未緩和の Tier 1 に最小緩和スラックを導入）
    Diagnose(&'a HashSet<RelaxationKind>),
}

/// 診断用MILPが保持するスラック変数群
#[derive(Default)]
struct DiagnosticSlacks {
    special_extra_weeks: Vec<(String, usize, Variable)>,
    wed_off_slacks: Vec<(String, u32, Variable)>,
    weekly_flex_slacks: Vec<(String, usize, Variable, Variable)>,
    min_total_slacks: Vec<(u32, i32, Variable)>,
    min_nurse_slacks: Vec<(u32, i32, Variable)>,
    min_exam_slacks: Vec<(u32, i32, Variable)>,
    ope_ded_slacks: Vec<(String, u32, Variable)>,
    fixed_off_day_slacks: Vec<(String, u32, Variable)>,
    fixed_off_agg_slacks: Vec<(String, Variable, Vec<(u32, Variable)>)>,
    sheet_hard_slacks: Vec<(usize, String, u32, i32, Variable)>,
    target_adjust_slacks: Vec<(String, Variable, Variable)>,
}

/// 勤務シフト自動生成の実行（厳格探索・自動緩和なし）
pub fn solve_shift(
    data: &ParsedExcelData,
    year: i32,
    month: u32,
    holidays: &HashMap<NaiveDate, String>,
) -> SolveResult {
    solve_shift_with_relaxations(data, year, month, holidays, &HashSet::new())
}

/// 承認済み緩和集合を指定して勤務シフト最適化を実行（解なし時は自動緩和せず原因診断を付与して停止）
pub fn solve_shift_with_relaxations(
    data: &ParsedExcelData,
    year: i32,
    month: u32,
    holidays: &HashMap<NaiveDate, String>,
    approved_relaxations: &HashSet<RelaxationKind>,
) -> SolveResult {
    let start_time = Instant::now();
    let mut logs = Vec::new();
    logs.push(format!("=== 勤務シフト生成開始: {year}年{month:02}月 ==="));

    let ctx = MonthContext::new(year, month, holidays);
    let (target_days_dict, ideal_mean) = calculate_target_days(data, &ctx, holidays);

    logs.push(format!(
        "カレンダー設定完了: 暦日数={}, 診療日数={}, 週数={}, 承認済み緩和={}件",
        ctx.days_in_month,
        ctx.clinic_days.len(),
        ctx.weeks.len(),
        approved_relaxations.len()
    ));

    // 無効な制約条件書式が含まれている場合は即停止
    let invalid_rules: Vec<&ShiftRule> = data
        .rules
        .iter()
        .filter(|r| r.is_invalid_condition)
        .collect();
    if !invalid_rules.is_empty() {
        let causes: Vec<ConflictCause> = invalid_rules
            .iter()
            .map(|r| ConflictCause {
                category: "制約書式エラー".to_string(),
                scope_label: r.name.clone(),
                detail: format!(
                    "適用条件『{}』が認識できない書式です（有効例: 全日 / 月曜〜土曜 / 手術のある日 / 火曜(OPE日) / 「Vit」手術日 / 注射日）。",
                    r.condition_desc
                ),
            })
            .collect();
        let summary = format!(
            "シフト制約条件シートに認識できない適用条件が {} 件あります。条件表を修正してください。",
            causes.len()
        );
        logs.push(format!("❌ {summary}"));
        return SolveResult {
            success: false,
            status: SolveStatus::Infeasible,
            stage: 0,
            shifts: HashMap::new(),
            main_offs: HashMap::new(),
            target_days: target_days_dict,
            execution_time_secs: start_time.elapsed().as_secs_f64(),
            logs,
            approved_relaxations: approved_relaxations.iter().cloned().collect(),
            diagnosis: Some(DiagnosisResult {
                tier0_unsolvable: true,
                summary,
                causes,
                proposals: Vec::new(),
            }),
            audit: ShiftAuditReport::default(),
        };
    }

    let (mut strict_res, _) = build_and_solve_model(
        data,
        &ctx,
        &target_days_dict,
        &ideal_mean,
        holidays,
        SolveMode::Strict(approved_relaxations),
    );

    if strict_res.success {
        let elapsed = start_time.elapsed().as_secs_f64();
        let audit = build_audit_report(
            data,
            &ctx,
            holidays,
            &strict_res.shifts,
            &strict_res.main_offs,
            &target_days_dict,
            approved_relaxations,
        );
        strict_res.audit = audit;
        strict_res.stage = if approved_relaxations.is_empty() { 1 } else { 2 };
        strict_res.execution_time_secs = elapsed;
        strict_res.approved_relaxations = approved_relaxations.iter().cloned().collect();

        logs.extend(strict_res.logs);
        if strict_res.stage == 1 {
            logs.push(format!("厳格条件で最適解を取得しました (所要時間: {:.2}秒)", elapsed));
        } else {
            logs.push(format!(
                "承認緩和 {} 件を適用して解を取得しました (所要時間: {:.2}秒)",
                approved_relaxations.len(),
                elapsed
            ));
        }
        append_audit_logs(&strict_res.audit, &mut logs);
        strict_res.logs = logs;
        return strict_res;
    }

    // 厳格探索で解なし -> 自動緩和せず「適切解なし」とし、診断解析を実行
    logs.extend(strict_res.logs);
    if strict_res.status == SolveStatus::Infeasible {
        logs.push("❌ 現在の条件では適切解が存在しません（自動緩和せず原因を診断中...）".to_string());
        let diag = diagnose_infeasibility(
            data,
            &ctx,
            &target_days_dict,
            &ideal_mean,
            holidays,
            approved_relaxations,
        );
        logs.push(format!("診断結果: {}", diag.summary));
        for c in &diag.causes {
            logs.push(format!("  - [{}] {}: {}", c.category, c.scope_label, c.detail));
        }
        for p in &diag.proposals {
            let tag = if p.is_last_resort {
                "[最終手段]"
            } else if p.recommended {
                "[推奨]"
            } else {
                "[代替]"
            };
            logs.push(format!("  * {} {}: {}", tag, p.label, p.impact_desc));
        }
        strict_res.diagnosis = Some(diag);
    }

    strict_res.stage = 0;
    strict_res.execution_time_secs = start_time.elapsed().as_secs_f64();
    strict_res.approved_relaxations = approved_relaxations.iter().cloned().collect();
    strict_res.logs = logs;
    strict_res
}

/// ヘルパー: 特定日の2階裏最低人数に対する承認済み緩和後下限を取得
fn resolve_approved_2f_min(
    base_min: i32,
    d: u32,
    w_idx: usize,
    approved: &HashSet<RelaxationKind>,
    selector: impl Fn(&RelaxationKind) -> Option<(&RelaxationScope, i32)>,
) -> i32 {
    let mut eff = base_min;
    for r in approved {
        if let Some((scope, relaxed)) = selector(r) {
            if scope.matches(d, w_idx) {
                eff = eff.min(relaxed).max(0);
            }
        }
    }
    eff
}

/// 単一モデルの構築およびソルブ（Strict / Diagnose 共通ビルダー）
fn build_and_solve_model(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    target_days_dict: &HashMap<String, i32>,
    ideal_mean: &HashMap<String, f64>,
    holidays: &HashMap<NaiveDate, String>,
    mode: SolveMode<'_>,
) -> (SolveResult, Option<DiagnosticSlacks>) {
    let mut logs = Vec::new();
    let mut vars = ProblemVariables::new();
    let mut constraints: Vec<Constraint> = Vec::new();
    let mut objective = Expression::from(0.0);

    let (approved, is_diagnose) = match mode {
        SolveMode::Strict(set) => (set, false),
        SolveMode::Diagnose(set) => (set, true),
    };
    let mut diag_slacks = if is_diagnose {
        Some(DiagnosticSlacks::default())
    } else {
        None
    };

    let config = &data.config;
    let common_sp_vac = compute_common_sp_vac(data, ctx);
    let day_to_week_idx: HashMap<u32, usize> = ctx
        .weeks
        .iter()
        .flat_map(|w| w.days.iter().map(move |&d| (d, w.week_index)))
        .collect();

    // 1. 決定変数 x[s, d]
    let mut x_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        for &d in &ctx.all_days {
            let var = vars.add(variable().binary());
            x_vars.insert((staff.name.clone(), d), var);
        }
    }

    // 2. 決定変数 y[s, w] (主公休 / 第2公休フラグ)
    let mut y_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        if (staff.is_core || staff.is_holiday_independent_week4 || staff.is_ope_dedicated)
            && staff.pattern != "除外"
        {
            for w in 0..=5 {
                let var = vars.add(variable().binary());
                y_vars.insert((staff.name.clone(), w), var);
            }
        }
    }

    // 3. スタッフ別制約の構築
    for staff in &data.staffs {
        let s_name = &staff.name;

        // Tier 0: 除外スタッフ
        if staff.pattern == "除外" {
            for &d in &ctx.all_days {
                constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
            }
            continue;
        }

        // Tier 0: 日曜・祝日・全員共通特別有休・個人確定休暇は絶対に休み (x == 0)
        for &d in &ctx.all_days {
            let date = ctx.date_map[&d];
            if ctx.weekday_map[&d] == 6
                || ctx.is_holiday_map[&d]
                || common_sp_vac.contains(&date)
                || is_personal_leave_on_date(staff, &date, config)
            {
                constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
            }
        }

        // Tier 0: 連勤上限制約 (月またぎ含む、前月末尾実績は一切改変しない)
        let max_k = config.max_consecutive_work_days;
        let tail_slice = extract_prev_tail_for_staff(staff, data, ctx);
        let tail_take = tail_slice.len();
        let total_days = ctx.days_in_month as usize;
        let padded_len = tail_take + total_days;
        if max_k > 0 {
            for window_start in 0..padded_len {
                if window_start + max_k >= padded_len {
                    break;
                }
                let mut sum_expr = Expression::from(0.0);
                for offset in 0..=max_k {
                    let idx = window_start + offset;
                    if idx < tail_take {
                        sum_expr += tail_slice[idx] as f64;
                    } else {
                        let d = (idx - tail_take + 1) as u32;
                        sum_expr += x_vars[&(s_name.clone(), d)];
                    }
                }
                constraints.push(constraint!(sum_expr <= max_k as f64));
            }
        }

        // 承認済み「所定勤務日数±1日」調整の抽出
        let mut allow_target_under = false;
        let mut allow_target_over = false;
        for r in approved {
            if let RelaxationKind::TargetDaysAdjust { staff_name, delta } = r {
                if staff_name.as_deref().map(|n| n == s_name).unwrap_or(true) {
                    if *delta <= 0 {
                        allow_target_under = true;
                    }
                    if *delta >= 0 {
                        allow_target_over = true;
                    }
                }
            }
        }

        // 週4祝日非連動（石橋さん相当）
        if staff.is_holiday_independent_week4 {
            let plans = compute_holiday_independent_week4_plans(staff, data, ctx, holidays);

            // 主公休表示用フラグは水曜(2)に固定
            for w in 0..=5 {
                let target_y = if w == 2 { 1.0 } else { 0.0 };
                constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == target_y));
            }

            for week in &ctx.weeks {
                let Some(plan) = plans.get(week.week_index) else {
                    continue;
                };

                for &d in &week.clinic_days {
                    let date = ctx.date_map[&d];
                    let w = ctx.weekday_map[&d];

                    if common_sp_vac.contains(&date) || is_personal_leave_on_date(staff, &date, config)
                    {
                        continue;
                    }

                    let has_ope_or_inj = staff.is_ope_dedicated
                        && (is_ope_date(&data.ope_dict, &date)
                            || is_injection_date(&data.ope_dict, &date));

                    if has_ope_or_inj {
                        let is_ope_relaxed = approved.iter().any(|r| {
                            matches!(
                                r,
                                RelaxationKind::OpeDedicatedWork { staff_name, scope }
                                if staff_name == s_name && scope.matches(d, week.week_index)
                            )
                        });
                        if is_diagnose {
                            let slack = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                x_vars[&(s_name.clone(), d)] + slack >= 1.0
                            ));
                            if is_ope_relaxed {
                                objective += slack * 1.0;
                            } else {
                                objective += slack * DIAG_WEIGHT_OPE_DEDICATED as f64;
                                diag_slacks
                                    .as_mut()
                                    .unwrap()
                                    .ope_ded_slacks
                                    .push((s_name.clone(), d, slack));
                            }
                        } else if is_ope_relaxed {
                            let slack = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                x_vars[&(s_name.clone(), d)] + slack >= 1.0
                            ));
                            objective += slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                        } else {
                            constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 1.0));
                        }
                    } else if staff.basic_off_days.contains(&w) {
                        let is_swap_relaxed = approved.iter().any(|r| {
                            matches!(
                                r,
                                RelaxationKind::FixedOffDaySwap { staff_name, scope }
                                if staff_name == s_name && scope.matches(d, week.week_index)
                            )
                        });
                        if is_diagnose {
                            let slack = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                x_vars[&(s_name.clone(), d)] - slack <= 0.0
                            ));
                            if is_swap_relaxed {
                                objective += slack * 1.0;
                            } else {
                                objective += slack * DIAG_WEIGHT_FIXED_OFF_SWAP as f64;
                                diag_slacks
                                    .as_mut()
                                    .unwrap()
                                    .fixed_off_day_slacks
                                    .push((s_name.clone(), d, slack));
                            }
                        } else if is_swap_relaxed {
                            objective +=
                                x_vars[&(s_name.clone(), d)] * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                        } else {
                            constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                        }
                    }
                }

                // 条件付き水曜公休
                if let Some(wed_d) = plan.wed_day {
                    if plan.wed_should_be_off {
                        let is_wed_relaxed = approved.iter().any(|r| {
                            matches!(
                                r,
                                RelaxationKind::ConditionalWedOff { staff_name, scope }
                                if staff_name == s_name && scope.matches(wed_d, week.week_index)
                            )
                        });
                        if is_diagnose {
                            let slack = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                x_vars[&(s_name.clone(), wed_d)] - slack <= 0.0
                            ));
                            if is_wed_relaxed {
                                objective += slack * 1.0;
                            } else {
                                objective += slack * DIAG_WEIGHT_CONDITIONAL_WED_OFF as f64;
                                diag_slacks
                                    .as_mut()
                                    .unwrap()
                                    .wed_off_slacks
                                    .push((s_name.clone(), wed_d, slack));
                            }
                        } else if is_wed_relaxed {
                            objective += x_vars[&(s_name.clone(), wed_d)]
                                * WEIGHT_ISHIBASHI_WED_OFF_SLACK as f64;
                        } else {
                            constraints.push(constraint!(x_vars[&(s_name.clone(), wed_d)] == 0.0));
                        }
                    }
                }

                // 週4日出勤目標
                let week_sum: Expression = week
                    .clinic_days
                    .iter()
                    .map(|&d| x_vars[&(s_name.clone(), d)])
                    .sum();
                let is_week_flex = approved.iter().any(|r| {
                    matches!(
                        r,
                        RelaxationKind::WeeklyTargetFlex { staff_name, week_index }
                        if staff_name == s_name && *week_index == week.week_index
                    )
                });
                if is_diagnose {
                    let w_under = vars.add(variable().min(0.0));
                    let w_over = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        week_sum.clone() + w_under >= plan.week_target as f64
                    ));
                    constraints.push(constraint!(
                        week_sum - w_over <= plan.week_target as f64
                    ));
                    let wt = if is_week_flex {
                        1.0
                    } else {
                        DIAG_WEIGHT_WEEKLY_FLEX as f64
                    };
                    objective += (w_under + w_over) * wt;
                    if !is_week_flex {
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .weekly_flex_slacks
                            .push((s_name.clone(), week.week_index, w_under, w_over));
                    }
                } else if is_week_flex || allow_target_under || allow_target_over {
                    let w_under = vars.add(variable().min(0.0));
                    let w_over = vars.add(variable().min(0.0));
                    if !(is_week_flex || allow_target_under) {
                        constraints.push(constraint!(w_under == 0.0));
                    } else {
                        constraints.push(constraint!(w_under <= 1.0));
                    }
                    if !(is_week_flex || allow_target_over) {
                        constraints.push(constraint!(w_over == 0.0));
                    } else {
                        constraints.push(constraint!(w_over <= 1.0));
                    }
                    constraints.push(constraint!(
                        week_sum.clone() + w_under.clone() >= plan.week_target as f64
                    ));
                    constraints.push(constraint!(
                        week_sum - w_over.clone() <= plan.week_target as f64
                    ));
                    objective += (w_under + w_over) * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                } else {
                    constraints.push(constraint!(week_sum == plan.week_target as f64));
                }
            }

            // 週4祝日非連動の月間所定日数一致確認（週次flexのみ選択された場合も月合計を維持、TargetDaysAdjust時のみ±1許容）
            let month_target = target_days_dict.get(s_name).copied().unwrap_or(0);
            let month_work_sum: Expression = ctx
                .clinic_days
                .iter()
                .map(|&d| x_vars[&(s_name.clone(), d)])
                .sum();
            if is_diagnose {
                let t_under = vars.add(variable().min(0.0).max(1.0));
                let t_over = vars.add(variable().min(0.0).max(1.0));
                constraints.push(constraint!(
                    month_work_sum.clone() + t_under >= month_target as f64
                ));
                constraints.push(constraint!(month_work_sum - t_over <= month_target as f64));
                if allow_target_under {
                    objective += t_under * 1.0;
                } else {
                    objective += t_under * DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64;
                }
                if allow_target_over {
                    objective += t_over * 1.0;
                } else {
                    objective += t_over * DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64;
                }
                diag_slacks
                    .as_mut()
                    .unwrap()
                    .target_adjust_slacks
                    .push((s_name.clone(), t_under, t_over));
            } else if allow_target_under || allow_target_over {
                let t_under = vars.add(variable().min(0.0));
                let t_over = vars.add(variable().min(0.0));
                if !allow_target_under {
                    constraints.push(constraint!(t_under == 0.0));
                } else {
                    constraints.push(constraint!(t_under <= 1.0));
                }
                if !allow_target_over {
                    constraints.push(constraint!(t_over == 0.0));
                } else {
                    constraints.push(constraint!(t_over <= 1.0));
                }
                constraints.push(constraint!(
                    month_work_sum.clone() + t_under.clone() >= month_target as f64
                ));
                constraints.push(constraint!(
                    month_work_sum - t_over.clone() <= month_target as f64
                ));
                objective += (t_under + t_over) * WEIGHT_APPROVED_RELAX_PENALTY as f64;
            } else {
                constraints.push(constraint!(month_work_sum == month_target as f64));
            }
            continue;
        }

        // 非コアスタッフ（パート：鈴木・後藤・岡本）
        if !staff.is_core {
            for &d in &ctx.all_days {
                let date = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                if w == 6
                    || ctx.is_holiday_map[&d]
                    || common_sp_vac.contains(&date)
                    || is_personal_leave_on_date(staff, &date, config)
                {
                    continue;
                }
                if staff.basic_off_days.contains(&w) {
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                } else {
                    // 希望出勤曜日の診療日は出勤必須（無断連勤スキップ廃止）
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 1.0));
                }
            }
            continue;
        }

        // 通常コアスタッフ（石橋さん以外の正社員・パート社員）
        let target = target_days_dict.get(s_name).copied().unwrap_or(0);
        let month_work_sum: Expression = ctx
            .clinic_days
            .iter()
            .map(|&d| x_vars[&(s_name.clone(), d)])
            .sum();

        let is_week4_fixed_one = staff.pattern.contains("週4")
            && staff.basic_off_days.len() == 1
            && staff.basic_off_type == OffType::And;
        let is_pure_fixed_and = staff.basic_off_type == OffType::And
            && !staff.basic_off_days.is_empty()
            && !is_week4_fixed_one;

        // 当月の稼働可能日数（日祝・共通休診・個人確定休暇を除いた診療日数）
        let workable_clinic_days_cnt = ctx
            .clinic_days
            .iter()
            .filter(|&&d| {
                let dt = ctx.date_map[&d];
                !common_sp_vac.contains(&dt) && !is_personal_leave_on_date(staff, &dt, config)
            })
            .count() as i32;
        let allowed_public_offs = (workable_clinic_days_cnt - target).max(0);

        if is_week4_fixed_one {
            let fix_w = staff.basic_off_days[0];
            let all_fixed_clinic_days: Vec<u32> = ctx
                .clinic_days
                .iter()
                .copied()
                .filter(|&d| {
                    let dt = ctx.date_map[&d];
                    ctx.weekday_map[&d] == fix_w
                        && !common_sp_vac.contains(&dt)
                        && !is_personal_leave_on_date(staff, &dt, config)
                })
                .collect();
            let unswapped_fixed_days: Vec<u32> = all_fixed_clinic_days
                .iter()
                .copied()
                .filter(|&d| {
                    let w_idx = day_to_week_idx[&d];
                    !approved.iter().any(|r| {
                        matches!(
                            r,
                            RelaxationKind::FixedOffDaySwap { staff_name, scope }
                            if staff_name == s_name && scope.matches(d, w_idx)
                        )
                    })
                })
                .collect();
            let swapped_cnt = (all_fixed_clinic_days.len() - unswapped_fixed_days.len()) as i32;

            if (all_fixed_clinic_days.len() as i32) <= allowed_public_offs {
                for &d in &unswapped_fixed_days {
                    if is_diagnose {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] - slack <= 0.0));
                        objective += slack * DIAG_WEIGHT_FIXED_OFF_SWAP as f64;
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .fixed_off_day_slacks
                            .push((s_name.clone(), d, slack));
                    } else {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    }
                }
            } else {
                // 固定曜日が月5回等ある場合は allowed_public_offs 回以上固定曜日を休み、超過分のみ出勤振替
                let req_fixed_offs = (allowed_public_offs - swapped_cnt)
                    .max(0)
                    .min(unswapped_fixed_days.len() as i32);
                let sum_fixed_offs: Expression = unswapped_fixed_days
                    .iter()
                    .map(|&d| Expression::from(1.0) - x_vars[&(s_name.clone(), d)])
                    .sum();
                if is_diagnose {
                    let agg_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_fixed_offs + agg_slack >= req_fixed_offs as f64
                    ));
                    objective += agg_slack * DIAG_WEIGHT_FIXED_OFF_SWAP as f64;
                    let day_x_vars: Vec<(u32, Variable)> = unswapped_fixed_days
                        .iter()
                        .map(|&d| (d, x_vars[&(s_name.clone(), d)]))
                        .collect();
                    diag_slacks
                        .as_mut()
                        .unwrap()
                        .fixed_off_agg_slacks
                        .push((s_name.clone(), agg_slack, day_x_vars));
                } else {
                    constraints.push(constraint!(sum_fixed_offs >= req_fixed_offs as f64));
                }
            }

            let candidates: Vec<u32> = (0..=4).filter(|&w| w != fix_w).collect();
            let y_sum: Expression = candidates.iter().map(|&w| y_vars[&(s_name.clone(), w)]).sum();
            constraints.push(constraint!(y_sum == 1.0));
            for w in 0..=5 {
                if !candidates.contains(&w) {
                    constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 0.0));
                }
            }

            if !is_diagnose {
                for week in &ctx.weeks {
                    let week_has_spec_off = config.specified_off_rule
                        && week
                            .days
                            .iter()
                            .any(|&d| staff.specified_off_dates.contains(&ctx.date_map[&d]));
                    if !week_has_spec_off {
                        for &d in &week.clinic_days {
                            let w = ctx.weekday_map[&d];
                            if candidates.contains(&w) {
                                let slide_slack = vars.add(variable().min(0.0));
                                constraints.push(constraint!(
                                    x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)]
                                        - slide_slack
                                        <= 1.0
                                ));
                                objective += slide_slack * WEIGHT_SECOND_OFF_SLIDE_SLACK as f64;
                            }
                        }
                    }
                }
            }
        } else if is_pure_fixed_and {
            // AND 固定公休スタッフ（小田切・長谷川・今川）
            for w in 0..=5 {
                if staff.basic_off_days.contains(&w) {
                    constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 1.0));
                } else {
                    constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 0.0));
                }
            }

            let all_fixed_clinic_days: Vec<u32> = ctx
                .clinic_days
                .iter()
                .copied()
                .filter(|&d| {
                    let dt = ctx.date_map[&d];
                    let w = ctx.weekday_map[&d];
                    staff.basic_off_days.contains(&w)
                        && !common_sp_vac.contains(&dt)
                        && !is_personal_leave_on_date(staff, &dt, config)
                })
                .collect();
            let unswapped_fixed_days: Vec<u32> = all_fixed_clinic_days
                .iter()
                .copied()
                .filter(|&d| {
                    let w_idx = day_to_week_idx[&d];
                    !approved.iter().any(|r| {
                        matches!(
                            r,
                            RelaxationKind::FixedOffDaySwap { staff_name, scope }
                            if staff_name == s_name && scope.matches(d, w_idx)
                        )
                    })
                })
                .collect();
            let swapped_cnt = (all_fixed_clinic_days.len() - unswapped_fixed_days.len()) as i32;

            if (all_fixed_clinic_days.len() as i32) <= allowed_public_offs {
                for &d in &unswapped_fixed_days {
                    if is_diagnose {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] - slack <= 0.0));
                        objective += slack * DIAG_WEIGHT_FIXED_OFF_SWAP as f64;
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .fixed_off_day_slacks
                            .push((s_name.clone(), d, slack));
                    } else {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    }
                }
            } else {
                // 固定公休曜日が月5回ある月（例: 2027年3月長谷川さん火曜5回）は、
                // 4回を公休とし1回のみ出勤へ振り替えることで共通所定勤務日数を厳守する
                let req_fixed_offs = (allowed_public_offs - swapped_cnt)
                    .max(0)
                    .min(unswapped_fixed_days.len() as i32);
                let sum_fixed_offs: Expression = unswapped_fixed_days
                    .iter()
                    .map(|&d| Expression::from(1.0) - x_vars[&(s_name.clone(), d)])
                    .sum();
                if is_diagnose {
                    let agg_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_fixed_offs + agg_slack >= req_fixed_offs as f64
                    ));
                    objective += agg_slack * DIAG_WEIGHT_FIXED_OFF_SWAP as f64;
                    let day_x_vars: Vec<(u32, Variable)> = unswapped_fixed_days
                        .iter()
                        .map(|&d| (d, x_vars[&(s_name.clone(), d)]))
                        .collect();
                    diag_slacks
                        .as_mut()
                        .unwrap()
                        .fixed_off_agg_slacks
                        .push((s_name.clone(), agg_slack, day_x_vars));
                } else {
                    constraints.push(constraint!(sum_fixed_offs >= req_fixed_offs as f64));
                }
            }
        } else {
            // OR タイプ (通常の主公休選択スタッフ、および空欄自動ローテーションスタッフ)
            let candidates: Vec<u32> = if staff.basic_off_days.is_empty() {
                vec![0, 1, 2, 3, 4]
            } else {
                staff.basic_off_days.clone()
            };
            let y_sum: Expression = candidates.iter().map(|&w| y_vars[&(s_name.clone(), w)]).sum();
            constraints.push(constraint!(y_sum == 1.0));
            for w in 0..=5 {
                if !candidates.contains(&w) {
                    constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 0.0));
                }
            }

            if !is_diagnose {
                for week in &ctx.weeks {
                    let week_has_spec_off = config.specified_off_rule
                        && week
                            .days
                            .iter()
                            .any(|&d| staff.specified_off_dates.contains(&ctx.date_map[&d]));
                    if !week_has_spec_off {
                        for &d in &week.clinic_days {
                            let w = ctx.weekday_map[&d];
                            if candidates.contains(&w) {
                                let slide_slack = vars.add(variable().min(0.0));
                                constraints.push(constraint!(
                                    x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)]
                                        - slide_slack
                                        <= 1.0
                                ));
                                objective += slide_slack * WEIGHT_MAIN_OFF_SLIDE_SLACK as f64;
                            }
                        }
                    }
                }
            }
        }

        // 月間所定勤務日数制約（v2.0.0: 全コア厳守。特別枠の週5発動および±1日調整は承認制）
        if staff.is_special {
            if is_diagnose {
                let mut extra_week_vars = Vec::new();
                for week in &ctx.weeks {
                    let active_c_len = week
                        .clinic_days
                        .iter()
                        .filter(|&&d| !common_sp_vac.contains(&ctx.date_map[&d]))
                        .count();
                    if active_c_len >= 5 {
                        let is_week_approved = approved.iter().any(|r| {
                            matches!(
                                r,
                                RelaxationKind::SpecialExtraWork { staff_name, scope }
                                if staff_name == s_name && scope.matches(week.clinic_days[0], week.week_index)
                            )
                        });
                        let wk_extra = vars.add(variable().min(0.0).max(1.0));
                        let wk_sum: Expression = week
                            .clinic_days
                            .iter()
                            .map(|&d| x_vars[&(s_name.clone(), d)])
                            .sum();
                        constraints.push(constraint!(wk_sum - wk_extra <= 4.0));
                        if is_week_approved {
                            objective += wk_extra * 1.0;
                        } else {
                            objective += wk_extra * DIAG_WEIGHT_SPECIAL_EXTRA_WORK as f64;
                            diag_slacks.as_mut().unwrap().special_extra_weeks.push((
                                s_name.clone(),
                                week.week_index,
                                wk_extra,
                            ));
                        }
                        extra_week_vars.push(wk_extra);
                    }
                }
                let extra_sum: Expression = extra_week_vars.iter().copied().sum();
                let t_under = vars.add(variable().min(0.0).max(1.0));
                let t_over = vars.add(variable().min(0.0).max(1.0));
                constraints.push(constraint!(
                    month_work_sum.clone() + t_under >= target as f64
                ));
                constraints.push(constraint!(
                    month_work_sum - extra_sum - t_over <= target as f64
                ));
                objective += t_under
                    * if allow_target_under {
                        1.0
                    } else {
                        DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64
                    };
                objective += t_over
                    * if allow_target_over {
                        1.0
                    } else {
                        DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64
                    };
                diag_slacks
                    .as_mut()
                    .unwrap()
                    .target_adjust_slacks
                    .push((s_name.clone(), t_under, t_over));
            } else {
                // Strict モード: 承認された週のみ週5許可
                let mut approved_week_count = 0i32;
                let mut month_approved = false;
                for r in approved {
                    if let RelaxationKind::SpecialExtraWork { staff_name, scope } = r {
                        if staff_name == s_name {
                            if matches!(scope, RelaxationScope::Month) {
                                month_approved = true;
                            } else {
                                approved_week_count += 1;
                            }
                        }
                    }
                }
                let max_extra = if month_approved {
                    5
                } else {
                    approved_week_count.min(5)
                };

                for week in &ctx.weeks {
                    let active_c_len = week
                        .clinic_days
                        .iter()
                        .filter(|&&d| !common_sp_vac.contains(&ctx.date_map[&d]))
                        .count();
                    if active_c_len >= 5 {
                        let week_approved = month_approved
                            || approved.iter().any(|r| {
                                matches!(
                                    r,
                                    RelaxationKind::SpecialExtraWork { staff_name, scope }
                                    if staff_name == s_name && matches!(scope, RelaxationScope::Week(wi) if *wi == week.week_index)
                                )
                            });
                        let wk_sum: Expression = week
                            .clinic_days
                            .iter()
                            .map(|&d| x_vars[&(s_name.clone(), d)])
                            .sum();
                        if week_approved {
                            constraints.push(constraint!(wk_sum <= 5.0));
                        } else {
                            constraints.push(constraint!(wk_sum <= 4.0));
                        }
                    }
                }

                let t_under = if allow_target_under {
                    let v = vars.add(variable().min(0.0).max(1.0));
                    objective += v * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                    Some(v)
                } else {
                    None
                };
                let extra_limit = max_extra + if allow_target_over { 1 } else { 0 };
                let extra_work = if extra_limit > 0 {
                    let v = vars.add(variable().min(0.0).max(extra_limit as f64));
                    objective += v * WEIGHT_SPECIAL_EXTRA_WORK as f64;
                    Some(v)
                } else {
                    None
                };

                match (t_under, extra_work) {
                    (None, None) => {
                        constraints.push(constraint!(month_work_sum == target as f64));
                    }
                    (Some(u), None) => {
                        constraints.push(constraint!(
                            month_work_sum.clone() + u >= target as f64
                        ));
                        constraints.push(constraint!(month_work_sum <= target as f64));
                    }
                    (None, Some(e)) => {
                        constraints.push(constraint!(month_work_sum.clone() >= target as f64));
                        constraints.push(constraint!(month_work_sum - e <= target as f64));
                    }
                    (Some(u), Some(e)) => {
                        constraints.push(constraint!(
                            month_work_sum.clone() + u >= target as f64
                        ));
                        constraints.push(constraint!(month_work_sum - e <= target as f64));
                    }
                }
            }
        } else if is_diagnose {
            let t_under = vars.add(variable().min(0.0).max(1.0));
            let t_over = vars.add(variable().min(0.0).max(1.0));
            constraints.push(constraint!(
                month_work_sum.clone() + t_under >= target as f64
            ));
            constraints.push(constraint!(month_work_sum - t_over <= target as f64));
            objective += t_under
                * if allow_target_under {
                    1.0
                } else {
                    DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64
                };
            objective += t_over
                * if allow_target_over {
                    1.0
                } else {
                    DIAG_WEIGHT_TARGET_DAYS_ADJUST as f64
                };
            diag_slacks
                .as_mut()
                .unwrap()
                .target_adjust_slacks
                .push((s_name.clone(), t_under, t_over));
        } else if allow_target_under || allow_target_over {
            let t_under = vars.add(variable().min(0.0));
            let t_over = vars.add(variable().min(0.0));
            if !allow_target_under {
                constraints.push(constraint!(t_under == 0.0));
            } else {
                constraints.push(constraint!(t_under <= 1.0));
            }
            if !allow_target_over {
                constraints.push(constraint!(t_over == 0.0));
            } else {
                constraints.push(constraint!(t_over <= 1.0));
            }
            constraints.push(constraint!(
                month_work_sum.clone() + t_under.clone() >= target as f64
            ));
            constraints.push(constraint!(
                month_work_sum - t_over.clone() <= target as f64
            ));
            objective += (t_under + t_over) * WEIGHT_APPROVED_RELAX_PENALTY as f64;
        } else {
            constraints.push(constraint!(month_work_sum == target as f64));
        }

        // 診断モード時はTier 2ソフト制約をスキップして純粋な最小緩和を探索
        if is_diagnose {
            continue;
        }

        // ====================================================================
        // Tier 2: 品質最適化ソフト制約（通常コアスタッフ向け）
        // ====================================================================

        // 5. 暦3連休抑制 (日曜・祝日含む連続3暦日、有休・特有・全休組は除外)
        let consec_weight = config.consecutive_off_weight();
        if consec_weight > 0 && ctx.days_in_month >= 3 {
            for d in 1..=(ctx.days_in_month - 2) {
                let d1 = d;
                let d2 = d + 1;
                let d3 = d + 2;
                let trio = [d1, d2, d3];

                let all_closed = trio.iter().all(|&day| {
                    let dt = ctx.date_map[&day];
                    ctx.weekday_map[&day] == 6
                        || ctx.is_holiday_map[&day]
                        || common_sp_vac.contains(&dt)
                });
                let has_vac_or_sp = trio.iter().any(|&day| {
                    let dt = ctx.date_map[&day];
                    staff.vacation_dates.contains(&dt) || staff.special_vacation_dates.contains(&dt)
                });
                if all_closed || has_vac_or_sp {
                    continue;
                }

                let o_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(
                    x_vars[&(s_name.clone(), d1)]
                        + x_vars[&(s_name.clone(), d2)]
                        + x_vars[&(s_name.clone(), d3)]
                        + o_slack
                        >= 1.0
                ));
                objective += o_slack * consec_weight as f64;
            }
        }

        // 6. 主公休不一致ペナルティ ＆ 過去3ヶ月ローテーション減衰ペナルティ
        for &d in &ctx.clinic_days {
            let date = ctx.date_map[&d];
            let w = ctx.weekday_map[&d];
            let is_exempt = common_sp_vac.contains(&date)
                || staff.vacation_dates.contains(&date)
                || staff.special_vacation_dates.contains(&date)
                || (config.specified_off_rule && staff.specified_off_dates.contains(&date))
                || (is_week4_fixed_one && staff.basic_off_days.contains(&w));
            if !is_exempt {
                let m_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(
                    x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)] + m_slack >= 1.0
                ));
                objective += m_slack * WEIGHT_MAIN_OFF_MISMATCH as f64;
            }
        }

        if !is_pure_fixed_and {
            let fallback_vec: Vec<u32> = data
                .prev_month_main_offs
                .get(s_name)
                .copied()
                .into_iter()
                .collect();
            let history: &[u32] = match data.prev_month_main_offs_history.get(s_name) {
                Some(h) if !h.is_empty() => h.as_slice(),
                _ => fallback_vec.as_slice(),
            };
            // v2.0.0: 週4固定1日の過剰膨張 (12M -> 4M) を是正
            let rot_weights: [i64; 3] = if is_week4_fixed_one {
                [4_000_000, 2_000_000, 1_000_000]
            } else {
                [1_200_000, 600_000, 300_000]
            };
            let mut seen_w = HashSet::new();
            for (idx, &prev_w) in history.iter().take(3).enumerate() {
                if prev_w <= 5 && seen_w.insert(prev_w) {
                    objective += y_vars[&(s_name.clone(), prev_w)] * rot_weights[idx] as f64;
                }
            }
        }

        // 7. 週単位の公休平準化・7日ローリング窓（祝日・有休除外版）・月末公休偏り抑制
        if config.weekly_off_balance_rule {
            let base_w5_c_off = (config.weekly_off_target - 1).max(0);
            for week in &ctx.weeks {
                let active_c_days: Vec<u32> = week
                    .clinic_days
                    .iter()
                    .copied()
                    .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
                    .collect();
                if active_c_days.len() < 5 {
                    continue;
                }

                let c_len = active_c_days.len() as f64;
                let sum_work_c: Expression = active_c_days
                    .iter()
                    .map(|&d| x_vars[&(s_name.clone(), d)])
                    .sum();

                let target_c_off = if staff.pattern.contains("週4") {
                    let fri_sat_cnt = active_c_days
                        .iter()
                        .filter(|&&d| matches!(ctx.weekday_map[&d], 4 | 5))
                        .count();
                    if fri_sat_cnt >= 2 {
                        (base_w5_c_off + 1) as f64
                    } else {
                        base_w5_c_off.max(1) as f64
                    }
                } else {
                    base_w5_c_off.max(1) as f64
                };

                let u_abs = vars.add(variable().min(0.0));
                let u_abs_expr = Expression::from(u_abs);
                constraints.push(constraint!(
                    u_abs_expr.clone() + sum_work_c.clone() >= c_len - target_c_off
                ));
                constraints.push(constraint!(
                    u_abs_expr - sum_work_c >= target_c_off - c_len
                ));
                objective += u_abs * WEIGHT_WEEKLY_OFF_DIFF as f64;
            }

            // 7b. 7暦日ローリングウィンドウ平準化（日祝・休診・個人有休・個人特有を除いた稼働可能日数から純粋公休を平準化）
            if ctx.days_in_month >= 7 {
                for start_d in 1..=(ctx.days_in_month - 6) {
                    let workable_days: Vec<u32> = (start_d..=start_d + 6)
                        .filter(|&d| {
                            let dt = ctx.date_map[&d];
                            ctx.weekday_map[&d] != 6
                                && !ctx.is_holiday_map[&d]
                                && !common_sp_vac.contains(&dt)
                                && !staff.vacation_dates.contains(&dt)
                                && !staff.special_vacation_dates.contains(&dt)
                        })
                        .collect();
                    let eff_days = workable_days.len() as i32;
                    if eff_days <= 2 {
                        continue;
                    }
                    let target_off = if staff.pattern.contains("週4") {
                        if eff_days >= 6 {
                            (config.weekly_off_target).max(2)
                        } else if eff_days >= 4 {
                            1
                        } else {
                            0
                        }
                    } else if eff_days >= 5 {
                        (config.weekly_off_target - 1).max(1)
                    } else {
                        0
                    };
                    let win_work: Expression = workable_days
                        .iter()
                        .map(|&d| x_vars[&(s_name.clone(), d)])
                        .sum();
                    let target_work = (eff_days - target_off) as f64;
                    let r_abs = vars.add(variable().min(0.0));
                    let r_abs_expr = Expression::from(r_abs);
                    constraints.push(constraint!(
                        r_abs_expr.clone() + win_work.clone() >= target_work
                    ));
                    constraints.push(constraint!(r_abs_expr - win_work >= -target_work));
                    objective += r_abs * WEIGHT_ROLLING_WINDOW_DIFF as f64;
                }
            }
        }

        // 7c. 月末3稼働日の公休集中抑制
        let tail_clinic_days: Vec<u32> = ctx
            .clinic_days
            .iter()
            .copied()
            .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
            .rev()
            .take(3)
            .collect();
        for d in tail_clinic_days {
            let date = ctx.date_map[&d];
            let w = ctx.weekday_map[&d];
            let is_forced_off = is_personal_leave_on_date(staff, &date, config)
                || (staff.basic_off_type == OffType::And && staff.basic_off_days.contains(&w));
            if !is_forced_off {
                objective += (Expression::from(1.0) - x_vars[&(s_name.clone(), d)])
                    * WEIGHT_TAIL_OFF_SUM as f64;
            }
        }

        // 8. 祝日週の公休優先配分ソフト目標（月間所定日数の枠内で合理的な週へ優先配分）
        if config.holiday_week_off_rule {
            for week in &ctx.weeks {
                if week.has_weekday_holiday && week.clinic_days.len() == 5 {
                    let week_work: Expression = week
                        .clinic_days
                        .iter()
                        .map(|&d| x_vars[&(s_name.clone(), d)])
                        .sum();
                    let sub_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(week_work - sub_slack <= 4.0));
                    objective += sub_slack * WEIGHT_HOLIDAY_WEEK_OFF_PREF as f64;
                }
            }
        }
    }

    // ========================================================================
    // 9. セクション別出勤人数・2階裏配置制約・早上がり重複制御
    // ========================================================================
    let staffs_1f: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| s.section == "1階" && s.pattern != "除外")
        .collect();
    let staffs_2f_front: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| s.section == "2階表" && s.is_core && s.pattern != "除外")
        .collect();
    let core_2f_back: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();
    let num_core_2f_b = core_2f_back.len();

    for &d in &ctx.clinic_days {
        let date = ctx.date_map[&d];
        if common_sp_vac.contains(&date) {
            continue;
        }
        let w = ctx.weekday_map[&d];
        let w_idx = day_to_week_idx.get(&d).copied().unwrap_or(0);

        // (A) 1階 & (B) 2階表: Tier 2 ソフト平準化・許容ブレ幅
        if !is_diagnose {
            let sum_1f: Expression = staffs_1f
                .iter()
                .map(|s| x_vars[&(s.name.clone(), d)])
                .sum();
            if let Some(&mean_1f) = ideal_mean.get("1階") {
                let m_round = mean_1f.round();
                let u_abs = vars.add(variable().min(0.0));
                let u_abs_expr = Expression::from(u_abs);
                constraints.push(constraint!(u_abs_expr.clone() + sum_1f.clone() >= m_round));
                constraints.push(constraint!(u_abs_expr.clone() - sum_1f.clone() >= -m_round));
                objective += u_abs * WEIGHT_SEC_BALANCE_ABS as f64;

                let max_dev_1f = staffs_1f.len().max(1);
                let q_dev_1f = vars.add(variable().min(0.0));
                let q_dev_1f_expr = Expression::from(q_dev_1f);
                for m in 1..=max_dev_1f {
                    let slope = (2 * m - 1) as f64;
                    let intercept = (m * (m - 1)) as f64;
                    constraints.push(constraint!(
                        q_dev_1f_expr.clone() - u_abs_expr.clone() * slope >= -intercept
                    ));
                }
                objective += q_dev_1f * WEIGHT_SEC_BALANCE_SQ as f64;

                let vac_1f_on_d = staffs_1f
                    .iter()
                    .filter(|s| is_personal_leave_on_date(s, &date, config))
                    .count();
                let min_1f = (mean_1f.floor()
                    - config.sec_capacity_tolerance as f64
                    - vac_1f_on_d as f64)
                    .max(0.0);
                let max_1f = mean_1f.ceil() + config.sec_capacity_tolerance as f64;
                let slack_1f_under = vars.add(variable().min(0.0));
                let slack_1f_over = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_1f.clone() + slack_1f_under >= min_1f));
                constraints.push(constraint!(sum_1f.clone() - slack_1f_over <= max_1f));
                objective += (slack_1f_under + slack_1f_over) * WEIGHT_GENERAL_SEC_SLACK as f64;

                if mean_1f >= 6.5 {
                    let b_under7 = vars.add(variable().binary());
                    constraints.push(constraint!(sum_1f + b_under7 * 7.0 >= 7.0));
                    objective += b_under7 * WEIGHT_1F_UNDER_7_PENALTY as f64;
                }
            }

            let sum_2f_front: Expression = staffs_2f_front
                .iter()
                .map(|s| x_vars[&(s.name.clone(), d)])
                .sum();
            if let Some(&mean_2f_f) = ideal_mean.get("2階表") {
                let m_round = mean_2f_f.round();
                let u_abs = vars.add(variable().min(0.0));
                let u_abs_expr = Expression::from(u_abs);
                constraints.push(constraint!(
                    u_abs_expr.clone() + sum_2f_front.clone() >= m_round
                ));
                constraints.push(constraint!(
                    u_abs_expr.clone() - sum_2f_front.clone() >= -m_round
                ));
                objective += u_abs * WEIGHT_SEC_BALANCE_ABS as f64;

                let max_dev_2f_f = staffs_2f_front.len().max(1);
                let q_dev_2f_f = vars.add(variable().min(0.0));
                let q_dev_2f_f_expr = Expression::from(q_dev_2f_f);
                for m in 1..=max_dev_2f_f {
                    let slope = (2 * m - 1) as f64;
                    let intercept = (m * (m - 1)) as f64;
                    constraints.push(constraint!(
                        q_dev_2f_f_expr.clone() - u_abs_expr.clone() * slope >= -intercept
                    ));
                }
                objective += q_dev_2f_f * WEIGHT_SEC_BALANCE_SQ as f64;

                let vac_2f_f_on_d = staffs_2f_front
                    .iter()
                    .filter(|s| is_personal_leave_on_date(s, &date, config))
                    .count();
                let min_2f_f = (mean_2f_f.floor()
                    - config.sec_capacity_tolerance as f64
                    - vac_2f_f_on_d as f64)
                    .max(0.0);
                let max_2f_f = mean_2f_f.ceil() + config.sec_capacity_tolerance as f64;
                let slack_2f_f_under = vars.add(variable().min(0.0));
                let slack_2f_f_over = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_2f_front.clone() + slack_2f_f_under >= min_2f_f));
                constraints.push(constraint!(sum_2f_front - slack_2f_f_over <= max_2f_f));
                objective +=
                    (slack_2f_f_under + slack_2f_f_over) * WEIGHT_GENERAL_SEC_SLACK as f64;
            }
        }

        // (C) 2階裏（外来）
        if num_core_2f_b > 0 {
            let is_ope = is_ope_date(&data.ope_dict, &date);
            let outpatient_2f_b: Vec<&Staff> = core_2f_back
                .iter()
                .copied()
                .filter(|s| !(is_ope && s.is_ope_dedicated))
                .collect();

            if !outpatient_2f_b.is_empty() {
                let sum_2f_back: Expression = outpatient_2f_b
                    .iter()
                    .map(|s| x_vars[&(s.name.clone(), d)])
                    .sum();
                let nurses_out: Vec<&Staff> = outpatient_2f_b
                    .iter()
                    .copied()
                    .filter(|s| s.is_nurse())
                    .collect();
                let exams_out: Vec<&Staff> = outpatient_2f_b
                    .iter()
                    .copied()
                    .filter(|s| s.is_exam())
                    .collect();
                let sum_nurses: Expression = nurses_out
                    .iter()
                    .map(|s| x_vars[&(s.name.clone(), d)])
                    .sum();
                let sum_exams: Expression = exams_out
                    .iter()
                    .map(|s| x_vars[&(s.name.clone(), d)])
                    .sum();

                let (req_total_min, req_nurse_min, req_exam_min) = compute_2f_back_day_requirements(
                    &core_2f_back,
                    &outpatient_2f_b,
                    date,
                    w,
                    is_ope,
                    config,
                );

                // Tier 1: 総人数・看護師・検査スタッフの厳格最低制約（無言クランプ・暗黙スラックなし）
                let eff_total = resolve_approved_2f_min(req_total_min, d, w_idx, approved, |r| {
                    if let RelaxationKind::Sec2fBackMinTotal { scope, relaxed_min } = r {
                        Some((scope, *relaxed_min))
                    } else {
                        None
                    }
                });
                let eff_nurse = resolve_approved_2f_min(req_nurse_min, d, w_idx, approved, |r| {
                    if let RelaxationKind::Sec2fBackMinNurse { scope, relaxed_min } = r {
                        Some((scope, *relaxed_min))
                    } else {
                        None
                    }
                });
                let eff_exam = resolve_approved_2f_min(req_exam_min, d, w_idx, approved, |r| {
                    if let RelaxationKind::Sec2fBackMinExam { scope, relaxed_min } = r {
                        Some((scope, *relaxed_min))
                    } else {
                        None
                    }
                });

                if is_diagnose {
                    if eff_total > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_2f_back.clone() + slack >= eff_total as f64
                        ));
                        objective += slack * DIAG_WEIGHT_2F_BACK_MIN as f64;
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .min_total_slacks
                            .push((d, eff_total, slack));
                    } else if req_total_min > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_2f_back.clone() + slack >= req_total_min as f64
                        ));
                        objective += slack * 1.0;
                    }

                    if eff_nurse > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_nurses.clone() + slack >= eff_nurse as f64
                        ));
                        objective += slack * DIAG_WEIGHT_2F_BACK_ROLE as f64;
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .min_nurse_slacks
                            .push((d, eff_nurse, slack));
                    } else if req_nurse_min > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_nurses.clone() + slack >= req_nurse_min as f64
                        ));
                        objective += slack * 1.0;
                    }

                    if eff_exam > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_exams.clone() + slack >= eff_exam as f64
                        ));
                        objective += slack * DIAG_WEIGHT_2F_BACK_ROLE as f64;
                        diag_slacks
                            .as_mut()
                            .unwrap()
                            .min_exam_slacks
                            .push((d, eff_exam, slack));
                    } else if req_exam_min > 0 {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_exams.clone() + slack >= req_exam_min as f64
                        ));
                        objective += slack * 1.0;
                    }
                    continue;
                }

                // Strict モード: Tier 1 ハード制約
                if eff_total > 0 {
                    constraints.push(constraint!(sum_2f_back.clone() >= eff_total as f64));
                }
                if eff_total < req_total_min {
                    let pref_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_2f_back.clone() + pref_slack >= req_total_min as f64
                    ));
                    objective += pref_slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                }

                if eff_nurse > 0 {
                    constraints.push(constraint!(sum_nurses.clone() >= eff_nurse as f64));
                }
                if eff_nurse < req_nurse_min {
                    let pref_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_nurses.clone() + pref_slack >= req_nurse_min as f64
                    ));
                    objective += pref_slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                }

                if eff_exam > 0 {
                    constraints.push(constraint!(sum_exams.clone() >= eff_exam as f64));
                }
                if eff_exam < req_exam_min {
                    let pref_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_exams.clone() + pref_slack >= req_exam_min as f64
                    ));
                    objective += pref_slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                }

                // Tier 2: 2階裏ソフト目標（軽い平準化＋5名超過抑制＋①Ope日推奨＞②早上がり補填）
                if let Some(&mean_2f_b) = ideal_mean.get("2階裏") {
                    let m_round = mean_2f_b.round();
                    let u_abs = vars.add(variable().min(0.0));
                    let u_abs_expr = Expression::from(u_abs);
                    constraints.push(constraint!(
                        u_abs_expr.clone() + sum_2f_back.clone() >= m_round
                    ));
                    constraints.push(constraint!(
                        u_abs_expr - sum_2f_back.clone() >= -m_round
                    ));
                    objective += u_abs * WEIGHT_2F_BACK_SOFT_BALANCE as f64;
                }

                // 5名以上ペナルティ
                if outpatient_2f_b.len() >= 5 {
                    let is_5plus = vars.add(variable().binary());
                    constraints.push(constraint!(
                        sum_2f_back.clone() - is_5plus * (outpatient_2f_b.len() as f64) <= 4.0
                    ));
                    objective += is_5plus * WEIGHT_2F_BACK_5_PENALTY as f64;
                }

                // ① Ope日の推奨体制（Ns>=2, 検査>=2）
                if is_ope {
                    if nurses_out.len() >= 2 {
                        let ope_n_rec_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_nurses.clone() + ope_n_rec_slack >= 2.0
                        ));
                        objective += ope_n_rec_slack * WEIGHT_OPE_2F_BACK_REC_PENALTY as f64;
                    }
                    if exams_out.len() >= 2 {
                        let ope_e_rec_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_exams.clone() + ope_e_rec_slack >= 2.0
                        ));
                        objective += ope_e_rec_slack * WEIGHT_OPE_2F_BACK_REC_PENALTY as f64;
                    }
                } else if config.nurse_rec_count > req_nurse_min && nurses_out.len() >= 2 {
                    let n_rec_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_nurses + n_rec_slack >= config.nurse_rec_count as f64
                    ));
                    objective += n_rec_slack * WEIGHT_NURSE_LT2_PENALTY as f64;
                }

                // ② 非Ope日の早上がり重複回避 ＆ フルタイム増員 (is_early_leave 属性ベース)
                if !is_ope {
                    let early_staffs: Vec<&Staff> = outpatient_2f_b
                        .iter()
                        .copied()
                        .filter(|s| s.is_early_leave)
                        .collect();
                    if early_staffs.len() >= 2 {
                        let sum_early: Expression = early_staffs
                            .iter()
                            .map(|s| x_vars[&(s.name.clone(), d)])
                            .sum();
                        let overlap_flag = vars.add(variable().binary());
                        // sum_early >= 2 のとき overlap_flag = 1
                        let cap_diff = (early_staffs.len() - 1) as f64;
                        constraints.push(constraint!(
                            sum_early - overlap_flag * cap_diff <= 1.0
                        ));
                        objective += overlap_flag * WEIGHT_EARLY_OVERLAP_PENALTY as f64;

                        let fulltime_out: Vec<&Staff> = outpatient_2f_b
                            .iter()
                            .copied()
                            .filter(|s| !s.is_early_leave)
                            .collect();
                        let req_ft = (config.sec_2f_back_base_cap - 1)
                            .max(2)
                            .min(fulltime_out.len() as i32);
                        if req_ft > 0 {
                            let sum_ft: Expression = fulltime_out
                                .iter()
                                .map(|s| x_vars[&(s.name.clone(), d)])
                                .sum();
                            let slack_ft = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                sum_ft + slack_ft - overlap_flag * (req_ft as f64) >= 0.0
                            ));
                            objective += slack_ft * WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK as f64;
                        }
                    }
                }
            }
        }
    }

    // (C)-2 ③ 2階裏 通常週（祝日・共通休診を含まない6稼働日の週）の4名体制最低日数制約
    if !is_diagnose && num_core_2f_b > 0 && config.sec_2f_back_weekly_4_min > 0 {
        for week in &ctx.weeks {
            let active_clinic_days: Vec<u32> = week
                .clinic_days
                .iter()
                .copied()
                .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
                .collect();
            if !week.has_holiday && active_clinic_days.len() == 6 {
                let mut is_4plus_sum = Expression::from(0.0);
                for &d in &active_clinic_days {
                    let dt = ctx.date_map[&d];
                    let is_ope = is_ope_date(&data.ope_dict, &dt);
                    let is_4plus = vars.add(variable().binary());
                    let sum_day: Expression = core_2f_back
                        .iter()
                        .filter(|s| !(is_ope && s.is_ope_dedicated))
                        .map(|s| x_vars[&(s.name.clone(), d)])
                        .sum();
                    constraints.push(constraint!(sum_day - is_4plus * 4.0 >= 0.0));
                    is_4plus_sum += is_4plus;
                }
                let slack_4 = vars.add(variable().min(0.0));
                constraints.push(constraint!(
                    is_4plus_sum + slack_4 >= config.sec_2f_back_weekly_4_min as f64
                ));
                objective += slack_4 * WEIGHT_2F_BACK_WEEKLY_4_SLACK as f64;
            }
        }
    }

    // ========================================================================
    // 10. シフト制約条件シート (data.rules) の各行適用（バグ修正＆厳格化）
    // ========================================================================
    for (r_idx, rule) in data.rules.iter().enumerate() {
        if rule.is_invalid_condition {
            continue;
        }
        let target_days = parse_rule_target_dates(&rule.condition_desc, ctx, &data.ope_dict);
        if target_days.is_empty() {
            continue;
        }

        let target_staff_refs: Vec<&Staff> = rule
            .target_staffs
            .iter()
            .filter_map(|name| data.staffs.iter().find(|s| &s.name == name && s.pattern != "除外"))
            .collect();
        let valid_count = target_staff_refs.len() as i32;
        if valid_count == 0 {
            continue;
        }

        // v2.0.0 バグ修正: 「同時休み不可」の val_min そのまま適用 ＆ 除外時は有効人数上限クランプのみ
        let eff_min = rule.val_min.min(valid_count).max(0);
        let eff_rec = rule.val_rec.min(valid_count).max(0);
        let is_hard = rule.level.to_uppercase().contains("HARD");
        let weight = get_level_weight(&rule.level) as f64;
        let rec_weight = get_rec_weight(&rule.level) as f64;
        let rule_label = format!(
            "{} / {} ({})",
            rule.name,
            rule.condition_desc,
            rule.target_staffs.join(",")
        );

        for &d in &target_days {
            let date = ctx.date_map[&d];
            if common_sp_vac.contains(&date) {
                continue;
            }
            let w_idx = day_to_week_idx.get(&d).copied().unwrap_or(0);
            let sum_target: Expression = target_staff_refs
                .iter()
                .map(|s| x_vars[&(s.name.clone(), d)])
                .sum();

            let is_rule_relaxed = approved.iter().any(|r| {
                matches!(
                    r,
                    RelaxationKind::SheetHardRule { rule_index, scope, .. }
                    if *rule_index == r_idx && scope.matches(d, w_idx)
                )
            });

            if eff_min > 0 {
                if is_hard {
                    if is_diagnose {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_target.clone() + slack >= eff_min as f64
                        ));
                        if is_rule_relaxed {
                            objective += slack * 1.0;
                        } else {
                            let diag_w = diag_sheet_hard_weight(&rule.level);
                            objective += slack * diag_w as f64;
                            diag_slacks.as_mut().unwrap().sheet_hard_slacks.push((
                                r_idx,
                                rule_label.clone(),
                                d,
                                eff_min,
                                slack,
                            ));
                        }
                    } else if is_rule_relaxed {
                        let slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_target.clone() + slack >= eff_min as f64
                        ));
                        objective += slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                    } else {
                        constraints.push(constraint!(sum_target.clone() >= eff_min as f64));
                    }
                } else if !is_diagnose {
                    let slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_target.clone() + slack >= eff_min as f64));
                    objective += slack * weight;
                }
            }

            if !is_diagnose && eff_rec > eff_min {
                let rec_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_target.clone() + rec_slack >= eff_rec as f64));
                objective += rec_slack * rec_weight;
            }

            if rule.val_max < 90 {
                if is_hard {
                    if is_diagnose {
                        let max_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_target - max_slack <= rule.val_max as f64
                        ));
                        if is_rule_relaxed {
                            objective += max_slack * 1.0;
                        } else {
                            objective += max_slack * diag_sheet_hard_weight(&rule.level) as f64;
                            diag_slacks.as_mut().unwrap().sheet_hard_slacks.push((
                                r_idx,
                                rule_label.clone(),
                                d,
                                rule.val_max,
                                max_slack,
                            ));
                        }
                    } else if is_rule_relaxed {
                        let max_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_target - max_slack <= rule.val_max as f64
                        ));
                        objective += max_slack * WEIGHT_APPROVED_RELAX_PENALTY as f64;
                    } else {
                        constraints.push(constraint!(sum_target <= rule.val_max as f64));
                    }
                } else if !is_diagnose {
                    let max_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_target - max_slack <= rule.val_max as f64));
                    objective += max_slack * weight;
                }
            }
        }
    }

    // ========================================================================
    // 11. 既存シフト維持ペナルティ (Tier 2)
    // ========================================================================
    if !is_diagnose {
        if let Some(existing_month_shifts) = data.existing_shifts.get(&(ctx.year, ctx.month)) {
            let weight_keep = config.existing_shift_weight() as f64;
            for staff in &data.staffs {
                if let Some(staff_shifts) = existing_month_shifts.get(&staff.name) {
                    for &d in &ctx.clinic_days {
                        let date = ctx.date_map[&d];
                        if common_sp_vac.contains(&date)
                            || is_personal_leave_on_date(staff, &date, config)
                        {
                            continue;
                        }
                        if let Some(&prev_val) = staff_shifts.get(&d) {
                            let var = x_vars[&(staff.name.clone(), d)];
                            if prev_val == 1 {
                                objective += (Expression::from(1.0) - var) * weight_keep;
                            } else {
                                objective += var * weight_keep;
                            }
                        }
                    }
                }
            }
        }
    }

    // ========================================================================
    // 12. ソルバーの構築と実行 (HiGHS)
    // ========================================================================
    logs.push(format!(
        "{}: 制約数={}",
        if is_diagnose { "診断モデル" } else { "厳格モデル" },
        constraints.len()
    ));

    let mut model = vars.minimise(objective).using(default_solver);
    model = model.set_option("time_limit", SOLVER_TIME_LIMIT_SECS);
    for c in constraints {
        model.add_constraint(c);
    }

    let solve_start = Instant::now();
    let solution_res = model.solve();
    let solve_duration = solve_start.elapsed().as_secs_f64();

    match solution_res {
        Ok(solution) => {
            let status = match solution.status() {
                SolutionStatus::Optimal => SolveStatus::Optimal,
                SolutionStatus::TimeLimit | SolutionStatus::GapLimit => SolveStatus::Feasible,
            };
            logs.push(format!(
                "ソルバー完了 ({:?}, {:.2}秒)",
                status, solve_duration
            ));

            let mut shifts: HashMap<String, HashMap<u32, u8>> = HashMap::new();
            let mut main_offs: HashMap<String, u32> = HashMap::new();

            for staff in &data.staffs {
                let s_name = &staff.name;
                let mut day_map = HashMap::new();
                for &d in &ctx.all_days {
                    let val = solution.value(x_vars[&(s_name.clone(), d)]);
                    day_map.insert(d, if val > 0.5 { 1 } else { 0 });
                }
                shifts.insert(s_name.clone(), day_map);

                let is_week4_fixed_one = staff.pattern.contains("週4")
                    && staff.basic_off_days.len() == 1
                    && staff.basic_off_type == OffType::And;
                let is_pure_fixed_and = staff.basic_off_type == OffType::And
                    && !staff.basic_off_days.is_empty()
                    && !is_week4_fixed_one
                    && !staff.is_holiday_independent_week4;

                if (staff.is_core || staff.is_holiday_independent_week4 || staff.is_ope_dedicated)
                    && staff.pattern != "除外"
                    && !is_pure_fixed_and
                {
                    let mut best_w = 0;
                    let mut max_val = -1.0;
                    for w in 0..=5 {
                        let val = solution.value(y_vars[&(s_name.clone(), w)]);
                        if val > max_val {
                            max_val = val;
                            best_w = w;
                        }
                    }
                    if max_val > 0.5 {
                        main_offs.insert(s_name.clone(), best_w);
                    }
                }
            }

            // 診断モードの場合はスラック変数値を shifts 代わりに？ 別途診断関数で評価するため diag_slacks 値の抽出をここで行ってよいが、
            // ここでは Solution がドロップされる前に diag_slacks から非ゼロスラックを取り出すため、
            // `shifts` のほかに診断解析用補助情報を logs には入れず、呼び出し元が使えるよう
            // 特殊設計として Diagnose 時は solution から取り出した結果を別関数に返すか、ここで抽出する。
            // → クリーンにするため `extract_diag_proposals` 用のローカルマップではなく Solution 自体は返さず、
            //   診断時は下の専用関数 `solve_diagnosis_model` を使う構成も可。ただし build_and_solve_model 内で
            //   diag_slacks の実測値を Solution から評価して `diagnosis` に格納してしまえば一発！
            let diagnosis_from_diag = if is_diagnose {
                diag_slacks
                    .as_ref()
                    .map(|slacks| evaluate_diag_solution(data, ctx, slacks, &solution, approved))
            } else {
                None
            };

            (
                SolveResult {
                    success: true,
                    status,
                    stage: 1,
                    shifts,
                    main_offs,
                    target_days: target_days_dict.clone(),
                    execution_time_secs: solve_duration,
                    logs,
                    approved_relaxations: Vec::new(),
                    diagnosis: diagnosis_from_diag,
                    audit: ShiftAuditReport::default(),
                },
                diag_slacks,
            )
        }
        Err(e) => {
            let status = match &e {
                ResolutionError::Infeasible => SolveStatus::Infeasible,
                ResolutionError::Other(msg) if *msg == "NoSolutionFound" => SolveStatus::Timeout,
                _ => SolveStatus::Error,
            };
            logs.push(format!("ソルバー終了ステータス: {:?} ({e})", status));
            (
                SolveResult {
                    success: false,
                    status,
                    stage: 0,
                    shifts: HashMap::new(),
                    main_offs: HashMap::new(),
                    target_days: target_days_dict.clone(),
                    execution_time_secs: solve_duration,
                    logs,
                    approved_relaxations: Vec::new(),
                    diagnosis: None,
                    audit: ShiftAuditReport::default(),
                },
                diag_slacks,
            )
        }
    }
}

/// 診断用MILPでのシートHARD制約スラック重み（特別枠15M/水曜公休20Mより重く、所定変更200Mより軽い）
fn diag_sheet_hard_weight(level: &str) -> i64 {
    let norm = level.trim().to_uppercase();
    if norm.contains("HARD1") {
        75_000_000
    } else if norm.contains("HARD3") {
        42_000_000
    } else {
        55_000_000
    }
}

/// 診断用MILPの解から最小緩和候補を抽出（推奨ピンポイント候補 + 代替/一括候補）
fn evaluate_diag_solution(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    slacks: &DiagnosticSlacks,
    solution: &impl Solution,
    approved: &HashSet<RelaxationKind>,
) -> DiagnosisResult {
    let mut proposals: Vec<RelaxationProposal> = Vec::new();
    let mut seen_kinds: HashSet<RelaxationKind> = HashSet::new();

    let mut push_prop = |kind: RelaxationKind, label: String, impact_desc: String, recommended: bool, is_last_resort: bool| {
        if seen_kinds.insert(kind.clone()) {
            proposals.push(RelaxationProposal {
                kind,
                label,
                impact_desc,
                recommended,
                is_last_resort,
            });
        }
    };

    // 1. 特別枠（堀さん）の週5許可
    for (s_name, w_idx, var) in &slacks.special_extra_weeks {
        if solution.value(*var) > 0.5 {
            let kind = RelaxationKind::SpecialExtraWork {
                staff_name: s_name.clone(),
                scope: RelaxationScope::Week(*w_idx),
            };
            push_prop(
                kind,
                format!("{} の {} 週5出勤を許可", s_name, format_week_label(ctx, *w_idx)),
                "この週のみ週4→週5日出勤を許可し、人員不足を補います（月間所定も+1日）".to_string(),
                true,
                false,
            );
        }
    }

    // 2. 条件付水曜公休の振替
    for (s_name, d, var) in &slacks.wed_off_slacks {
        if solution.value(*var) > 0.5 {
            let kind = RelaxationKind::ConditionalWedOff {
                staff_name: s_name.clone(),
                scope: RelaxationScope::Day(*d),
            };
            push_prop(
                kind,
                format!("{} の {} 非Ope水曜公休を別曜日へ振替", s_name, format_day_label(ctx, *d)),
                "水曜に出勤させ、同週内の別平日に公休を振り替えます（週4日は維持）".to_string(),
                true,
                false,
            );
        }
    }

    // 3. 2階裏 検査・看護師・総人数最低ラインのピンポイント緩和
    let mut exam_relaxed_days: Vec<(u32, i32, i32)> = Vec::new();
    for (d, orig_req, var) in &slacks.min_exam_slacks {
        let sv = solution.value(*var).round() as i32;
        if sv > 0 {
            let new_min = (*orig_req - sv).max(0);
            exam_relaxed_days.push((*d, *orig_req, new_min));
            let kind = RelaxationKind::Sec2fBackMinExam {
                scope: RelaxationScope::Day(*d),
                relaxed_min: new_min,
            };
            push_prop(
                kind,
                format!("{} 2階裏・検査最低人数を {}名→{}名 へ緩和", format_day_label(ctx, *d), orig_req, new_min),
                "該当日の2階裏検査スタッフ最低人数の下限を引き下げます".to_string(),
                true,
                false,
            );
        }
    }

    let mut nurse_relaxed_days: Vec<(u32, i32, i32)> = Vec::new();
    for (d, orig_req, var) in &slacks.min_nurse_slacks {
        let sv = solution.value(*var).round() as i32;
        if sv > 0 {
            let new_min = (*orig_req - sv).max(0);
            nurse_relaxed_days.push((*d, *orig_req, new_min));
            let kind = RelaxationKind::Sec2fBackMinNurse {
                scope: RelaxationScope::Day(*d),
                relaxed_min: new_min,
            };
            push_prop(
                kind,
                format!("{} 2階裏・看護師最低人数を {}名→{}名 へ緩和", format_day_label(ctx, *d), orig_req, new_min),
                "該当日の2階裏看護師最低人数の下限を引き下げます".to_string(),
                true,
                false,
            );
        }
    }

    let mut total_relaxed_days: Vec<(u32, i32, i32)> = Vec::new();
    for (d, orig_req, var) in &slacks.min_total_slacks {
        let sv = solution.value(*var).round() as i32;
        if sv > 0 {
            let new_min = (*orig_req - sv).max(0);
            total_relaxed_days.push((*d, *orig_req, new_min));
            let kind = RelaxationKind::Sec2fBackMinTotal {
                scope: RelaxationScope::Day(*d),
                relaxed_min: new_min,
            };
            push_prop(
                kind,
                format!("{} 2階裏・外来最低総人数を {}名→{}名 へ緩和", format_day_label(ctx, *d), orig_req, new_min),
                "該当日の2階裏外来総人数の下限を引き下げます".to_string(),
                true,
                false,
            );
        }
    }

    // 4. シフト制約条件シート HARD ルール緩和
    for (r_idx, rule_label, d, _orig_req, var) in &slacks.sheet_hard_slacks {
        let sv = solution.value(*var).round() as i32;
        if sv > 0 {
            let kind = RelaxationKind::SheetHardRule {
                rule_index: *r_idx,
                rule_label: rule_label.clone(),
                scope: RelaxationScope::Day(*d),
            };
            push_prop(
                kind,
                format!("{} 制約『{}』の最低人数を緩和", format_day_label(ctx, *d), rule_label),
                "該当日の制約シートHARD条件を最小限緩和します".to_string(),
                true,
                false,
            );
        }
    }

    // 5. 週次目標柔軟化（週4祝日非連動）
    for (s_name, w_idx, u_var, o_var) in &slacks.weekly_flex_slacks {
        if solution.value(*u_var) > 0.5 || solution.value(*o_var) > 0.5 {
            let kind = RelaxationKind::WeeklyTargetFlex {
                staff_name: s_name.clone(),
                week_index: *w_idx,
            };
            push_prop(
                kind,
                format!("{} の {} 週次出勤目標を柔軟化(±1日)", s_name, format_week_label(ctx, *w_idx)),
                "該当週の週4日目標を±1日柔軟化します".to_string(),
                true,
                false,
            );
        }
    }

    // 6. 固定公休曜日の振替
    for (s_name, d, var) in &slacks.fixed_off_day_slacks {
        if solution.value(*var) > 0.5 {
            let kind = RelaxationKind::FixedOffDaySwap {
                staff_name: s_name.clone(),
                scope: RelaxationScope::Day(*d),
            };
            push_prop(
                kind,
                format!("{} の {} 固定公休を別日へ振替", s_name, format_day_label(ctx, *d)),
                "該当日の固定公休を出勤に振り替え、同月内の別日に公休を取得します（所定日数は維持）".to_string(),
                true,
                false,
            );
        }
    }
    for (s_name, agg_var, day_x_vars) in &slacks.fixed_off_agg_slacks {
        let agg_val = solution.value(*agg_var).round() as usize;
        if agg_val > 0 {
            let mut worked_days: Vec<u32> = day_x_vars
                .iter()
                .filter_map(|&(d, x_v)| if solution.value(x_v) > 0.5 { Some(d) } else { None })
                .collect();
            worked_days.sort_unstable();
            for d in worked_days.into_iter().take(agg_val) {
                let kind = RelaxationKind::FixedOffDaySwap {
                    staff_name: s_name.clone(),
                    scope: RelaxationScope::Day(d),
                };
                push_prop(
                    kind,
                    format!("{} の {} 固定公休を別日へ振替", s_name, format_day_label(ctx, d)),
                    "該当日の固定公休を出勤に振り替え、同月内の別日に公休を取得します（所定日数は維持）".to_string(),
                    true,
                    false,
                );
            }
        }
    }

    // 7. Ope専従者のOpe/注射日必須出勤免除
    for (s_name, d, var) in &slacks.ope_ded_slacks {
        if solution.value(*var) > 0.5 {
            let kind = RelaxationKind::OpeDedicatedWork {
                staff_name: s_name.clone(),
                scope: RelaxationScope::Day(*d),
            };
            push_prop(
                kind,
                format!("{} の {} Ope/注射必須出勤を免除", s_name, format_day_label(ctx, *d)),
                "連勤上限や休暇との競合により該当日のOpe/注射必須出勤を免除します".to_string(),
                true,
                false,
            );
        }
    }

    // 8. 最終手段：所定勤務日数の ±1日調整
    for (s_name, u_var, o_var) in &slacks.target_adjust_slacks {
        let u = solution.value(*u_var) > 0.5;
        let o = solution.value(*o_var) > 0.5;
        if u || o {
            let delta = if u && !o { -1 } else if o && !u { 1 } else { 0 };
            let kind = RelaxationKind::TargetDaysAdjust {
                staff_name: Some(s_name.clone()),
                delta,
            };
            let d_str = if delta < 0 { "-1日" } else if delta > 0 { "+1日" } else { "±1日" };
            push_prop(
                kind,
                format!("【最終手段】{} の所定勤務日数を {} 調整", s_name, d_str),
                "やむを得ない場合のみ当月の月間所定勤務日数を1日調整します".to_string(),
                true,
                true,
            );
        }
    }

    // 既に承認されていた項目がなお必要な場合も候補として引き継げるようにする
    for r in approved {
        let label = format_relaxation_kind(r, ctx);
        let is_last = matches!(r, RelaxationKind::TargetDaysAdjust { .. });
        push_prop(
            r.clone(),
            label,
            "既に選択された緩和条件です".to_string(),
            true,
            is_last,
        );
    }

    // 週単位・月単位の一括代替候補および代替スタッフ候補（火曜検査0名時の長谷川振替等）を追加
    let day_to_w: HashMap<u32, usize> = ctx
        .weeks
        .iter()
        .flat_map(|w| w.days.iter().map(move |&d| (d, w.week_index)))
        .collect();

    if exam_relaxed_days.len() >= 2 {
        let mut by_week: HashMap<usize, i32> = HashMap::new();
        for &(d, _, new_m) in &exam_relaxed_days {
            if let Some(&wi) = day_to_w.get(&d) {
                by_week
                    .entry(wi)
                    .and_modify(|m| *m = (*m).min(new_m))
                    .or_insert(new_m);
            }
        }
        for (wi, min_m) in by_week {
            push_prop(
                RelaxationKind::Sec2fBackMinExam {
                    scope: RelaxationScope::Week(wi),
                    relaxed_min: min_m,
                },
                format!("【一括】{} の2階裏・検査最低人数を {}名 へ緩和", format_week_label(ctx, wi), min_m),
                "該当週の必要日のみ最小限に検査最低人数を緩和します".to_string(),
                false,
                false,
            );
        }
    }

    if total_relaxed_days.len() >= 2 {
        let mut by_week: HashMap<usize, i32> = HashMap::new();
        for &(d, _, new_m) in &total_relaxed_days {
            if let Some(&wi) = day_to_w.get(&d) {
                by_week
                    .entry(wi)
                    .and_modify(|m| *m = (*m).min(new_m))
                    .or_insert(new_m);
            }
        }
        for (wi, min_m) in by_week {
            push_prop(
                RelaxationKind::Sec2fBackMinTotal {
                    scope: RelaxationScope::Week(wi),
                    relaxed_min: min_m,
                },
                format!("【一括】{} の2階裏・外来最低総人数を {}名 へ緩和", format_week_label(ctx, wi), min_m),
                "該当週の必要日のみ最小限に総人数最低ラインを緩和します".to_string(),
                false,
                false,
            );
        }
    }

    // 代替案：検査0名の日が固定公休曜日（火曜等）と重なっている場合、固定公休振替案も代替提示
    for &(d, _, new_m) in &exam_relaxed_days {
        if new_m == 0 {
            let w = ctx.weekday_map[&d];
            let dt = ctx.date_map[&d];
            for s in &data.staffs {
                if s.section == "2階裏"
                    && s.is_exam()
                    && s.basic_off_type == OffType::And
                    && s.basic_off_days.contains(&w)
                    && !is_personal_leave_on_date(s, &dt, &data.config)
                {
                    push_prop(
                        RelaxationKind::FixedOffDaySwap {
                            staff_name: s.name.clone(),
                            scope: RelaxationScope::Day(d),
                        },
                        format!("【代替】{} の {} 固定公休を別日へ振替", s.name, format_day_label(ctx, d)),
                        "検査0名を避けるため、固定公休を別日と入れ替えて出勤させます".to_string(),
                        false,
                        false,
                    );
                }
            }
        }
    }

    // 代替案：月一括（特別枠週5）
    if !slacks.special_extra_weeks.is_empty() {
        let sp_names: HashSet<String> = slacks
            .special_extra_weeks
            .iter()
            .filter(|(_, _, v)| solution.value(*v) > 0.5)
            .map(|(n, _, _)| n.clone())
            .collect();
        for s_name in sp_names {
            push_prop(
                RelaxationKind::SpecialExtraWork {
                    staff_name: s_name.clone(),
                    scope: RelaxationScope::Month,
                },
                format!("【一括】{} の当月全体の必要週で週5出勤を許可", s_name),
                "当月内で不足する週のみ必要最小限で週5出勤を許可します".to_string(),
                false,
                false,
            );
        }
    }

    // 推奨・非最終手段を先頭へ並べる
    proposals.sort_by_key(|p| (p.is_last_resort, !p.recommended));

    DiagnosisResult {
        tier0_unsolvable: false,
        summary: String::new(),
        causes: Vec::new(),
        proposals,
    }
}

/// 適切解なしの構造的原因分析 ＋ 診断用MILPによる最小緩和候補探索
fn diagnose_infeasibility(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    target_days_dict: &HashMap<String, i32>,
    ideal_mean: &HashMap<String, f64>,
    holidays: &HashMap<NaiveDate, String>,
    approved: &HashSet<RelaxationKind>,
) -> DiagnosisResult {
    let mut causes: Vec<ConflictCause> = Vec::new();
    let common_sp_vac = compute_common_sp_vac(data, ctx);
    let config = &data.config;
    let max_k = config.max_consecutive_work_days;

    // 1. 個人別の Tier 0 物理上限チェック（日祝・休診・確定休暇・連勤上限）
    let mut has_tier0_individual_overload = false;
    for staff in &data.staffs {
        if staff.pattern == "除外" {
            continue;
        }
        let target = target_days_dict.get(&staff.name).copied().unwrap_or(0);
        let tier0_max = compute_tier0_max_workable_days(staff, data, ctx, &common_sp_vac);
        if target > tier0_max {
            if !staff.is_core {
                has_tier0_individual_overload = true;
                causes.push(ConflictCause {
                    category: "パート連勤上限競合 (緩和不可)".to_string(),
                    scope_label: staff.name.clone(),
                    detail: format!(
                        "希望出勤曜日の出勤日数({target}日)が連勤上限({max_k}日)等と競合しています（Tier 0上限: {tier0_max}日）。基本公休曜日を見直してください。"
                    ),
                });
            } else {
                causes.push(ConflictCause {
                    category: "個人出勤上限不足".to_string(),
                    scope_label: staff.name.clone(),
                    detail: format!(
                        "希望休・確定休暇・連勤上限を考慮した最大出勤可能日数({tier0_max}日)が所定勤務日数({target}日)を下回っています。"
                    ),
                });
            }
        }
    }

    // 2. 2階裏（外来）の日次・週次供給チェック
    let core_2f_back: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();

    if !core_2f_back.is_empty() {
        let mut is_strict_avail_outpatient = HashMap::<(String, u32), bool>::new();
        for &s in &core_2f_back {
            let tail = extract_prev_tail_for_staff(s, data, ctx);
            let tr_ones = tail.iter().rev().take_while(|&&v| v == 1).count();
            let plans = if s.is_holiday_independent_week4 {
                Some(compute_holiday_independent_week4_plans(s, data, ctx, holidays))
            } else {
                None
            };

            for &d in &ctx.clinic_days {
                let dt = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                let is_ope = is_ope_date(&data.ope_dict, &dt);
                if common_sp_vac.contains(&dt)
                    || is_personal_leave_on_date(s, &dt, config)
                    || (d == 1 && max_k > 0 && tr_ones >= max_k)
                    || (is_ope && s.is_ope_dedicated)
                {
                    is_strict_avail_outpatient.insert((s.name.clone(), d), false);
                    continue;
                }
                let blocked_fixed = if s.is_holiday_independent_week4 {
                    let is_inj = is_injection_date(&data.ope_dict, &dt);
                    let wed_off = plans
                        .as_ref()
                        .and_then(|ps| ps.iter().find(|p| p.wed_day == Some(d) && p.wed_should_be_off))
                        .is_some();
                    (s.basic_off_days.contains(&w) && !is_inj) || wed_off
                } else {
                    s.basic_off_type == OffType::And && s.basic_off_days.contains(&w)
                };
                is_strict_avail_outpatient.insert((s.name.clone(), d), !blocked_fixed);
            }
        }

        for &d in &ctx.clinic_days {
            let dt = ctx.date_map[&d];
            if common_sp_vac.contains(&dt) {
                continue;
            }
            let w = ctx.weekday_map[&d];
            let is_ope = is_ope_date(&data.ope_dict, &dt);
            let outpatient: Vec<&Staff> = core_2f_back
                .iter()
                .copied()
                .filter(|s| !(is_ope && s.is_ope_dedicated))
                .collect();
            let (req_t, req_n, req_e) =
                compute_2f_back_day_requirements(&core_2f_back, &outpatient, dt, w, is_ope, config);

            let avail_t = outpatient
                .iter()
                .filter(|s| is_strict_avail_outpatient.get(&(s.name.clone(), d)).copied().unwrap_or(false))
                .count() as i32;
            let avail_n = outpatient
                .iter()
                .filter(|s| {
                    s.is_nurse()
                        && is_strict_avail_outpatient.get(&(s.name.clone(), d)).copied().unwrap_or(false)
                })
                .count() as i32;
            let avail_e = outpatient
                .iter()
                .filter(|s| {
                    s.is_exam()
                        && is_strict_avail_outpatient.get(&(s.name.clone(), d)).copied().unwrap_or(false)
                })
                .count() as i32;

            if avail_t < req_t {
                causes.push(ConflictCause {
                    category: "2階裏 日次最低人数不足".to_string(),
                    scope_label: format_day_label(ctx, d),
                    detail: format!("出勤可能({avail_t}名) < 最低必要({req_t}名)"),
                });
            }
            if avail_n < req_n {
                causes.push(ConflictCause {
                    category: "2階裏 看護師最低人数不足".to_string(),
                    scope_label: format_day_label(ctx, d),
                    detail: format!("看護師出勤可能({avail_n}名) < 最低必要({req_n}名)"),
                });
            }
            if avail_e < req_e {
                causes.push(ConflictCause {
                    category: "2階裏 検査スタッフ最低人数不足".to_string(),
                    scope_label: format_day_label(ctx, d),
                    detail: format!("検査スタッフ出勤可能({avail_e}名) < 最低必要({req_e}名)"),
                });
            }
        }

        // 週次人日チェック
        for week in &ctx.weeks {
            let active_c: Vec<u32> = week
                .clinic_days
                .iter()
                .copied()
                .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
                .collect();
            if active_c.is_empty() {
                continue;
            }
            let mut week_demand = 0i32;
            for &d in &active_c {
                let dt = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                let is_ope = is_ope_date(&data.ope_dict, &dt);
                let outpatient: Vec<&Staff> = core_2f_back
                    .iter()
                    .copied()
                    .filter(|s| !(is_ope && s.is_ope_dedicated))
                    .collect();
                let (req_t, req_n, req_e) =
                    compute_2f_back_day_requirements(&core_2f_back, &outpatient, dt, w, is_ope, config);
                week_demand += req_t.max(req_n + req_e);
            }

            let mut week_supply = 0i32;
            for &s in &core_2f_back {
                let avail_in_w = active_c
                    .iter()
                    .filter(|&&d| {
                        is_strict_avail_outpatient
                            .get(&(s.name.clone(), d))
                            .copied()
                            .unwrap_or(false)
                    })
                    .count() as i32;
                let cap = if s.is_holiday_independent_week4 {
                    let plans = compute_holiday_independent_week4_plans(s, data, ctx, holidays);
                    let w_target = plans
                        .get(week.week_index)
                        .map(|p| p.week_target)
                        .unwrap_or(0);
                    let ope_in_w = active_c
                        .iter()
                        .filter(|&&d| {
                            s.is_ope_dedicated
                                && is_ope_date(&data.ope_dict, &ctx.date_map[&d])
                                && !is_personal_leave_on_date(s, &ctx.date_map[&d], config)
                        })
                        .count() as i32;
                    (w_target - ope_in_w).max(0).min(avail_in_w)
                } else if s.pattern.contains("週4") {
                    avail_in_w.min(4)
                } else if max_k > 0 {
                    avail_in_w.min(max_k as i32)
                } else {
                    avail_in_w
                };
                week_supply += cap;
            }

            if week_demand > week_supply {
                causes.push(ConflictCause {
                    category: "2階裏 週次総供給不足".to_string(),
                    scope_label: format_week_label(ctx, week.week_index),
                    detail: format!(
                        "外来必要人日({week_demand}人日)に対し週4基準での供給上限({week_supply}人日)が{}人日不足しています",
                        week_demand - week_supply
                    ),
                });
            }
        }
    }

    // 3. Ope専従スタッフの Tier 0 連勤競合チェック
    for staff in &data.staffs {
        if staff.is_ope_dedicated && staff.pattern != "除外" && max_k > 0 {
            let tail = extract_prev_tail_for_staff(staff, data, ctx);
            let tr_ones = tail.iter().rev().take_while(|&&v| v == 1).count();
            if let Some(&d1) = ctx.all_days.first() {
                let dt1 = ctx.date_map[&d1];
                let ope_or_inj_1 = !common_sp_vac.contains(&dt1)
                    && !is_personal_leave_on_date(staff, &dt1, config)
                    && (is_ope_date(&data.ope_dict, &dt1)
                        || is_injection_date(&data.ope_dict, &dt1));
                if tr_ones >= max_k && ope_or_inj_1 {
                    causes.push(ConflictCause {
                        category: "月跨ぎ連勤上限 vs Ope/注射必須".to_string(),
                        scope_label: format!("{} {}", staff.name, format_day_label(ctx, 1)),
                        detail: format!(
                            "前月末{}連勤で連勤上限({max_k}日)に達しており、{}は出勤できません（Ope/注射必須と競合）",
                            tr_ones,
                            format_day_label(ctx, 1)
                        ),
                    });
                }
            }
        }
    }

    // 4. シフト制約条件シート HARD 制約の日次供給不足チェック
    for rule in &data.rules {
        if rule.is_invalid_condition || !rule.level.to_uppercase().contains("HARD") {
            continue;
        }
        let target_days = parse_rule_target_dates(&rule.condition_desc, ctx, &data.ope_dict);
        let refs: Vec<&Staff> = rule
            .target_staffs
            .iter()
            .filter_map(|n| data.staffs.iter().find(|s| &s.name == n && s.pattern != "除外"))
            .collect();
        if refs.is_empty() {
            continue;
        }
        let eff_min = rule.val_min.min(refs.len() as i32).max(0);
        for &d in &target_days {
            let dt = ctx.date_map[&d];
            if common_sp_vac.contains(&dt) {
                continue;
            }
            let w = ctx.weekday_map[&d];
            let avail_cnt = refs
                .iter()
                .filter(|s| {
                    !is_personal_leave_on_date(s, &dt, config)
                        && !(s.basic_off_type == OffType::And
                            && s.basic_off_days.contains(&w)
                            && !s.is_ope_dedicated)
                })
                .count() as i32;
            if avail_cnt < eff_min {
                causes.push(ConflictCause {
                    category: "制約シートHARD不足".to_string(),
                    scope_label: format!("{} ({})", rule.name, format_day_label(ctx, d)),
                    detail: format!(
                        "『{}』必要人数({eff_min}名)に対し出勤可能({avail_cnt}名)が不足しています",
                        rule.condition_desc
                    ),
                });
            }
        }
    }

    // 5. 診断用MILPを実行して最小緩和候補を取得
    let (diag_res, _) = build_and_solve_model(
        data,
        ctx,
        target_days_dict,
        ideal_mean,
        holidays,
        SolveMode::Diagnose(approved),
    );

    if has_tier0_individual_overload || !diag_res.success {
        if causes.is_empty() {
            causes.push(ConflictCause {
                category: "Tier 0 絶対制約競合 (緩和不可)".to_string(),
                scope_label: format!("{}年{}月", ctx.year, ctx.month),
                detail: "日祝・確定休暇・連勤上限（またはパート固定出勤）同士が直接競合しており、緩和候補では解決できません。勤務条件表を見直してください。".to_string(),
            });
        }
        return DiagnosisResult {
            tier0_unsolvable: true,
            summary: "Tier 0（確定休暇・連勤上限等）に矛盾があるため緩和不可です。勤務条件表を見直してください。".to_string(),
            causes,
            proposals: Vec::new(),
        };
    }

    let mut diag = diag_res.diagnosis.unwrap_or(DiagnosisResult {
        tier0_unsolvable: false,
        summary: String::new(),
        causes: Vec::new(),
        proposals: Vec::new(),
    });

    if causes.is_empty() {
        causes.push(ConflictCause {
            category: "複合制約競合".to_string(),
            scope_label: format!("{}年{}月", ctx.year, ctx.month),
            detail: "複数の必須条件（所定日数・連勤上限・2階裏配置・HARD制約）が同時に競合しています。".to_string(),
        });
    }

    let rec_cnt = diag.proposals.iter().filter(|p| p.recommended).count();
    diag.causes = causes;
    diag.summary = format!(
        "厳格条件では適切解がありません（原因 {} 件検出 / 推奨緩和候補 {} 件）。以下の選択肢から適用する緩和条件を選んで再試行してください。",
        diag.causes.len(),
        rec_cnt
    );
    diag
}

/// 生成済みシフトの品質監査レポートを生成
fn build_audit_report(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
    shifts: &HashMap<String, HashMap<u32, u8>>,
    main_offs: &HashMap<String, u32>,
    target_days: &HashMap<String, i32>,
    approved: &HashSet<RelaxationKind>,
) -> ShiftAuditReport {
    let mut audit = ShiftAuditReport::default();
    let common_sp_vac = compute_common_sp_vac(data, ctx);
    let config = &data.config;

    for r in approved {
        audit.applied_relaxations.push(format_relaxation_kind(r, ctx));
    }

    for staff in &data.staffs {
        if staff.pattern == "除外" {
            continue;
        }
        let s_name = &staff.name;
        let s_shifts = match shifts.get(s_name) {
            Some(m) => m,
            None => continue,
        };

        // 1. 所定勤務日数検算
        let actual: i32 = ctx
            .all_days
            .iter()
            .map(|d| s_shifts.get(d).copied().unwrap_or(0) as i32)
            .sum();
        let target = target_days.get(s_name).copied().unwrap_or(0);
        if actual != target {
            if staff.is_special && actual > target {
                audit.special_extra_work_days.push(format!(
                    "{}: 実出勤{}日 / 週4基準{}日 (+{}日 週5応援)",
                    s_name,
                    actual,
                    target,
                    actual - target
                ));
            } else {
                let diff = actual - target;
                let sign = if diff > 0 { "+" } else { "" };
                audit.target_mismatches.push(format!(
                    "{}: 実出勤{}日 / 所定{}日 ({sign}{diff}日)",
                    s_name, actual, target
                ));
            }
        }

        // 2. 固定公休曜日の振替出勤検出
        let is_week4_fixed_one = staff.pattern.contains("週4")
            && staff.basic_off_days.len() == 1
            && staff.basic_off_type == OffType::And;
        let is_pure_fixed_and = staff.basic_off_type == OffType::And
            && !staff.basic_off_days.is_empty()
            && !is_week4_fixed_one
            && !staff.is_holiday_independent_week4;

        if staff.is_core && !staff.is_holiday_independent_week4 && (is_pure_fixed_and || is_week4_fixed_one) {
            for &d in &ctx.clinic_days {
                let dt = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                if !common_sp_vac.contains(&dt)
                    && !is_personal_leave_on_date(staff, &dt, config)
                    && staff.basic_off_days.contains(&w)
                    && s_shifts.get(&d).copied().unwrap_or(0) == 1
                {
                    audit.fixed_off_swaps.push(format!(
                        "{}: {} を固定公休から出勤へ振替",
                        s_name,
                        format_day_label(ctx, d)
                    ));
                }
            }
        }

        // 3. 主公休スライド検出
        if staff.is_core && !is_pure_fixed_and && !staff.is_holiday_independent_week4 {
            if let Some(&chosen_w) = main_offs.get(s_name) {
                for week in &ctx.weeks {
                    let week_has_spec = config.specified_off_rule
                        && week
                            .days
                            .iter()
                            .any(|&d| staff.specified_off_dates.contains(&ctx.date_map[&d]));
                    if week_has_spec {
                        continue;
                    }
                    for &d in &week.clinic_days {
                        let dt = ctx.date_map[&d];
                        if ctx.weekday_map[&d] == chosen_w
                            && !common_sp_vac.contains(&dt)
                            && !is_personal_leave_on_date(staff, &dt, config)
                            && s_shifts.get(&d).copied().unwrap_or(0) == 1
                        {
                            audit.main_off_slides.push(format!(
                                "{}: {} 主公休({})を別日へスライド",
                                s_name,
                                format_day_label(ctx, d),
                                weekday_kanji(chosen_w)
                            ));
                        }
                    }
                }
            }
        }
    }

    // 4. 2階裏（Ope推奨未達・早上がり重複・警告日）
    let core_2f_back: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();
    if !core_2f_back.is_empty() {
        for &d in &ctx.clinic_days {
            let dt = ctx.date_map[&d];
            if common_sp_vac.contains(&dt) {
                continue;
            }
            let is_ope = is_ope_date(&data.ope_dict, &dt);
            let out_members: Vec<&Staff> = core_2f_back
                .iter()
                .copied()
                .filter(|s| !(is_ope && s.is_ope_dedicated))
                .collect();
            let out_cnt: i32 = out_members
                .iter()
                .map(|s| shifts.get(&s.name).and_then(|m| m.get(&d)).copied().unwrap_or(0) as i32)
                .sum();
            let ns_cnt: i32 = out_members
                .iter()
                .filter(|s| s.is_nurse())
                .map(|s| shifts.get(&s.name).and_then(|m| m.get(&d)).copied().unwrap_or(0) as i32)
                .sum();
            let ex_cnt: i32 = out_members
                .iter()
                .filter(|s| s.is_exam())
                .map(|s| shifts.get(&s.name).and_then(|m| m.get(&d)).copied().unwrap_or(0) as i32)
                .sum();

            if out_cnt <= config.warn_2f_back_count {
                audit.warn_2f_back_days.push(format!(
                    "{}: 2階裏外来 {}名 (Ns{}/検{})",
                    format_day_label(ctx, d),
                    out_cnt,
                    ns_cnt,
                    ex_cnt
                ));
            }
            if is_ope && (ns_cnt < 2 || (ctx.weekday_map[&d] != 1 && ex_cnt < 2)) {
                let kind = data.ope_dict.get(&dt).map(|s| s.as_str()).unwrap_or("Ope");
                audit.rec_misses.push(format!(
                    "{} ({}): 2階裏Ope推奨未達 (Ns{}/検{})",
                    format_day_label(ctx, d),
                    kind,
                    ns_cnt,
                    ex_cnt
                ));
            }
            if !is_ope {
                let early_on: Vec<&str> = out_members
                    .iter()
                    .filter(|s| {
                        s.is_early_leave
                            && shifts.get(&s.name).and_then(|m| m.get(&d)).copied().unwrap_or(0) == 1
                    })
                    .map(|s| s.name.as_str())
                    .collect();
                if early_on.len() >= 2 {
                    audit.early_overlaps.push(format!(
                        "{}: 早上がり重複 ({}) / 外来{}名",
                        format_day_label(ctx, d),
                        early_on.join("・"),
                        out_cnt
                    ));
                }
            }
        }
    }

    // 5. シフト制約条件シートの推奨未達
    for rule in &data.rules {
        if rule.is_invalid_condition || rule.val_rec <= rule.val_min {
            continue;
        }
        let target_days = parse_rule_target_dates(&rule.condition_desc, ctx, &data.ope_dict);
        let refs: Vec<&Staff> = rule
            .target_staffs
            .iter()
            .filter_map(|n| data.staffs.iter().find(|s| &s.name == n && s.pattern != "除外"))
            .collect();
        if refs.is_empty() {
            continue;
        }
        let eff_rec = rule.val_rec.min(refs.len() as i32);
        for &d in &target_days {
            if common_sp_vac.contains(&ctx.date_map[&d]) {
                continue;
            }
            let cnt: i32 = refs
                .iter()
                .map(|s| shifts.get(&s.name).and_then(|m| m.get(&d)).copied().unwrap_or(0) as i32)
                .sum();
            if cnt < eff_rec {
                audit.rec_misses.push(format!(
                    "{}: 制約『{} / {}』推奨未達 ({}/{}名)",
                    format_day_label(ctx, d),
                    rule.name,
                    rule.condition_desc,
                    cnt,
                    eff_rec
                ));
            }
        }
    }

    // 念のため holidays 未使用警告抑止
    let _ = holidays;
    audit
}

fn append_audit_logs(audit: &ShiftAuditReport, logs: &mut Vec<String>) {
    if !audit.target_mismatches.is_empty() {
        logs.push(format!("  [所定調整] {}", audit.target_mismatches.join(", ")));
    }
    if !audit.special_extra_work_days.is_empty() {
        logs.push(format!("  [週5応援] {}", audit.special_extra_work_days.join(", ")));
    }
    if !audit.fixed_off_swaps.is_empty() {
        logs.push(format!("  [固定公休振替] {}件", audit.fixed_off_swaps.len()));
    }
    if !audit.rec_misses.is_empty() {
        logs.push(format!("  [推奨未達] {}件", audit.rec_misses.len()));
    }
    if !audit.main_off_slides.is_empty() {
        logs.push(format!("  [主公休スライド] {}件", audit.main_off_slides.len()));
    }
    if !audit.warn_2f_back_days.is_empty() {
        logs.push(format!("  [2階裏警告日] {}", audit.warn_2f_back_days.join(", ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rule_target_dates_multi_weekday_and_invalid() {
        let holidays = HashMap::new();
        let ctx = MonthContext::new(2026, 9, &holidays);
        let ope_dict = HashMap::new();

        // 複数曜日表記（「火曜・水曜」）
        let days = parse_rule_target_dates("火曜・水曜", &ctx, &ope_dict);
        assert!(!days.is_empty());
        for &d in &days {
            let w = ctx.weekday_map[&d];
            assert!(w == 1 || w == 2, "火曜(1)または水曜(2)のみ: {d}");
        }
        assert!(days.contains(&1)); // 2026-09-01 火曜
        assert!(days.contains(&2)); // 2026-09-02 水曜

        // 未知文字列は「全日」にならず空を返すこと
        assert!(parse_rule_target_dates("月末", &ctx, &ope_dict).is_empty());
        assert!(parse_rule_target_dates("第1金曜", &ctx, &ope_dict).is_empty());
        assert!(parse_rule_target_dates("月燿", &ctx, &ope_dict).is_empty());
    }
}
