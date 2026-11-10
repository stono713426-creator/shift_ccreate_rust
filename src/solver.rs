//! 勤務シフト自動生成システム 二段階数理最適化ソルバー (`solver.rs`)
//!
//! Version: v1.3.1
//! Updated: 2026-11-10 - v1.3.1 ソルバー競合・破綻防止強化（Stage 2 所定日数防衛重み25M化、連勤上限の月跨ぎ整合＆Stage 2緊急スラック化、特別枠[堀さん]Stage 2スラック＆target上限クランプ、2階裏およびHARDルールの週次/月次供給量Pigeonholeガード追加）
//! - v1.3.0 (2026-11-08): 2階裏人数・職種バランス強化（Ope日3人以上[Ns1/検査2]＆2+2推奨、火曜[Ns2/検査1]・木曜[Ns1/検査2]・土曜3人以上[PM休例外]）、石橋さん個別ルール（祝日非連動の週4固定・Ope/注射必須・条件付水曜公休・Ope日外来除外）、長谷川さん・石橋さん重複最小化＆フルタイム+1増員対応
//! - v1.2.0 (2026-11-01): 全仕様監査対応: 週4×固定1日特別枠対応、2階裏基本人数・ブレ幅・二乗ペナルティ実装、3ヶ月ローテーション履歴、7日ローリング窓・月末公休抑制、3連休有休除外、OPEタイポ吸収、タイムリミット適用
//! - v1.1.0 (2026-10-31): OPE曜日条件評価順・UTF-8スライス修正、除外スタッフ人数補正、OR型主公休スライド柔軟化、固定公休y変数整合化
//! - v1.0.0 (2026-10-31): Phase 2 実装: good_lp + HiGHS (MILP) 二段階数理最適化エンジンの完全実装
//!
//! SPECIFICATION.md セクション5 に基づく厳密な線形不等式モデルと二段階探索の実装。

use crate::config::*;
use crate::parser::*;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use good_lp::{
    constraint, default_solver, variable, Constraint, Expression, ProblemVariables,
    Solution, SolverModel, Variable,
};
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// ソルバーの解ステータス
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveStatus {
    Optimal,
    Feasible,
    Infeasible,
    Timeout,
    Error,
}

/// ソルバーの実行結果
#[derive(Debug, Clone)]
pub struct SolveResult {
    pub success: bool,
    pub status: SolveStatus,
    pub stage: u8, // 1: 厳格探索, 2: スラック緩和探索
    /// スタッフ名 -> 日付 -> 1(出勤) / 0(休み)
    pub shifts: HashMap<String, HashMap<u32, u8>>,
    /// スタッフ名 -> 当月主公休曜日 (0=月..5=土)
    pub main_offs: HashMap<String, u32>,
    /// スタッフ名 -> 当月所定勤務日数
    pub target_days: HashMap<String, i32>,
    pub execution_time_secs: f64,
    pub logs: Vec<String>,
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
            let date = NaiveDate::from_ymd_opt(safe_year, safe_month, d)
                .unwrap_or(first_day);
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

            for &d in &days {
                if is_holiday_map[&d] {
                    has_holiday = true;
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

/// コアスタッフ全員共通の特別有休（全期間分：月跨ぎ週の年末年始・夏季休診判定用）を算出
pub fn compute_all_common_sp_vac(data: &ParsedExcelData) -> HashSet<NaiveDate> {
    let core_staffs: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| s.is_core && s.pattern != "除外")
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

/// 石橋さん専用の各ISOカレンダー週の勤務計画情報（仕様 2-1）
#[derive(Debug, Clone)]
pub struct IshibashiWeekPlan {
    pub week_index: usize,
    /// 当月内のこの週で出勤すべき日数（通常週・祝日週ともに原則4日、端数週・大型連休・有休取得時は整合補正）
    pub week_target: i32,
    /// 当月内の水曜日の日付（1..=days_in_month、当月外や休診日の場合は None）
    pub wed_day: Option<u32>,
    /// 水曜日に Ope・注射がなく、水曜以外で週4日出勤を満たせるため原則公休とすべきか
    pub wed_should_be_off: bool,
}

/// 石橋さんの週次勤務計画（祝日非連動の週4日固定、Ope・注射必須出勤、条件付き水曜公休）を算出
pub fn compute_ishibashi_week_plans(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
) -> Vec<IshibashiWeekPlan> {
    let Some(staff) = data
        .staffs
        .iter()
        .find(|s| s.name == "石橋" && s.pattern != "除外")
    else {
        return Vec::new();
    };

    let all_common_sp_vac = compute_all_common_sp_vac(data);
    let first_day_of_month = NaiveDate::from_ymd_opt(ctx.year, ctx.month, 1).unwrap();
    let last_day_of_month =
        NaiveDate::from_ymd_opt(ctx.year, ctx.month, ctx.days_in_month).unwrap();
    let prev_end_date = first_day_of_month - Duration::days(1);
    let prev_tail_matches_month = match data.prev_sheet_year_month {
        Some((py, pm)) => {
            let next_ym = if pm == 12 { (py + 1, 1) } else { (py, pm + 1) };
            next_ym == (ctx.year, ctx.month)
        }
        None => true,
    };
    let prev_tail_vec = if prev_tail_matches_month {
        let common_sp_vac_curr = compute_common_sp_vac(data, ctx);
        Some(sanitize_prev_tail_for_staff(
            staff,
            data,
            ctx,
            &common_sp_vac_curr,
        ))
    } else {
        None
    };
    let prev_tail = prev_tail_vec.as_ref();
    let trailing_prev_ones = prev_tail
        .map(|t| t.iter().rev().take_while(|&&v| v == 1).count())
        .unwrap_or(0);

    let mut plans = Vec::with_capacity(ctx.weeks.len());

    for week in &ctx.weeks {
        if week.days.is_empty() {
            plans.push(IshibashiWeekPlan {
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

            let has_ope_or_inj = !is_closed
                && !is_leave
                && (is_ope_date(&data.ope_dict, &dt) || is_injection_date(&data.ope_dict, &dt));

            // 当月1日が前月末尾からの連勤上限に達している場合（かつOpe・注射必須でない場合）は出勤不可
            let blocked_by_prev_consec = dt == first_day_of_month
                && data.config.max_consecutive_work_days > 0
                && trailing_prev_ones >= data.config.max_consecutive_work_days
                && !has_ope_or_inj;

            // Ope日・注射日は曜日（土曜の基本公休含む）を問わず出勤可能・必須出勤
            is_avail[w] =
                !is_closed && !is_leave && !blocked_by_prev_consec && (!is_basic_off || has_ope_or_inj);
            is_forced[w] = has_ope_or_inj;
        }

        let avail_all = is_avail.iter().filter(|&&b| b).count() as i32;
        let forced_all = is_forced.iter().filter(|&&b| b).count() as i32;
        let avail_curr = (0..6).filter(|&w| in_curr[w] && is_avail[w]).count() as i32;
        let forced_curr = (0..6).filter(|&w| in_curr[w] && is_forced[w]).count() as i32;

        // 週4日出勤基準（個人の有休・夏休取得日がある週はその日数分を控除して集計行のマイナス処理と整合させる）
        let desired_full = (4 - personal_vac_cnt).max(0);
        let max_consec = data.config.max_consecutive_work_days as i32;
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
            // 月初の月跨ぎ端数週: 前月シフト実績があれば実出勤数を引き継ぎ、なければ標準選択から推定
            let mut prev_worked = 0i32;
            for w in 0..6usize {
                if in_prev[w] {
                    let days_back = (prev_end_date - dates_6[w]).num_days();
                    if days_back >= 0 {
                        let db = days_back as usize;
                        if let Some(tail) = prev_tail {
                            if tail.len() > db {
                                if tail[tail.len() - 1 - db] == 1 {
                                    prev_worked += 1;
                                }
                                continue;
                            }
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
            // 月末の月跨ぎ端数週: 標準選択のうち当月分の出勤日数を割り当て（翌月側で残りを引き継ぎ週計4日を維持）
            let sel_curr = std_selected.iter().filter(|&&w| in_curr[w]).count() as i32;
            sel_curr
                .min(avail_curr)
                .max(forced_curr)
                .min(max_consec)
        } else {
            // 当月完結週: 祝日の有無にかかわらず原則4日（物理上限・Ope必須下限でクランプ）
            full_week_target.min(avail_curr).max(forced_curr)
        };

        let wed_day = if in_curr[2] && is_avail[2] {
            Some(dates_6[2].day())
        } else {
            None
        };
        let wed_should_be_off = if in_curr[2] && is_avail[2] && !is_forced[2] {
            let avail_curr_without_wed = avail_curr - 1;
            avail_curr_without_wed >= week_target
        } else {
            false
        };

        plans.push(IshibashiWeekPlan {
            week_index: week.week_index,
            week_target,
            wed_day,
            wed_should_be_off,
        });
    }

    plans
}

/// 前月末尾シフト列をスタッフの当月1日以降の必須出勤と矛盾しないよう正規化する
fn sanitize_prev_tail_for_staff(
    staff: &Staff,
    data: &ParsedExcelData,
    ctx: &MonthContext,
    common_sp_vac: &HashSet<NaiveDate>,
) -> Vec<u8> {
    let max_k = data.config.max_consecutive_work_days;
    let prev_tail = data
        .prev_month_tail_shifts
        .get(&staff.name)
        .cloned()
        .unwrap_or_default();
    if max_k == 0 || prev_tail.is_empty() {
        return Vec::new();
    }
    let tail_take = max_k.min(prev_tail.len());
    let mut slice: Vec<u8> = prev_tail[prev_tail.len() - tail_take..]
        .iter()
        .map(|&v| if v > 0 { 1 } else { 0 })
        .collect();

    // 当月1日から連続する「ハード必須出勤日」（石橋さんのOpe・注射日など）の日数をカウント
    let mut forced_start_ones = 0usize;
    if staff.name == "石橋" && staff.pattern != "除外" {
        for &d in &ctx.all_days {
            let date = ctx.date_map[&d];
            let is_closed = ctx.weekday_map[&d] == 6
                || ctx.is_holiday_map[&d]
                || common_sp_vac.contains(&date);
            let is_leave = staff.vacation_dates.contains(&date)
                || staff.special_vacation_dates.contains(&date)
                || staff.preferred_off_dates.contains(&date)
                || (data.config.specified_off_rule && staff.specified_off_dates.contains(&date));
            let has_ope_or_inj = !is_closed
                && !is_leave
                && (is_ope_date(&data.ope_dict, &date)
                    || is_injection_date(&data.ope_dict, &date));
            if has_ope_or_inj {
                forced_start_ones += 1;
            } else {
                break;
            }
        }
    }

    let allowed_trailing = max_k.saturating_sub(forced_start_ones);
    let trailing_ones = slice.iter().rev().take_while(|&&v| v == 1).count();
    if trailing_ones > allowed_trailing {
        let start_pos = slice.len() - trailing_ones;
        let to_clear = trailing_ones - allowed_trailing;
        for i in 0..to_clear {
            slice[start_pos + i] = 0;
        }
    }

    slice
}

/// スタッフの当月における物理的最大出勤可能日数を算出する（確定休暇・固定公休・月跨ぎ連勤上限を考慮）
fn compute_staff_max_workable_days(
    staff: &Staff,
    data: &ParsedExcelData,
    ctx: &MonthContext,
    common_sp_vac: &HashSet<NaiveDate>,
) -> i32 {
    if staff.pattern == "除外" {
        return 0;
    }
    let max_k = data.config.max_consecutive_work_days;
    let tail_slice = sanitize_prev_tail_for_staff(staff, data, ctx, common_sp_vac);
    let mut streak = tail_slice.iter().rev().take_while(|&&v| v == 1).count();
    let mut max_workable = 0i32;

    for &d in &ctx.all_days {
        let date = ctx.date_map[&d];
        let w = ctx.weekday_map[&d];

        let is_closed = w == 6 || ctx.is_holiday_map[&d] || common_sp_vac.contains(&date);
        let is_leave = staff.vacation_dates.contains(&date)
            || staff.special_vacation_dates.contains(&date)
            || staff.preferred_off_dates.contains(&date)
            || (data.config.specified_off_rule && staff.specified_off_dates.contains(&date));

        let is_fixed_basic_off = if staff.name == "石橋" {
            let has_ope_or_inj =
                is_ope_date(&data.ope_dict, &date) || is_injection_date(&data.ope_dict, &date);
            staff.basic_off_days.contains(&w) && !has_ope_or_inj
        } else if !staff.is_core {
            staff.basic_off_days.contains(&w)
        } else {
            staff.basic_off_type == OffType::And && staff.basic_off_days.contains(&w)
        };

        if is_closed || is_leave || is_fixed_basic_off {
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

/// 5.2 所定勤務日数の計算
pub fn calculate_target_days(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    holidays: &HashMap<NaiveDate, String>,
) -> (HashMap<String, i32>, HashMap<String, f64>) {
    let mut target_days = HashMap::new();
    let mut ideal_mean = HashMap::new();

    // 1. コアスタッフ全員共通の特別有休 (common_sp_vac)
    let common_sp_vac = compute_common_sp_vac(data, ctx);
    let ishibashi_plans = compute_ishibashi_week_plans(data, ctx, holidays);

    // 2. 共通公休日数
    // common_off_w5: 当月の土曜日のうち、祝日でも common_sp_vac でもない日数
    // common_off_w4: 当月の金曜・土曜日のうち、祝日でも common_sp_vac でもない日数
    let mut common_off_w5 = 0;
    let mut common_off_w4 = 0;

    for &d in &ctx.all_days {
        let date = ctx.date_map[&d];
        let w = ctx.weekday_map[&d];
        let is_holiday = ctx.is_holiday_map[&d];
        let is_common_sp = common_sp_vac.contains(&date);

        if !is_holiday && !is_common_sp {
            if w == 5 {
                // 土曜
                common_off_w5 += 1;
                common_off_w4 += 1;
            } else if w == 4 {
                // 金曜
                common_off_w4 += 1;
            }
        }
    }

    // 3. basic_slots = 暦日数 - 日曜数 - 日曜以外の祝日数
    let sunday_count = ctx.all_days.iter().filter(|&&d| ctx.weekday_map[&d] == 6).count() as i32;
    let holiday_not_sun_count = ctx
        .all_days
        .iter()
        .filter(|&&d| ctx.is_holiday_map[&d] && ctx.weekday_map[&d] != 6)
        .count() as i32;
    let basic_slots = ctx.days_in_month as i32 - sunday_count - holiday_not_sun_count;

    // 各スタッフの target_days
    for staff in &data.staffs {
        if staff.pattern == "除外" {
            target_days.insert(staff.name.clone(), 0);
            continue;
        }

        // 石橋さん専用ルール（仕様 2-1: 祝日非連動の毎週4日出勤積み上げ、月間15〜16日程度）
        if staff.name == "石橋" {
            let ishi_target: i32 = ishibashi_plans.iter().map(|p| p.week_target).sum();
            target_days.insert(staff.name.clone(), ishi_target);
            continue;
        }

        let max_workable = compute_staff_max_workable_days(staff, data, ctx, &common_sp_vac);

        if !staff.is_core {
            // パート等非コア: 希望出勤曜日（月〜土、日曜・祝日・有休・特有・希望休・指定公休を除く、連勤上限考慮）の出勤日数
            let mut work_days_count = 0;
            for &d in &ctx.clinic_days {
                let date = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];

                let is_basic_off = staff.basic_off_days.contains(&w);
                let is_vac = staff.vacation_dates.contains(&date);
                let is_sp_vac = staff.special_vacation_dates.contains(&date);
                let is_pref_off = staff.preferred_off_dates.contains(&date);
                let is_spec_off =
                    data.config.specified_off_rule && staff.specified_off_dates.contains(&date);

                if !common_sp_vac.contains(&date)
                    && !is_basic_off
                    && !is_vac
                    && !is_sp_vac
                    && !is_pref_off
                    && !is_spec_off
                {
                    work_days_count += 1;
                }
            }
            target_days.insert(staff.name.clone(), work_days_count.min(max_workable));
        } else {
            // コアスタッフ
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
            let target = raw_target.min(max_workable);
            target_days.insert(staff.name.clone(), target);
        }
    }

    // 4. セクション別理想平均人数 M_s (全員共通特別有休の休診日を除いた実稼働日数で算出)
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
            .map(|s| target_days.get(&s.name).cloned().unwrap_or(0))
            .sum();
        ideal_mean.insert("1階".to_string(), sum_1f as f64 / work_days_len);

        let sum_2f_front: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "2階表" && s.is_core && s.pattern != "除外")
            .map(|s| target_days.get(&s.name).cloned().unwrap_or(0))
            .sum();
        ideal_mean.insert("2階表".to_string(), sum_2f_front as f64 / work_days_len);

        let sum_2f_back_raw: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "2階裏" && (s.is_core || s.name == "石橋") && s.pattern != "除外")
            .map(|s| target_days.get(&s.name).cloned().unwrap_or(0))
            .sum();

        // 仕様 2-2: Ope日（注射以外の手術がある日）の石橋さんの出勤分は 2階裏（外来）人数から除外して平均を算出
        let ishi_ope_days: i32 = if let Some(ishi) = data
            .staffs
            .iter()
            .find(|s| s.name == "石橋" && s.section == "2階裏" && s.pattern != "除外")
        {
            ctx.clinic_days
                .iter()
                .filter(|&&d| {
                    let dt = ctx.date_map[&d];
                    !common_sp_vac.contains(&dt)
                        && is_ope_date(&data.ope_dict, &dt)
                        && !ishi.vacation_dates.contains(&dt)
                        && !ishi.special_vacation_dates.contains(&dt)
                        && !ishi.preferred_off_dates.contains(&dt)
                        && !(data.config.specified_off_rule && ishi.specified_off_dates.contains(&dt))
                })
                .count() as i32
        } else {
            0
        };

        let sum_2f_back = (sum_2f_back_raw - ishi_ope_days).max(0);
        ideal_mean.insert("2階裏".to_string(), sum_2f_back as f64 / work_days_len);
    }

    (target_days, ideal_mean)
}

/// ルール適用対象日リストの抽出
pub fn parse_rule_target_dates(
    desc: &str,
    ctx: &MonthContext,
    ope_dict: &HashMap<NaiveDate, String>,
) -> Vec<u32> {
    let desc_trim = desc.trim();
    let desc_lower = desc_trim.to_lowercase();
    let mut target_days = Vec::new();

    let day_filter = |target_w: u32| -> Vec<u32> {
        ctx.clinic_days
            .iter()
            .filter(|&&d| ctx.weekday_map[&d] == target_w)
            .cloned()
            .collect()
    };

    let base_days_by_weekday = || -> Vec<u32> {
        if desc_trim.contains("月曜") {
            day_filter(0)
        } else if desc_trim.contains("火曜") {
            day_filter(1)
        } else if desc_trim.contains("水曜") {
            day_filter(2)
        } else if desc_trim.contains("木曜") {
            day_filter(3)
        } else if desc_trim.contains("金曜") {
            day_filter(4)
        } else if desc_trim.contains("土曜") {
            day_filter(5)
        } else {
            ctx.clinic_days.clone()
        }
    };

    if desc_trim.contains("全日") {
        target_days = ctx.clinic_days.clone();
    } else if desc_trim.contains("注射") {
        // 仕様 1-2: 「注射日」指定ルール
        for d in base_days_by_weekday() {
            if is_injection_date(ope_dict, &ctx.date_map[&d]) {
                target_days.push(d);
            }
        }
    } else if desc_trim.contains("手術のある日") || desc_trim.contains("日曜日以外の手術のある日") {
        // 仕様 1-2: 「注射」以外のオペがある「Ope日」のみ対象
        target_days = ctx
            .clinic_days
            .iter()
            .filter(|&&d| is_ope_date(ope_dict, &ctx.date_map[&d]))
            .cloned()
            .collect();
    } else if desc_trim.contains("手術") || desc_lower.contains("ope") || desc_trim.contains("オペ") {
        // 先頭の曜日指定があるか判定 (例: "火曜(OPE日)", "水曜(OPE日)")
        let base_days = base_days_by_weekday();

        // キーワード抽出: 「...」または部分一致 (UTF-8 文字境界安全)
        let mut kw = String::new();
        if let Some(start) = desc_trim.find('「') {
            let content_start = start + '「'.len_utf8();
            if let Some(rel_end) = desc_trim[content_start..].find('」') {
                kw = desc_trim[content_start..content_start + rel_end].trim().to_string();
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
                // 術式指定なしの一般 OPE 日指定（例: 火曜(OPE日)）は「注射」のみの注射日を除外し Ope日 のみ一致
                if is_ope_date(ope_dict, &date) {
                    target_days.push(d);
                }
            } else if let Some(ope_text) = ope_dict.get(&date) {
                let ope_lower = ope_text.to_lowercase();
                let matched = if kw_lower == "vit" || kw_lower == "viit" || kw == "硝子体" {
                    // 実データのタイポ ("Viit" 等) も吸収
                    ope_lower.contains("vit") || ope_lower.contains("viit") || ope_text.contains("硝子体")
                } else if kw_lower == "cat" || kw_lower == "cay" || kw == "白内障" {
                    // 実データのタイポ ("cay" 等) も吸収
                    ope_lower.contains("cat") || ope_lower.contains("cay") || ope_text.contains("白内障")
                } else if kw_lower == "gla" || kw == "緑内障" {
                    ope_lower.contains("gla") || ope_text.contains("緑内障")
                } else if kw == "下垂" || kw == "眼瞼" || kw == "眼瞼下垂" {
                    ope_text.contains("下垂") || ope_text.contains("眼瞼")
                } else {
                    ope_lower.contains(&kw_lower)
                };
                if matched {
                    target_days.push(d);
                }
            }
        }
    } else if desc_trim.contains("月曜") {
        target_days = day_filter(0);
    } else if desc_trim.contains("火曜") {
        target_days = day_filter(1);
    } else if desc_trim.contains("水曜") {
        target_days = day_filter(2);
    } else if desc_trim.contains("木曜") {
        target_days = day_filter(3);
    } else if desc_trim.contains("金曜") {
        target_days = day_filter(4);
    } else if desc_trim.contains("土曜") {
        target_days = day_filter(5);
    } else {
        target_days = ctx.clinic_days.clone();
    }

    target_days
}

/// 勤務シフト自動生成の実行（二段階探索: Stage 1 厳格 -> Stage 2 スラック緩和）
pub fn solve_shift(
    data: &ParsedExcelData,
    year: i32,
    month: u32,
    holidays: &HashMap<NaiveDate, String>,
) -> SolveResult {
    let start_time = Instant::now();
    let mut logs = Vec::new();
    logs.push(format!("=== 勤務シフト自動生成開始: {year}年{month:02}月 ==="));

    let ctx = MonthContext::new(year, month, holidays);
    let (target_days_dict, ideal_mean) = calculate_target_days(data, &ctx, holidays);

    logs.push(format!(
        "カレンダー設定完了: 暦日数={}, 診療日数={}, 週数={}",
        ctx.days_in_month,
        ctx.clinic_days.len(),
        ctx.weeks.len()
    ));

    // Stage 1: 厳格探索
    logs.push("--- Stage 1 (厳格探索) を開始します ---".to_string());
    let (stage1_res, _) = build_and_solve_stage(data, &ctx, &target_days_dict, &ideal_mean, holidays, 1);

    if stage1_res.success {
        let elapsed = start_time.elapsed().as_secs_f64();
        logs.extend(stage1_res.logs);
        logs.push(format!("Stage 1 で最適解を取得しました (所要時間: {:.2}秒)", elapsed));
        return SolveResult {
            success: true,
            status: stage1_res.status,
            stage: 1,
            shifts: stage1_res.shifts,
            main_offs: stage1_res.main_offs,
            target_days: target_days_dict,
            execution_time_secs: elapsed,
            logs,
        };
    }

    // Stage 2: スラック緩和探索
    logs.extend(stage1_res.logs);
    logs.push("Stage 1 が INFEASIBLE のため、Stage 2 (スラック変数による優先度緩和探索) を開始します...".to_string());
    let (stage2_res, _) = build_and_solve_stage(data, &ctx, &target_days_dict, &ideal_mean, holidays, 2);

    let elapsed = start_time.elapsed().as_secs_f64();
    logs.extend(stage2_res.logs);
    logs.push(format!("Stage 2 探索終了: 成功={}, 所要時間: {:.2}秒", stage2_res.success, elapsed));

    SolveResult {
        success: stage2_res.success,
        status: stage2_res.status,
        stage: 2,
        shifts: stage2_res.shifts,
        main_offs: stage2_res.main_offs,
        target_days: target_days_dict,
        execution_time_secs: elapsed,
        logs,
    }
}

/// 単一ステージのモデル構築および解の取得
fn build_and_solve_stage(
    data: &ParsedExcelData,
    ctx: &MonthContext,
    target_days_dict: &HashMap<String, i32>,
    ideal_mean: &HashMap<String, f64>,
    holidays: &HashMap<NaiveDate, String>,
    stage: u8,
) -> (SolveResult, Option<Box<dyn Solution>>) {
    let mut logs = Vec::new();
    let mut vars = ProblemVariables::new();
    let mut constraints: Vec<Constraint> = Vec::new();
    let mut objective = Expression::from(0.0);

    let config = &data.config;
    let common_sp_vac = compute_common_sp_vac(data, ctx);
    let ishibashi_plans = compute_ishibashi_week_plans(data, ctx, holidays);
    let mut ishibashi_forced_wed_offs: HashSet<u32> = HashSet::new();
    let mut sanitized_prev_tails: HashMap<String, Vec<u8>> = HashMap::new();
    let mut trailing_prev_ones_map: HashMap<String, usize> = HashMap::new();
    let mut forced_work_days: HashMap<String, HashSet<u32>> = HashMap::new();
    let mut hard_holiday_week_off: HashSet<(String, usize)> = HashSet::new();
    for staff in &data.staffs {
        let s_tail = sanitize_prev_tail_for_staff(staff, data, ctx, &common_sp_vac);
        let tr_ones = s_tail.iter().rev().take_while(|&&v| v == 1).count();
        if stage == 1 {
            if let Some(orig_tail) = data.prev_month_tail_shifts.get(&staff.name) {
                let max_k = config.max_consecutive_work_days;
                if max_k > 0 && !orig_tail.is_empty() {
                    let take_n = max_k.min(orig_tail.len());
                    let orig_ones = orig_tail[orig_tail.len() - take_n..]
                        .iter()
                        .rev()
                        .take_while(|&&v| v > 0)
                        .count();
                    if orig_ones > tr_ones {
                        logs.push(format!(
                            "⚠️ [事前診断] スタッフ『{}』の前月末{}連勤と当月1日からの必須出勤が連勤上限（{}日）と競合したため、月跨ぎ連勤制約を自動調整しました。",
                            staff.name, orig_ones, max_k
                        ));
                    }
                }
            }
        }
        sanitized_prev_tails.insert(staff.name.clone(), s_tail);
        trailing_prev_ones_map.insert(staff.name.clone(), tr_ones);
        forced_work_days.insert(staff.name.clone(), HashSet::new());
    }

    // 1. 決定変数 x[s, d] の定義 (s: スタッフ, d: 1..=days_in_month)
    let mut x_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        for &d in &ctx.all_days {
            let var = vars.add(variable().binary());
            x_vars.insert((staff.name.clone(), d), var);
        }
    }

    // 2. 決定変数 y[s, w] の定義 (w: 0=月..5=土) - コアスタッフ（および石橋さん）の主公休選択フラグ
    let mut y_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        if (staff.is_core || staff.name == "石橋") && staff.pattern != "除外" {
            for w in 0..=5 {
                let var = vars.add(variable().binary());
                y_vars.insert((staff.name.clone(), w), var);
            }
        }
    }

    // 3. 基本公休・強制休みの適用
    for staff in &data.staffs {
        let s_name = &staff.name;

        if staff.pattern == "除外" {
            for &d in &ctx.all_days {
                constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
            }
            continue;
        }

        // 休業日（日曜および祝日）は全員休み
        for &d in &ctx.all_days {
            if ctx.weekday_map[&d] == 6 || ctx.is_holiday_map[&d] {
                constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
            }
        }

        // 有休・特別有休・希望休・指定公休
        for &d in &ctx.all_days {
            let date = ctx.date_map[&d];
            if staff.vacation_dates.contains(&date)
                || staff.special_vacation_dates.contains(&date)
                || staff.preferred_off_dates.contains(&date)
                || (config.specified_off_rule && staff.specified_off_dates.contains(&date))
            {
                constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
            }
        }

        // 仕様 2-1: 石橋さんの個別勤務ルール（祝日非連動の週4日固定、Ope日・注射日必須出勤、条件付き水曜公休）
        if s_name == "石橋" {
            // 主公休表示用フラグは「水曜(2)」に固定（土曜日と合わせて「土・水」表示）
            for w in 0..=5 {
                let target_y = if w == 2 { 1.0 } else { 0.0 };
                constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == target_y));
            }

            for week in &ctx.weeks {
                let Some(plan) = ishibashi_plans.get(week.week_index) else {
                    continue;
                };

                let forced_cnt_in_week = week
                    .clinic_days
                    .iter()
                    .filter(|&&d| {
                        let date = ctx.date_map[&d];
                        let is_leave = common_sp_vac.contains(&date)
                            || staff.vacation_dates.contains(&date)
                            || staff.special_vacation_dates.contains(&date)
                            || staff.preferred_off_dates.contains(&date)
                            || (config.specified_off_rule
                                && staff.specified_off_dates.contains(&date));
                        !is_leave
                            && (is_ope_date(&data.ope_dict, &date)
                                || is_injection_date(&data.ope_dict, &date))
                    })
                    .count();

                for &d in &week.clinic_days {
                    let date = ctx.date_map[&d];
                    let w = ctx.weekday_map[&d];

                    if common_sp_vac.contains(&date) {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                        continue;
                    }

                    let is_leave = staff.vacation_dates.contains(&date)
                        || staff.special_vacation_dates.contains(&date)
                        || staff.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && staff.specified_off_dates.contains(&date));
                    if is_leave {
                        continue;
                    }

                    let has_ope_or_inj = is_ope_date(&data.ope_dict, &date)
                        || is_injection_date(&data.ope_dict, &date);

                    if has_ope_or_inj {
                        // Ope日および注射日は曜日を問わず必須出勤（ハード制約、ただし週連勤上限超過時やStage 2はスラック緩和）
                        if stage == 1 && forced_cnt_in_week <= config.max_consecutive_work_days {
                            constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 1.0));
                            forced_work_days.entry(s_name.clone()).or_default().insert(d);
                        } else {
                            let ope_slack = vars.add(variable().min(0.0));
                            constraints.push(constraint!(
                                x_vars[&(s_name.clone(), d)] + ope_slack >= 1.0
                            ));
                            objective += ope_slack * WEIGHT_FIXED_OFF_SHORTAGE as f64;
                        }
                    } else if staff.basic_off_days.contains(&w) {
                        // Ope・注射がない基本公休曜日（土曜）は公休
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    }
                }

                // 条件付き水曜公休: 水曜に Ope・注射がない週は原則として水曜を公休とする
                if let Some(wed_d) = plan.wed_day {
                    if plan.wed_should_be_off {
                        let wed_date = ctx.date_map[&wed_d];
                        let is_other_2f_b_avail = |s: &&Staff| -> bool {
                            let tr_ones = trailing_prev_ones_map.get(&s.name).copied().unwrap_or(0);
                            let blocked_d1 = wed_d == 1
                                && config.max_consecutive_work_days > 0
                                && tr_ones >= config.max_consecutive_work_days;
                            s.section == "2階裏"
                                && s.name != "石橋"
                                && s.pattern != "除外"
                                && !blocked_d1
                                && !s.vacation_dates.contains(&wed_date)
                                && !s.special_vacation_dates.contains(&wed_date)
                                && !s.preferred_off_dates.contains(&wed_date)
                                && !(config.specified_off_rule
                                    && s.specified_off_dates.contains(&wed_date))
                                && !((s.basic_off_type == OffType::And || !s.is_core)
                                    && s.basic_off_days.contains(&2))
                        };
                        let other_avail_nurses = data
                            .staffs
                            .iter()
                            .filter(|s| is_other_2f_b_avail(s) && s.is_nurse())
                            .count();
                        let other_avail_total = data
                            .staffs
                            .iter()
                            .filter(|s| is_other_2f_b_avail(s) && s.is_core)
                            .count();
                        let min_other_needed =
                            (config.sec_2f_back_base_cap as usize).saturating_sub(1);

                        if stage == 1
                            && other_avail_nurses > 0
                            && other_avail_total >= min_other_needed
                        {
                            constraints.push(constraint!(x_vars[&(s_name.clone(), wed_d)] == 0.0));
                            ishibashi_forced_wed_offs.insert(wed_d);
                        } else {
                            objective += x_vars[&(s_name.clone(), wed_d)]
                                * WEIGHT_ISHIBASHI_WED_OFF_SLACK as f64;
                        }
                    }
                }

                // 週4日出勤の厳守（祝日の有無にかかわらず plan.week_target 日出勤）
                let week_sum: Expression = week
                    .clinic_days
                    .iter()
                    .map(|&d| x_vars[&(s_name.clone(), d)])
                    .sum();
                if stage == 1 {
                    constraints.push(constraint!(week_sum == plan.week_target as f64));
                } else {
                    let w_under = vars.add(variable().min(0.0));
                    let w_over = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        week_sum.clone() + w_under >= plan.week_target as f64
                    ));
                    constraints.push(constraint!(
                        week_sum - w_over <= plan.week_target as f64
                    ));
                    objective += (w_under + w_over) * WEIGHT_FIXED_OFF_SHORTAGE as f64;
                }
            }
        } else if !staff.is_core {
            // 非コアスタッフのスケジュール固定（連勤上限超過時は自動で公休挿入、Stage 2ではスラック緩和可能）
            let mut consec_streak = trailing_prev_ones_map.get(s_name).copied().unwrap_or(0);
            for &d in &ctx.all_days {
                let date = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];
                if w == 6 || ctx.is_holiday_map[&d] {
                    consec_streak = 0;
                    continue;
                }

                let is_off = common_sp_vac.contains(&date)
                    || staff.basic_off_days.contains(&w)
                    || staff.vacation_dates.contains(&date)
                    || staff.special_vacation_dates.contains(&date)
                    || staff.preferred_off_dates.contains(&date)
                    || (config.specified_off_rule && staff.specified_off_dates.contains(&date));

                if is_off {
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    consec_streak = 0;
                } else if config.max_consecutive_work_days > 0
                    && consec_streak >= config.max_consecutive_work_days
                {
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    consec_streak = 0;
                } else {
                    consec_streak += 1;
                    if stage == 1 {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 1.0));
                        forced_work_days.entry(s_name.clone()).or_default().insert(d);
                    } else {
                        let nc_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            x_vars[&(s_name.clone(), d)] + nc_slack >= 1.0
                        ));
                        objective += nc_slack * WEIGHT_TARGET_DAYS_SLACK as f64;
                    }
                }
            }
            continue;
        } else {
            // コアスタッフ（石橋さん以外）の勤務パターン制約
            let target = target_days_dict.get(s_name).cloned().unwrap_or(0);
            let month_work_sum: Expression = ctx
                .clinic_days
                .iter()
                .map(|&d| x_vars[&(s_name.clone(), d)])
                .sum();

            let is_week4_fixed_one = staff.pattern.contains("週4")
                && staff.basic_off_days.len() == 1
                && staff.basic_off_type == OffType::And;

            if is_week4_fixed_one {
                // 週4×固定1日指定者 (例: 宮澤さん)
                let fix_w = staff.basic_off_days[0];
                // 固定公休曜日 (診療日) は休み
                for &d in &ctx.clinic_days {
                    if ctx.weekday_map[&d] == fix_w {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    }
                }

                // 第2公休候補曜日 C: {0..4} \ {fix_w}
                let candidates: Vec<u32> = (0..=4).filter(|&w| w != fix_w).collect();
                let y_sum: Expression = candidates.iter().map(|&w| y_vars[&(s_name.clone(), w)]).sum();
                constraints.push(constraint!(y_sum == 1.0));

                // 候補外の y は 0
                for w in 0..=5 {
                    if !candidates.contains(&w) {
                        constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 0.0));
                    }
                }

                // 通常週4スタッフ（特別枠でない場合）は、指定公休を含まない各ISO週において原則として第2公休曜日は休み
                if !staff.is_special {
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
                                        x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)] - slide_slack <= 1.0
                                    ));
                                    objective += slide_slack * WEIGHT_SECOND_OFF_SLIDE_SLACK as f64;
                                }
                            }
                        }
                    }
                }

                // 月間出勤数
                if staff.is_special {
                    let extra_work = vars.add(variable().min(0.0));
                    constraints.push(constraint!(month_work_sum.clone() - extra_work <= target as f64));
                    objective += extra_work * WEIGHT_SPECIAL_EXTRA_WORK as f64;
                    if stage == 1 {
                        constraints.push(constraint!(month_work_sum.clone() >= target as f64));
                        constraints.push(constraint!(month_work_sum <= (target + 5) as f64));
                    } else {
                        let slack_under = vars.add(variable().min(0.0));
                        let slack_over = vars.add(variable().min(0.0));
                        constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                        constraints.push(constraint!(month_work_sum - slack_over <= (target + 5) as f64));
                        objective += (slack_under + slack_over) * WEIGHT_TARGET_DAYS_SLACK as f64;
                    }
                } else if stage == 1 {
                    constraints.push(constraint!(month_work_sum == target as f64));
                } else {
                    let slack_under = vars.add(variable().min(0.0));
                    let slack_over = vars.add(variable().min(0.0));
                    constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                    constraints.push(constraint!(month_work_sum - slack_over <= target as f64));
                    objective += (slack_under + slack_over) * WEIGHT_TARGET_DAYS_SLACK as f64;
                }
            } else if staff.basic_off_type == OffType::And && !staff.basic_off_days.is_empty() {
                // AND 固定公休スタッフ
                for &d in &ctx.clinic_days {
                    let w = ctx.weekday_map[&d];
                    if staff.basic_off_days.contains(&w) {
                        constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                    }
                }

                // y_vars を固定公休曜日に一致させる（主公休不一致ペナルティのすり抜け防止）
                for w in 0..=5 {
                    if staff.basic_off_days.contains(&w) {
                        constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 1.0));
                    } else {
                        constraints.push(constraint!(y_vars[&(s_name.clone(), w)] == 0.0));
                    }
                }

                // 上限は target
                constraints.push(constraint!(month_work_sum.clone() <= target as f64));

                // 不足許容スラック
                let spec_off_cnt = if config.specified_off_rule {
                    staff
                        .specified_off_dates
                        .iter()
                        .filter(|d| {
                            d.year() == ctx.year
                                && d.month() == ctx.month
                                && !ctx.is_holiday_map.get(&(d.day())).cloned().unwrap_or(false)
                        })
                        .count() as i32
                } else {
                    0
                };
                let min_allowed = (target - config.fixed_off_shortage_allowance - spec_off_cnt).max(0);

                let slack_fixed_shortage = vars.add(variable().min(0.0));
                constraints.push(constraint!(month_work_sum.clone() + slack_fixed_shortage >= min_allowed as f64));
                objective += slack_fixed_shortage * WEIGHT_FIXED_OFF_SHORTAGE as f64;

                // target に対する不足ペナルティ（早上がり重複補填ペナルティ等より上位にして不当な出勤削減を防止）
                let slack_under_target = vars.add(variable().min(0.0));
                constraints.push(constraint!(month_work_sum + slack_under_target >= target as f64));
                objective += slack_under_target * WEIGHT_FIXED_OFF_UNDER_TARGET as f64;
            } else {
                // OR タイプ (通常の主公休選択スタッフ、および空欄スタッフ)
                let candidates: Vec<u32> = if staff.basic_off_days.is_empty() {
                    vec![0, 1, 2, 3, 4] // 月〜金
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

                // 各 ISO 週において、指定公休を含まない週なら原則として主公休曜日は休み
                if !staff.is_special {
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
                                        x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)] - slide_slack <= 1.0
                                    ));
                                    objective += slide_slack * WEIGHT_MAIN_OFF_SLIDE_SLACK as f64;
                                }
                            }
                        }
                    }
                }

                // 月間出勤数
                if staff.is_special {
                    let extra_work = vars.add(variable().min(0.0));
                    constraints.push(constraint!(month_work_sum.clone() - extra_work <= target as f64));
                    objective += extra_work * WEIGHT_SPECIAL_EXTRA_WORK as f64;
                    if stage == 1 {
                        constraints.push(constraint!(month_work_sum.clone() >= target as f64));
                        constraints.push(constraint!(month_work_sum <= (target + 5) as f64));
                    } else {
                        let slack_under = vars.add(variable().min(0.0));
                        let slack_over = vars.add(variable().min(0.0));
                        constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                        constraints.push(constraint!(month_work_sum - slack_over <= (target + 5) as f64));
                        objective += (slack_under + slack_over) * WEIGHT_TARGET_DAYS_SLACK as f64;
                    }
                } else if stage == 1 {
                    constraints.push(constraint!(month_work_sum == target as f64));
                } else {
                    let slack_under = vars.add(variable().min(0.0));
                    let slack_over = vars.add(variable().min(0.0));
                    constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                    constraints.push(constraint!(month_work_sum - slack_over <= target as f64));
                    objective += (slack_under + slack_over) * WEIGHT_TARGET_DAYS_SLACK as f64;
                }
            }
        }

        let is_week4_fixed_one = staff.pattern.contains("週4")
            && staff.basic_off_days.len() == 1
            && staff.basic_off_type == OffType::And;

        // 4. 連勤上限制約 (月またぎ連勤含む)
        let max_k = config.max_consecutive_work_days; // 5
        let tail_slice = sanitized_prev_tails
            .get(s_name)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let tail_take = tail_slice.len();

        // 窓長 k+1 の全スライディングウィンドウに対し sum <= k
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
                if stage == 1 {
                    constraints.push(constraint!(sum_expr <= max_k as f64));
                } else {
                    let consec_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_expr - consec_slack <= max_k as f64));
                    objective += consec_slack * WEIGHT_CONSEC_WORK_SLACK as f64;
                }
            }
        }

        // 5. 3連休抑制制約 (診療日ベース、有休・特別有休・休診日を除く通常の公休3連休を抑制)
        let consec_weight = config.consecutive_off_weight();
        let active_c_days_for_consec: Vec<u32> = ctx
            .clinic_days
            .iter()
            .copied()
            .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
            .collect();
        if consec_weight > 0 && active_c_days_for_consec.len() >= 3 {
            for i in 0..active_c_days_for_consec.len() - 2 {
                let d1 = active_c_days_for_consec[i];
                let d2 = active_c_days_for_consec[i + 1];
                let d3 = active_c_days_for_consec[i + 2];

                let has_vac_in_triple = [d1, d2, d3].iter().any(|&d| {
                    let dt = ctx.date_map[&d];
                    staff.vacation_dates.contains(&dt) || staff.special_vacation_dates.contains(&dt)
                });
                if has_vac_in_triple {
                    continue;
                }

                let o_slack = vars.add(variable().min(0.0));
                // o_slack >= 1 - (x1 + x2 + x3)
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

        // 6. 主公休不一致ペナルティ (有休・特有・指定公休・固定公休・共通休診日を除く診療日；石橋さんは専用ルールのため除外)
        if staff.is_core && staff.pattern != "除外" && s_name != "石橋" {
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
                    // m_slack >= 1 - x[s, d] - y[s, w]
                    constraints.push(constraint!(
                        x_vars[&(s_name.clone(), d)] + y_vars[&(s_name.clone(), w)] + m_slack >= 1.0
                    ));
                    objective += m_slack * WEIGHT_MAIN_OFF_MISMATCH as f64;
                }
            }

            // 過去最大3ヶ月分の主公休ローテーション減衰ペナルティ (AND固定公休者は対象外)
            let is_pure_fixed_and = staff.basic_off_type == OffType::And
                && !staff.basic_off_days.is_empty()
                && !is_week4_fixed_one;
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
                let multipliers: [i64; 3] = if is_week4_fixed_one {
                    [40, 20, 10] // [12,000,000, 6,000,000, 3,000,000]
                } else {
                    [4, 2, 1] // [1,200,000, 600,000, 300,000]
                };
                let mut seen_w = HashSet::new();
                for (idx, &prev_w) in history.iter().take(3).enumerate() {
                    if prev_w <= 5 && seen_w.insert(prev_w) {
                        let rot_weight = WEIGHT_PREV_OFF_REPEAT * multipliers[idx];
                        objective += y_vars[&(s_name.clone(), prev_w)] * rot_weight as f64;
                    }
                }
            }
        }

        // 7. 週単位の公休平準化・7日ローリングウィンドウ・月末公休偏り抑制（石橋さんは専用週4固定ルールのため除外）
        if staff.is_core && staff.pattern != "除外" && s_name != "石橋" {
            if config.weekly_off_balance_rule {
                // 7a. ISO週単位の公休平準化 (月端の5稼働日未満の端数週や休診週は除外)
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

                    let target_c_off = if staff.pattern.contains("週4") && !staff.is_special {
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
                    // u_abs >= (c_len - sum_work_c) - target_c_off
                    constraints.push(constraint!(
                        u_abs_expr.clone() + sum_work_c.clone() >= c_len - target_c_off
                    ));
                    // u_abs >= target_c_off - (c_len - sum_work_c)
                    constraints.push(constraint!(
                        u_abs_expr - sum_work_c >= target_c_off - c_len
                    ));

                    objective += u_abs * WEIGHT_WEEKLY_OFF_DIFF as f64;
                }

                // 7b. 7暦日ローリングウィンドウ平準化 (WEIGHT_ROLLING_WINDOW_DIFF)
                if ctx.days_in_month >= 7 {
                    let rolling_target_off = if staff.pattern.contains("週4") && !staff.is_special {
                        (config.weekly_off_target + 1) as f64
                    } else {
                        config.weekly_off_target as f64
                    };
                    for start_d in 1..=(ctx.days_in_month - 6) {
                        let has_closure = (start_d..=start_d + 6)
                            .any(|d| common_sp_vac.contains(&ctx.date_map[&d]));
                        if has_closure {
                            continue;
                        }
                        let win_work: Expression = (start_d..=start_d + 6)
                            .map(|d| x_vars[&(s_name.clone(), d)])
                            .sum();
                        let r_abs = vars.add(variable().min(0.0));
                        let r_abs_expr = Expression::from(r_abs);
                        constraints.push(constraint!(
                            r_abs_expr.clone() + win_work.clone() >= 7.0 - rolling_target_off
                        ));
                        constraints.push(constraint!(
                            r_abs_expr - win_work >= rolling_target_off - 7.0
                        ));
                        objective += r_abs * WEIGHT_ROLLING_WINDOW_DIFF as f64;
                    }
                }
            }

            // 7c. 月末3稼働日の公休集中抑制 (WEIGHT_TAIL_OFF_SUM)
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
                let is_forced_off = staff.vacation_dates.contains(&date)
                    || staff.special_vacation_dates.contains(&date)
                    || staff.preferred_off_dates.contains(&date)
                    || (config.specified_off_rule && staff.specified_off_dates.contains(&date))
                    || (staff.basic_off_type == OffType::And && staff.basic_off_days.contains(&w));
                if !is_forced_off {
                    objective += (Expression::from(1.0) - x_vars[&(s_name.clone(), d)])
                        * WEIGHT_TAIL_OFF_SUM as f64;
                }
            }
        }

        // 8. 祝日週の振替公休・最低1日公休制約 (holiday_week_off_rule；石橋さんは祝日非連動で週4維持のため除外)
        if config.holiday_week_off_rule && staff.is_core && staff.pattern != "除外" && s_name != "石橋" {
            let target = target_days_dict.get(s_name).copied().unwrap_or(0);
            let mut sum_weekly_caps_with_rule = 0i32;
            let mut streak = trailing_prev_ones_map.get(s_name).copied().unwrap_or(0);
            let max_k = config.max_consecutive_work_days;
            for week in &ctx.weeks {
                let mut w_avail = 0i32;
                for &d in &week.days {
                    let date = ctx.date_map[&d];
                    let w = ctx.weekday_map[&d];
                    let is_closed =
                        w == 6 || ctx.is_holiday_map[&d] || common_sp_vac.contains(&date);
                    let is_leave = staff.vacation_dates.contains(&date)
                        || staff.special_vacation_dates.contains(&date)
                        || staff.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && staff.specified_off_dates.contains(&date));
                    let is_fixed_basic =
                        staff.basic_off_type == OffType::And && staff.basic_off_days.contains(&w);
                    if is_closed || is_leave || is_fixed_basic || (max_k > 0 && streak >= max_k) {
                        streak = 0;
                    } else {
                        streak += 1;
                        w_avail += 1;
                    }
                }
                if week.has_holiday && week.clinic_days.len() == 5 {
                    w_avail = w_avail.min(4);
                }
                if max_k > 0 {
                    w_avail = w_avail.min(max_k as i32);
                }
                sum_weekly_caps_with_rule += w_avail;
            }
            let can_enforce_hard = sum_weekly_caps_with_rule >= target;

            for week in &ctx.weeks {
                // 祝日を含み、診療日数が5日の週
                if week.has_holiday && week.clinic_days.len() == 5 {
                    let week_work: Expression = week.clinic_days.iter().map(|&d| x_vars[&(s_name.clone(), d)]).sum();
                    if stage == 1 && can_enforce_hard {
                        // 最大4日出勤（最低1日公休）
                        constraints.push(constraint!(week_work <= 4.0));
                        hard_holiday_week_off.insert((s_name.clone(), week.week_index));
                    } else {
                        let sub_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(week_work - sub_slack <= 4.0));
                        objective += sub_slack * WEIGHT_SUBSTITUTE_OFF_SLACK as f64;
                    }
                }
            }
        }
    }

    // スタッフ別の当日出勤可能判定・週次/月次供給上限ヘルパー（Stage 1 における過剰制約・鳩の巣原理クラッシュ防止用）
    let day_to_week_idx: HashMap<u32, usize> = ctx
        .weeks
        .iter()
        .flat_map(|w| w.days.iter().map(move |&d| (d, w.week_index)))
        .collect();

    let is_staff_avail_on_d = |s: &Staff, d: u32| -> bool {
        if s.pattern == "除外" {
            return false;
        }
        let date = ctx.date_map[&d];
        let w = ctx.weekday_map[&d];
        if w == 6 || ctx.is_holiday_map[&d] || common_sp_vac.contains(&date) {
            return false;
        }
        let fv = s.vacation_dates.contains(&date)
            || s.special_vacation_dates.contains(&date)
            || s.preferred_off_dates.contains(&date)
            || (config.specified_off_rule && s.specified_off_dates.contains(&date));
        if fv {
            return false;
        }
        let tr_ones = trailing_prev_ones_map.get(&s.name).copied().unwrap_or(0);
        let is_ope = is_ope_date(&data.ope_dict, &date);
        let is_inj = is_injection_date(&data.ope_dict, &date);
        if d == 1
            && config.max_consecutive_work_days > 0
            && tr_ones >= config.max_consecutive_work_days
            && !(s.name == "石橋" && (is_ope || is_inj))
        {
            return false;
        }
        if s.name == "石橋" {
            if ishibashi_forced_wed_offs.contains(&d) {
                return false;
            }
            let fb = s.basic_off_days.contains(&w) && !is_ope && !is_inj;
            !fb
        } else {
            let fb = (s.basic_off_type == OffType::And || !s.is_core)
                && s.basic_off_days.contains(&w);
            !fb
        }
    };

    let staff_week_cap = |s: &Staff, week_idx: usize, for_2f_back_outpatient: bool| -> i32 {
        let week = &ctx.weeks[week_idx];
        let max_k = config.max_consecutive_work_days;
        let mut streak = if week_idx == 0 {
            trailing_prev_ones_map.get(&s.name).copied().unwrap_or(0)
        } else {
            0
        };
        let mut cap = 0i32;
        for &d in &week.days {
            if is_staff_avail_on_d(s, d) {
                if max_k > 0 && streak >= max_k {
                    streak = 0;
                } else {
                    streak += 1;
                    cap += 1;
                }
            } else {
                streak = 0;
            }
        }
        if max_k > 0 {
            cap = cap.min(max_k as i32);
        }
        if hard_holiday_week_off.contains(&(s.name.clone(), week_idx)) {
            cap = cap.min(4);
        }
        if s.name == "石橋" {
            if let Some(plan) = ishibashi_plans.get(week_idx) {
                cap = cap.min(plan.week_target);
            }
            if for_2f_back_outpatient {
                let ope_in_w = week
                    .clinic_days
                    .iter()
                    .filter(|&&d| {
                        is_ope_date(&data.ope_dict, &ctx.date_map[&d]) && is_staff_avail_on_d(s, d)
                    })
                    .count() as i32;
                cap = (cap - ope_in_w).max(0);
            }
        }
        cap
    };

    let staff_month_cap = |s: &Staff, for_2f_back_outpatient: bool| -> i32 {
        let target = target_days_dict.get(&s.name).copied().unwrap_or(0);
        let base_cap = if s.is_special {
            let max_w = compute_staff_max_workable_days(s, data, ctx, &common_sp_vac);
            (target + 5).min(max_w)
        } else {
            target
        };
        if for_2f_back_outpatient && s.name == "石橋" {
            let ope_in_m = ctx
                .clinic_days
                .iter()
                .filter(|&&d| {
                    is_ope_date(&data.ope_dict, &ctx.date_map[&d]) && is_staff_avail_on_d(s, d)
                })
                .count() as i32;
            (base_cap - ope_in_m).max(0)
        } else {
            base_cap
        }
    };

    let evaluate_group_hard_weeks =
        |group_staffs: &[&Staff],
         for_2f_back_outpatient: bool,
         day_reqs: &HashMap<u32, (i32, Vec<String>)>,
         forced_work_days: &mut HashMap<String, HashSet<u32>>|
         -> HashSet<usize> {
            let mut feasible_weeks = HashSet::new();
            let max_k = config.max_consecutive_work_days;

            for week in &ctx.weeks {
                let w_idx = week.week_index;
                let active_days: Vec<u32> = week
                    .clinic_days
                    .iter()
                    .copied()
                    .filter(|d| day_reqs.get(d).map(|(req, _)| *req > 0).unwrap_or(false))
                    .collect();
                if active_days.is_empty() {
                    feasible_weeks.insert(w_idx);
                    continue;
                }

                // 当該週で tight (eff_min == avail_len) となる日を抽出
                let mut new_tight_by_staff: HashMap<String, HashSet<u32>> = HashMap::new();
                for &d in &active_days {
                    let (req, avail_names) = &day_reqs[&d];
                    if *req > 0 && *req as usize >= avail_names.len() {
                        for name in avail_names {
                            new_tight_by_staff.entry(name.clone()).or_default().insert(d);
                        }
                    }
                }

                let mut week_ok = true;

                // 1. 各スタッフの週次上限・月次上限・連勤上限チェック
                for &s in group_staffs {
                    let existing_forced = forced_work_days.get(&s.name).cloned().unwrap_or_default();
                    let added_tight = new_tight_by_staff.get(&s.name).cloned().unwrap_or_default();
                    let union_forced: HashSet<u32> =
                        existing_forced.union(&added_tight).copied().collect();

                    let total_forced_in_w = week
                        .clinic_days
                        .iter()
                        .filter(|d| union_forced.contains(d))
                        .count() as i32;
                    if total_forced_in_w > staff_week_cap(s, w_idx, false) {
                        week_ok = false;
                        break;
                    }

                    if for_2f_back_outpatient {
                        let out_forced_in_w = week
                            .clinic_days
                            .iter()
                            .filter(|&&d| {
                                union_forced.contains(&d)
                                    && !(s.name == "石橋"
                                        && is_ope_date(&data.ope_dict, &ctx.date_map[&d]))
                            })
                            .count() as i32;
                        if out_forced_in_w > staff_week_cap(s, w_idx, true) {
                            week_ok = false;
                            break;
                        }
                    }

                    if union_forced.len() as i32 > staff_month_cap(s, false) {
                        week_ok = false;
                        break;
                    }

                    if max_k > 0 {
                        let mut streak = trailing_prev_ones_map.get(&s.name).copied().unwrap_or(0);
                        for &d in &ctx.all_days {
                            if union_forced.contains(&d) {
                                streak += 1;
                                if streak > max_k {
                                    week_ok = false;
                                    break;
                                }
                            } else {
                                streak = 0;
                            }
                        }
                        if !week_ok {
                            break;
                        }
                    }
                }

                // 2. 非tight日を含めた週次グループ総需要 <= 週次グループ供給上限チェック
                if week_ok {
                    let mut rem_demand_in_w = 0i32;
                    let mut non_tight_days_in_w = Vec::new();
                    for &d in &active_days {
                        let (req, avail_names) = &day_reqs[&d];
                        if (*req as usize) < avail_names.len() {
                            let already_forced_cnt = avail_names
                                .iter()
                                .filter(|name| {
                                    forced_work_days
                                        .get(*name)
                                        .map(|set| set.contains(&d))
                                        .unwrap_or(false)
                                        || new_tight_by_staff
                                            .get(*name)
                                            .map(|set| set.contains(&d))
                                            .unwrap_or(false)
                                })
                                .count() as i32;
                            rem_demand_in_w += (*req - already_forced_cnt).max(0);
                            non_tight_days_in_w.push(d);
                        }
                    }

                    let mut rem_supply_in_w = 0i32;
                    for &s in group_staffs {
                        let existing_forced =
                            forced_work_days.get(&s.name).cloned().unwrap_or_default();
                        let added_tight =
                            new_tight_by_staff.get(&s.name).cloned().unwrap_or_default();
                        let used_in_w = week
                            .clinic_days
                            .iter()
                            .filter(|&&d| {
                                (existing_forced.contains(&d) || added_tight.contains(&d))
                                    && !(for_2f_back_outpatient
                                        && s.name == "石橋"
                                        && is_ope_date(&data.ope_dict, &ctx.date_map[&d]))
                            })
                            .count() as i32;
                        let rem_cap =
                            (staff_week_cap(s, w_idx, for_2f_back_outpatient) - used_in_w).max(0);
                        let unforced_avail_on_non_tight = non_tight_days_in_w
                            .iter()
                            .filter(|&&d| {
                                day_reqs[&d].1.contains(&s.name)
                                    && !existing_forced.contains(&d)
                                    && !added_tight.contains(&d)
                            })
                            .count() as i32;
                        rem_supply_in_w += rem_cap.min(unforced_avail_on_non_tight);
                    }

                    if rem_demand_in_w > rem_supply_in_w {
                        week_ok = false;
                    }
                }

                if week_ok {
                    feasible_weeks.insert(w_idx);
                    for (name, days) in new_tight_by_staff {
                        forced_work_days.entry(name).or_default().extend(days);
                    }
                }
            }

            // 3. 月次グループ総需要 <= 月次グループ供給上限チェック
            let total_month_demand: i32 = day_reqs
                .iter()
                .filter(|(&d, _)| {
                    day_to_week_idx
                        .get(&d)
                        .map(|w_idx| feasible_weeks.contains(w_idx))
                        .unwrap_or(false)
                })
                .map(|(_, (req, _))| *req)
                .sum();
            let total_month_supply: i32 = group_staffs
                .iter()
                .map(|&s| staff_month_cap(s, for_2f_back_outpatient))
                .sum();
            if total_month_demand > total_month_supply {
                feasible_weeks.clear();
            }

            feasible_weeks
        };

    // 9. セクション出勤人数の平準化・許容ブレ幅・基本人数とペナルティ (全員共通特別有休の休診日は除外)
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
        .filter(|s| s.section == "2階裏" && (s.is_core || s.name == "石橋") && s.pattern != "除外")
        .collect();
    let num_core_2f_b = core_2f_back.len();
    let nurses_2f_back: Vec<&Staff> = core_2f_back
        .iter()
        .copied()
        .filter(|s| s.is_nurse())
        .collect();
    let exams_2f_back: Vec<&Staff> = core_2f_back
        .iter()
        .copied()
        .filter(|s| s.is_exam())
        .collect();

    // 2階裏（外来）の日次要求人数を事前計算し、週次/月次供給量との整合性（Stage 1 ハード適用可能週）を判定
    let mut day_reqs_2f_total: HashMap<u32, (i32, Vec<String>)> = HashMap::new();
    let mut day_reqs_2f_nurse: HashMap<u32, (i32, Vec<String>)> = HashMap::new();
    let mut day_reqs_2f_exam: HashMap<u32, (i32, Vec<String>)> = HashMap::new();

    if num_core_2f_b > 0 {
        for &d in &ctx.clinic_days {
            let date = ctx.date_map[&d];
            if common_sp_vac.contains(&date) {
                continue;
            }
            let w = ctx.weekday_map[&d];
            let is_ope = is_ope_date(&data.ope_dict, &date);
            let outpatient_2f_b: Vec<&Staff> = core_2f_back
                .iter()
                .copied()
                .filter(|s| !(is_ope && s.name == "石橋"))
                .collect();
            if outpatient_2f_b.is_empty() {
                continue;
            }

            let avail_total_names: Vec<String> = outpatient_2f_b
                .iter()
                .filter(|&&s| is_staff_avail_on_d(s, d))
                .map(|s| s.name.clone())
                .collect();
            let avail_nurse_names: Vec<String> = outpatient_2f_b
                .iter()
                .filter(|&&s| s.is_nurse() && is_staff_avail_on_d(s, d))
                .map(|s| s.name.clone())
                .collect();
            let avail_exam_names: Vec<String> = outpatient_2f_b
                .iter()
                .filter(|&&s| s.is_exam() && is_staff_avail_on_d(s, d))
                .map(|s| s.name.clone())
                .collect();

            let vac_2f_b_on_d = outpatient_2f_b
                .iter()
                .filter(|s| {
                    s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date))
                })
                .count() as i32;

            let (mut req_total_min, mut req_nurse_min, mut req_exam_min) = if is_ope {
                (
                    3.max(config.sec_2f_back_base_cap),
                    1.max(config.nurse_min_count),
                    2,
                )
            } else {
                (
                    (config.sec_2f_back_base_cap - vac_2f_b_on_d).max(0),
                    config.nurse_min_count.max(0),
                    0,
                )
            };

            match w {
                1 => {
                    req_nurse_min = req_nurse_min.max(2);
                    req_exam_min = req_exam_min.max(1);
                    req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
                }
                3 => {
                    req_nurse_min = req_nurse_min.max(1);
                    req_exam_min = req_exam_min.max(2);
                    req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
                }
                5 => {
                    let has_pm_leave_2f_b = core_2f_back
                        .iter()
                        .any(|s| s.pm_leave_dates.contains(&date));
                    if has_pm_leave_2f_b && !is_ope {
                        req_total_min = 2;
                    } else {
                        req_total_min = req_total_min.max(3.max(config.sec_2f_back_base_cap));
                    }
                }
                _ => {}
            }

            let eff_total = req_total_min.min(avail_total_names.len() as i32);
            let eff_nurse = req_nurse_min.min(avail_nurse_names.len() as i32);
            let eff_exam = req_exam_min.min(avail_exam_names.len() as i32);

            day_reqs_2f_total.insert(d, (eff_total, avail_total_names));
            day_reqs_2f_nurse.insert(d, (eff_nurse, avail_nurse_names));
            day_reqs_2f_exam.insert(d, (eff_exam, avail_exam_names));
        }
    }

    let hard_weeks_2f_exam =
        evaluate_group_hard_weeks(&exams_2f_back, true, &day_reqs_2f_exam, &mut forced_work_days);
    let hard_weeks_2f_nurse = evaluate_group_hard_weeks(
        &nurses_2f_back,
        true,
        &day_reqs_2f_nurse,
        &mut forced_work_days,
    );
    let hard_weeks_2f_total =
        evaluate_group_hard_weeks(&core_2f_back, true, &day_reqs_2f_total, &mut forced_work_days);

    if stage == 1 && num_core_2f_b > 0 {
        let total_weeks = ctx.weeks.len();
        if hard_weeks_2f_total.len() < total_weeks
            || hard_weeks_2f_nurse.len() < total_weeks
            || hard_weeks_2f_exam.len() < total_weeks
        {
            logs.push(
                "⚠️ [事前診断] 2階裏（外来）の週次・月次供給上限（公休・有休・連勤制限）に対し日次最低人数の合計が超過する週を検出したため、該当週をスラック高優先維持モードへ自動切替しました。"
                    .to_string(),
            );
        }
    }

    for &d in &ctx.clinic_days {
        let date = ctx.date_map[&d];
        if common_sp_vac.contains(&date) {
            continue;
        }
        let w = ctx.weekday_map[&d];
        let w_idx = day_to_week_idx.get(&d).copied().unwrap_or(0);

        // --------------------------------------------------------------------
        // (A) 1階: 絶対偏差 + 二乗偏差 + 許容ブレ幅 (sec_capacity_tolerance) + 6名以下ペナルティ
        // --------------------------------------------------------------------
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

            // 二乗偏差 q_dev = u_abs^2 (整数凸区分線形表現)
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

            // セクション人数ブレ幅 (sec_capacity_tolerance) 逸脱スラックペナルティ
            let vac_1f_on_d = staffs_1f
                .iter()
                .filter(|s| {
                    s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date))
                })
                .count();
            let avail_1f_on_d = staffs_1f
                .iter()
                .filter(|&&s| is_staff_avail_on_d(s, d))
                .count();
            let min_1f = (mean_1f.floor()
                - config.sec_capacity_tolerance as f64
                - vac_1f_on_d as f64)
                .max(0.0)
                .min(avail_1f_on_d as f64);
            let max_1f = mean_1f.ceil() + config.sec_capacity_tolerance as f64;
            let slack_1f_under = vars.add(variable().min(0.0));
            let slack_1f_over = vars.add(variable().min(0.0));
            constraints.push(constraint!(sum_1f.clone() + slack_1f_under >= min_1f));
            constraints.push(constraint!(sum_1f.clone() - slack_1f_over <= max_1f));
            objective += (slack_1f_under + slack_1f_over) * WEIGHT_GENERAL_SEC_SLACK as f64;

            // 1階 6名以下ペナルティ (mean_1f >= 6.5 のとき K <= 6 に 1.5M ペナルティ)
            if mean_1f >= 6.5 {
                let b_under7 = vars.add(variable().binary());
                // sum_1f + 7 * b_under7 >= 7
                constraints.push(constraint!(sum_1f + b_under7 * 7.0 >= 7.0));
                objective += b_under7 * WEIGHT_1F_UNDER_7_PENALTY as f64;
            }
        }

        // --------------------------------------------------------------------
        // (B) 2階表: 絶対偏差 + 二乗偏差 + 許容ブレ幅 (sec_capacity_tolerance)
        // --------------------------------------------------------------------
        let sum_2f_front: Expression = staffs_2f_front
            .iter()
            .map(|s| x_vars[&(s.name.clone(), d)])
            .sum();

        if let Some(&mean_2f_f) = ideal_mean.get("2階表") {
            let m_round = mean_2f_f.round();
            let u_abs = vars.add(variable().min(0.0));
            let u_abs_expr = Expression::from(u_abs);
            constraints.push(constraint!(u_abs_expr.clone() + sum_2f_front.clone() >= m_round));
            constraints.push(constraint!(u_abs_expr.clone() - sum_2f_front.clone() >= -m_round));
            objective += u_abs * WEIGHT_SEC_BALANCE_ABS as f64;

            // 二乗偏差 q_dev = u_abs^2
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

            // セクション人数ブレ幅 (sec_capacity_tolerance) 逸脱スラックペナルティ
            let vac_2f_f_on_d = staffs_2f_front
                .iter()
                .filter(|s| {
                    s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date))
                })
                .count();
            let avail_2f_f_on_d = staffs_2f_front
                .iter()
                .filter(|&&s| is_staff_avail_on_d(s, d))
                .count();
            let min_2f_f = (mean_2f_f.floor()
                - config.sec_capacity_tolerance as f64
                - vac_2f_f_on_d as f64)
                .max(0.0)
                .min(avail_2f_f_on_d as f64);
            let max_2f_f = mean_2f_f.ceil() + config.sec_capacity_tolerance as f64;
            let slack_2f_f_under = vars.add(variable().min(0.0));
            let slack_2f_f_over = vars.add(variable().min(0.0));
            constraints.push(constraint!(sum_2f_front.clone() + slack_2f_f_under >= min_2f_f));
            constraints.push(constraint!(sum_2f_front - slack_2f_f_over <= max_2f_f));
            objective += (slack_2f_f_under + slack_2f_f_over) * WEIGHT_GENERAL_SEC_SLACK as f64;
        }

        // --------------------------------------------------------------------
        // (C) 2階裏（外来）: Ope日の石橋さん除外(2-2)、Ope日・曜日別人数/職種バランス(3-1, 3-2)、早上がり重複最小化＆フルタイム+1増員(3-3)
        // --------------------------------------------------------------------
        let is_ope = is_ope_date(&data.ope_dict, &date);

        // 仕様 2-2: Ope日（注射以外の手術がある日）は石橋さんが手術対応に入るため、2階裏（外来）カウントから除外
        let outpatient_2f_b: Vec<&Staff> = core_2f_back
            .iter()
            .copied()
            .filter(|s| !(is_ope && s.name == "石橋"))
            .collect();
        let num_out_2f_b = outpatient_2f_b.len();

        let sum_2f_back: Expression = outpatient_2f_b
            .iter()
            .map(|s| x_vars[&(s.name.clone(), d)])
            .sum();

        if let Some(&mean_2f_b) = ideal_mean.get("2階裏") {
            let m_round = mean_2f_b.round();
            let u_abs = vars.add(variable().min(0.0));
            let u_abs_expr = Expression::from(u_abs);
            constraints.push(constraint!(u_abs_expr.clone() + sum_2f_back.clone() >= m_round));
            constraints.push(constraint!(u_abs_expr.clone() - sum_2f_back.clone() >= -m_round));
            objective += u_abs * WEIGHT_SEC_BALANCE_ABS as f64;

            // 二乗偏差 q_dev = u_abs^2 (平均 m_round 中心のため特別枠の出勤増を不当に阻害しない)
            let max_dev_2f_b = num_out_2f_b.max(1);
            let q_dev_2f_b = vars.add(variable().min(0.0));
            let q_dev_2f_b_expr = Expression::from(q_dev_2f_b);
            for m in 1..=max_dev_2f_b {
                let slope = (2 * m - 1) as f64;
                let intercept = (m * (m - 1)) as f64;
                constraints.push(constraint!(
                    q_dev_2f_b_expr.clone() - u_abs_expr.clone() * slope >= -intercept
                ));
            }
            objective += q_dev_2f_b * WEIGHT_SEC_BALANCE_SQ as f64;
        }

        // 当日外来に物理的に出勤可能なスタッフ判定（確定休暇・固定公休・石橋さんの条件付水曜公休・月跨ぎ連勤上限を考慮）
        let is_avail_out_2f_b = |s: &Staff| -> bool { is_staff_avail_on_d(s, d) };

        if num_core_2f_b > 0 && num_out_2f_b > 0 {
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

            let eff_total_min = day_reqs_2f_total.get(&d).map(|(r, _)| *r).unwrap_or(0);
            let eff_nurse_min = day_reqs_2f_nurse.get(&d).map(|(r, _)| *r).unwrap_or(0);
            let eff_exam_min = day_reqs_2f_exam.get(&d).map(|(r, _)| *r).unwrap_or(0);

            // (1) 2階裏（外来）日次最低総人数制約
            if eff_total_min > 0 {
                if stage == 1 && hard_weeks_2f_total.contains(&w_idx) {
                    constraints.push(constraint!(sum_2f_back.clone() >= eff_total_min as f64));
                } else {
                    let b2_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_2f_back.clone() + b2_slack >= eff_total_min as f64
                    ));
                    objective += b2_slack * WEIGHT_2F_BACK_MIN_SLACK as f64;
                }
            }

            // (2) 2階裏（外来）看護師最低人数制約
            if eff_nurse_min > 0 {
                if stage == 1 && hard_weeks_2f_nurse.contains(&w_idx) {
                    constraints.push(constraint!(sum_nurses.clone() >= eff_nurse_min as f64));
                } else {
                    let n_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_nurses.clone() + n_slack >= eff_nurse_min as f64
                    ));
                    objective += n_slack * WEIGHT_2F_BACK_ROLE_MIN_SLACK as f64;
                }
            }

            // (3) 2階裏（外来）検査スタッフ最低人数制約
            if eff_exam_min > 0 {
                if stage == 1 && hard_weeks_2f_exam.contains(&w_idx) {
                    constraints.push(constraint!(sum_exams.clone() >= eff_exam_min as f64));
                } else {
                    let e_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(
                        sum_exams.clone() + e_slack >= eff_exam_min as f64
                    ));
                    objective += e_slack * WEIGHT_2F_BACK_ROLE_MIN_SLACK as f64;
                }
            }

            // (4) ソフト推奨目標: Ope日は「石橋さん以外に 看護師2人＋検査2人（計4人）」を強く優遇（仕様 3-1）
            if is_ope {
                if nurses_out.len() >= 2 {
                    let ope_n_rec_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_nurses + ope_n_rec_slack >= 2.0));
                    objective += ope_n_rec_slack * WEIGHT_OPE_2F_BACK_REC_PENALTY as f64;
                }
                if exams_out.len() >= 2 {
                    let ope_e_rec_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_exams + ope_e_rec_slack >= 2.0));
                    objective += ope_e_rec_slack * WEIGHT_OPE_2F_BACK_REC_PENALTY as f64;
                }
            } else if config.nurse_rec_count > eff_nurse_min && nurses_out.len() >= 2 {
                // 非Ope日の看護師推奨人数 (nurse_rec_count = 2)
                let n_rec_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(
                    sum_nurses + n_rec_slack >= config.nurse_rec_count as f64
                ));
                objective += n_rec_slack * WEIGHT_NURSE_LT2_PENALTY as f64;
            }

            // (5) 仕様 3-3: 早上がりスタッフ（長谷川さん 16:15・石橋さん 17:00）の外来同時出勤抑制とフルタイム増員
            if !is_ope {
                let has_hase = outpatient_2f_b.iter().any(|s| s.name == "長谷川");
                let has_ishi = outpatient_2f_b.iter().any(|s| s.name == "石橋");
                if has_hase && has_ishi {
                    let x_hase = x_vars[&("長谷川".to_string(), d)];
                    let x_ishi = x_vars[&("石橋".to_string(), d)];
                    let both_early = vars.add(variable().binary());

                    // both_early = x_hase AND x_ishi
                    constraints.push(constraint!(both_early - x_hase - x_ishi >= -1.0));
                    constraints.push(constraint!(both_early - x_hase <= 0.0));
                    constraints.push(constraint!(both_early - x_ishi <= 0.0));

                    // 3-3(1) 重複の最小化（ソフト制約）
                    objective += both_early * WEIGHT_EARLY_OVERLAP_PENALTY as f64;

                    // 3-3(2) 重複時のフルタイム増員（通常勤務スタッフをプラス1人多く配置＝フルタイム2名以上、外来計4名以上）
                    let fulltime_out: Vec<&Staff> = outpatient_2f_b
                        .iter()
                        .copied()
                        .filter(|s| s.name != "長谷川" && s.name != "石橋")
                        .collect();
                    let avail_ft = fulltime_out
                        .iter()
                        .filter(|&&s| is_avail_out_2f_b(s))
                        .count() as i32;
                    let req_ft = (config.sec_2f_back_base_cap - 1).max(2).min(avail_ft);
                    if req_ft > 0 {
                        let sum_ft: Expression = fulltime_out
                            .iter()
                            .map(|s| x_vars[&(s.name.clone(), d)])
                            .sum();
                        let slack_ft = vars.add(variable().min(0.0));
                        constraints.push(constraint!(
                            sum_ft + slack_ft - both_early * (req_ft as f64) >= 0.0
                        ));
                        objective += slack_ft * WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK as f64;
                    }
                }
            }
        }

        // 2階裏（外来）人数ワンホット変数 delta[v] (v in 0..=num_out_2f_b)
        let mut delta_vars = Vec::new();
        for _ in 0..=num_out_2f_b {
            delta_vars.push(vars.add(variable().binary()));
        }
        let delta_sum: Expression = delta_vars.iter().cloned().sum();
        constraints.push(constraint!(delta_sum == 1.0));

        let delta_val_sum: Expression = delta_vars
            .iter()
            .enumerate()
            .map(|(v, &var)| var * v as f64)
            .sum();
        constraints.push(constraint!(delta_val_sum == sum_2f_back.clone()));

        // 3名ペナルティ (300k)
        if num_out_2f_b >= 3 {
            objective += delta_vars[3] * WEIGHT_2F_BACK_3_PENALTY as f64;
        }
        // 5名ペナルティ (2M)
        if num_out_2f_b >= 5 {
            objective += delta_vars[5] * WEIGHT_2F_BACK_5_PENALTY as f64;
        }
    }

    // 2階裏 通常週（祝日・共通特別有休を含まない6稼働日の週）の4名体制最低日数制約（Ope日の石橋さんは外来カウント除外）
    let core_2f_back_names: Vec<String> = data
        .staffs
        .iter()
        .filter(|s| s.section == "2階裏" && (s.is_core || s.name == "石橋") && s.pattern != "除外")
        .map(|s| s.name.clone())
        .collect();

    for week in &ctx.weeks {
        let active_clinic_days: Vec<u32> = week
            .clinic_days
            .iter()
            .copied()
            .filter(|&d| !common_sp_vac.contains(&ctx.date_map[&d]))
            .collect();
        if !week.has_holiday && active_clinic_days.len() == 6 {
            let vac_person_days_in_week: usize = core_2f_back
                .iter()
                .map(|s| {
                    active_clinic_days
                        .iter()
                        .filter(|&&d| {
                            let dt = ctx.date_map[&d];
                            s.vacation_dates.contains(&dt)
                                || s.special_vacation_dates.contains(&dt)
                                || s.preferred_off_dates.contains(&dt)
                                || (config.specified_off_rule && s.specified_off_dates.contains(&dt))
                        })
                        .count()
                })
                .sum();
            let ope_days_in_week: usize = active_clinic_days
                .iter()
                .filter(|&&d| is_ope_date(&data.ope_dict, &ctx.date_map[&d]))
                .count();
            let week_out_supply: i32 = core_2f_back
                .iter()
                .map(|&s| staff_week_cap(s, week.week_index, true))
                .sum();
            let min_needed_for_4plus = 18 + config.sec_2f_back_weekly_4_min;

            // 各稼働日 d について、外来4名以上出勤フラグ is_4plus を定義
            let mut is_4plus_sum = Expression::from(0.0);
            for &d in &active_clinic_days {
                let dt = ctx.date_map[&d];
                let is_ope = is_ope_date(&data.ope_dict, &dt);
                let is_4plus = vars.add(variable().binary());
                let sum_day: Expression = core_2f_back_names
                    .iter()
                    .filter(|name| !(is_ope && name.as_str() == "石橋"))
                    .map(|name| x_vars[&(name.clone(), d)])
                    .sum();
                // sum_day - 4 * is_4plus >= 0 (sum_day >= 4 のとき is_4plus は 1 になれる)
                constraints.push(constraint!(sum_day - is_4plus * 4.0 >= 0.0));
                is_4plus_sum += is_4plus;
            }

            let min_4days = config.sec_2f_back_weekly_4_min as f64;
            if stage == 1
                && vac_person_days_in_week <= 1
                && ope_days_in_week <= 2
                && week_out_supply >= min_needed_for_4plus
                && hard_weeks_2f_total.contains(&week.week_index)
            {
                constraints.push(constraint!(is_4plus_sum >= min_4days));
            } else {
                let slack_4 = vars.add(variable().min(0.0));
                constraints.push(constraint!(is_4plus_sum + slack_4 >= min_4days));
                objective += slack_4 * WEIGHT_2F_BACK_WEEKLY_4_SLACK as f64;
            }
        }
    }

    // 10. シフト制約条件シート (df_rules) の各行適用
    for rule in &data.rules {
        let target_days = parse_rule_target_dates(&rule.condition_desc, ctx, &data.ope_dict);
        if target_days.is_empty() {
            continue;
        }

        let target_staff_refs: Vec<&Staff> = rule
            .target_staffs
            .iter()
            .filter_map(|name| data.staffs.iter().find(|s| &s.name == name && s.pattern != "除外"))
            .collect();
        let target_staffs: Vec<String> = target_staff_refs.iter().map(|s| s.name.clone()).collect();

        if target_staffs.is_empty() {
            continue;
        }

        let original_count = rule.target_staffs.len() as i32;
        let valid_count = target_staffs.len() as i32;
        let excluded_count = (original_count - valid_count).max(0);

        let is_simultaneous_off = rule.rule_type.contains("同時休み") || rule.name.contains("同時休み");
        let base_min = if is_simultaneous_off {
            rule.val_min.max(original_count - rule.val_min)
        } else {
            rule.val_min
        };
        let eff_min = (base_min - excluded_count).max(0);
        let eff_rec = (rule.val_rec - excluded_count).max(0);

        let is_hard = rule.level.to_uppercase().contains("HARD");
        let weight = get_level_weight(&rule.level) as f64;
        let rec_weight = get_rec_weight(&rule.level) as f64;

        let mut day_reqs_rule: HashMap<u32, (i32, Vec<String>)> = HashMap::new();
        for &d in &target_days {
            let date = ctx.date_map[&d];
            if common_sp_vac.contains(&date) {
                continue;
            }
            let avail_names: Vec<String> = target_staff_refs
                .iter()
                .filter(|&&st| is_staff_avail_on_d(st, d))
                .map(|st| st.name.clone())
                .collect();
            let day_eff_min = eff_min.min(avail_names.len() as i32);
            day_reqs_rule.insert(d, (day_eff_min, avail_names));
        }

        let hard_weeks_rule = if is_hard && stage == 1 {
            evaluate_group_hard_weeks(
                &target_staff_refs,
                false,
                &day_reqs_rule,
                &mut forced_work_days,
            )
        } else {
            HashSet::new()
        };

        for &d in &target_days {
            let date = ctx.date_map[&d];
            if common_sp_vac.contains(&date) {
                continue;
            }
            let w_idx = day_to_week_idx.get(&d).copied().unwrap_or(0);
            let day_eff_min = day_reqs_rule.get(&d).map(|(r, _)| *r).unwrap_or(0);

            let sum_target: Expression = target_staffs
                .iter()
                .map(|name| x_vars[&(name.clone(), d)])
                .sum();

            if day_eff_min > 0 {
                if is_hard && stage == 1 && hard_weeks_rule.contains(&w_idx) {
                    constraints.push(constraint!(sum_target.clone() >= day_eff_min as f64));
                } else {
                    let slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_target.clone() + slack >= day_eff_min as f64));
                    objective += slack * weight;
                }
            }

            // 推奨人数 (eff_rec > day_eff_min)
            if eff_rec > day_eff_min {
                let rec_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_target.clone() + rec_slack >= eff_rec as f64));
                objective += rec_slack * rec_weight;
            }

            // 最大人数 (val_max < 99)
            if rule.val_max < 90 {
                let max_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_target - max_slack <= rule.val_max as f64));
                objective += max_slack * weight;
            }
        }
    }

    // 11. 既存シフト維持ペナルティ
    if let Some(existing_month_shifts) = data.existing_shifts.get(&(ctx.year, ctx.month)) {
        let weight_keep = config.existing_shift_weight() as f64;
        for staff in &data.staffs {
            if let Some(staff_shifts) = existing_month_shifts.get(&staff.name) {
                for &d in &ctx.clinic_days {
                    let date = ctx.date_map[&d];
                    // 共通休診・有休・特有・希望休・指定公休は除外
                    if common_sp_vac.contains(&date)
                        || staff.vacation_dates.contains(&date)
                        || staff.special_vacation_dates.contains(&date)
                        || staff.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && staff.specified_off_dates.contains(&date))
                    {
                        continue;
                    }
                    if let Some(&prev_val) = staff_shifts.get(&d) {
                        let var = x_vars[&(staff.name.clone(), d)];
                        if prev_val == 1 {
                            // 前回復帰: (1 - x) * weight
                            objective += (Expression::from(1.0) - var) * weight_keep;
                        } else {
                            // 前回休み: x * weight
                            objective += var * weight_keep;
                        }
                    }
                }
            }
        }
    }

    // 12. ソルバーの構築と実行 (HiGHS)
    logs.push(format!(
        "Stage {}: 変数作成完了, 制約数={}",
        stage,
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
            logs.push(format!("Stage {} 最適解取得成功! (ソルバー時間: {:.2}秒)", stage, solve_duration));

            // 出勤フラグおよび主公休フラグの抽出
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
                    && s_name != "石橋";

                if (staff.is_core || s_name == "石橋") && staff.pattern != "除外" && !is_pure_fixed_and {
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

            (
                SolveResult {
                    success: true,
                    status: SolveStatus::Optimal,
                    stage,
                    shifts,
                    main_offs,
                    target_days: target_days_dict.clone(),
                    execution_time_secs: solve_duration,
                    logs,
                },
                Some(Box::new(solution)),
            )
        }
        Err(e) => {
            logs.push(format!("Stage {} ソルバー終了ステータス: {:?}", stage, e));
            (
                SolveResult {
                    success: false,
                    status: SolveStatus::Infeasible,
                    stage,
                    shifts: HashMap::new(),
                    main_offs: HashMap::new(),
                    target_days: target_days_dict.clone(),
                    execution_time_secs: solve_duration,
                    logs,
                },
                None,
            )
        }
    }
}
