//! 勤務シフト自動生成システム 二段階数理最適化ソルバー (`solver.rs`)
//!
//! Version: v1.2.0
//! Updated: 2026-11-01 - v1.2.0 全仕様監査対応: 週4×固定1日特別枠対応、2階裏基本人数・ブレ幅・二乗ペナルティ実装、3ヶ月ローテーション履歴、7日ローリング窓・月末公休抑制、3連休有休除外、OPEタイポ吸収、タイムリミット適用
//! - v1.1.0 (2026-10-31): OPE曜日条件評価順・UTF-8スライス修正、除外スタッフ人数補正、OR型主公休スライド柔軟化、固定公休y変数整合化
//! - v1.0.0 (2026-10-31): Phase 2 実装: good_lp + HiGHS (MILP) 二段階数理最適化エンジンの完全実装
//!
//! SPECIFICATION.md セクション5 に基づく厳密な線形不等式モデルと二段階探索の実装。

use crate::config::*;
use crate::parser::*;
use chrono::{Datelike, NaiveDate, Weekday};
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
        let first_day = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
        let next_month = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1).unwrap()
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap()
        };
        let days_in_month = (next_month - first_day).num_days() as u32;

        let mut all_days = Vec::new();
        let mut clinic_days = Vec::new();
        let mut date_map = HashMap::new();
        let mut weekday_map = HashMap::new();
        let mut is_holiday_map = HashMap::new();

        for d in 1..=days_in_month {
            let date = NaiveDate::from_ymd_opt(year, month, d).unwrap();
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
            year,
            month,
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

/// コアスタッフ全員共通の特別有休（年末年始・夏季休診等）の日付集合を算出
pub fn compute_common_sp_vac(data: &ParsedExcelData, ctx: &MonthContext) -> HashSet<NaiveDate> {
    let core_staffs: Vec<&Staff> = data
        .staffs
        .iter()
        .filter(|s| s.is_core && s.pattern != "除外")
        .collect();

    let mut common_sp_vac = HashSet::new();
    if let Some(first) = core_staffs.first() {
        common_sp_vac = first
            .special_vacation_dates
            .iter()
            .filter(|d| d.year() == ctx.year && d.month() == ctx.month)
            .cloned()
            .collect();
        for s in core_staffs.iter().skip(1) {
            let s_vac: HashSet<NaiveDate> = s
                .special_vacation_dates
                .iter()
                .filter(|d| d.year() == ctx.year && d.month() == ctx.month)
                .cloned()
                .collect();
            common_sp_vac = common_sp_vac.intersection(&s_vac).cloned().collect();
        }
    }
    common_sp_vac
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

        if !staff.is_core {
            // パート等非コア: 希望出勤曜日（月〜土、日曜・祝日・有休・特有・希望休・指定公休を除く）の出勤日数
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

                if !is_basic_off && !is_vac && !is_sp_vac && !is_pref_off && !is_spec_off {
                    work_days_count += 1;
                }
            }
            target_days.insert(staff.name.clone(), work_days_count);
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
            let target = (basic_slots - effective_off - sp_vac_count - vac_count).max(0);
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

        let sum_2f_back: i32 = data
            .staffs
            .iter()
            .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
            .map(|s| target_days.get(&s.name).cloned().unwrap_or(0))
            .sum();
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

    if desc_trim.contains("全日") {
        target_days = ctx.clinic_days.clone();
    } else if desc_trim.contains("手術のある日") || desc_trim.contains("日曜日以外の手術のある日") {
        target_days = ctx
            .clinic_days
            .iter()
            .filter(|&&d| ope_dict.contains_key(&ctx.date_map[&d]))
            .cloned()
            .collect();
    } else if desc_trim.contains("手術") || desc_lower.contains("ope") || desc_trim.contains("オペ") {
        // 先頭の曜日指定があるか判定 (例: "火曜(OPE日)", "水曜(OPE日)")
        let base_days = if desc_trim.contains("月曜") {
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
        };

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
            if let Some(ope_text) = ope_dict.get(&date) {
                let ope_lower = ope_text.to_lowercase();
                let matched = if kw.is_empty() {
                    true
                } else if kw_lower == "vit" || kw_lower == "viit" || kw == "硝子体" {
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
    _holidays: &HashMap<NaiveDate, String>,
    stage: u8,
) -> (SolveResult, Option<Box<dyn Solution>>) {
    let mut logs = Vec::new();
    let mut vars = ProblemVariables::new();
    let mut constraints: Vec<Constraint> = Vec::new();
    let mut objective = Expression::from(0.0);

    let config = &data.config;
    let common_sp_vac = compute_common_sp_vac(data, ctx);

    // 1. 決定変数 x[s, d] の定義 (s: スタッフ, d: 1..=days_in_month)
    let mut x_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        for &d in &ctx.all_days {
            let var = vars.add(variable().binary());
            x_vars.insert((staff.name.clone(), d), var);
        }
    }

    // 2. 決定変数 y[s, w] の定義 (w: 0=月..5=土) - コアスタッフの主公休選択フラグ
    let mut y_vars: HashMap<(String, u32), Variable> = HashMap::new();
    for staff in &data.staffs {
        if staff.is_core && staff.pattern != "除外" {
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

        // 非コアスタッフのスケジュール完全固定
        if !staff.is_core {
            for &d in &ctx.clinic_days {
                let date = ctx.date_map[&d];
                let w = ctx.weekday_map[&d];

                let is_off = staff.basic_off_days.contains(&w)
                    || staff.vacation_dates.contains(&date)
                    || staff.special_vacation_dates.contains(&date)
                    || staff.preferred_off_dates.contains(&date)
                    || (config.specified_off_rule && staff.specified_off_dates.contains(&date));

                if is_off {
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 0.0));
                } else {
                    constraints.push(constraint!(x_vars[&(s_name.clone(), d)] == 1.0));
                }
            }
            continue;
        }

        // コアスタッフの勤務パターン制約
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
            // 週4×固定1日指定者 (例: 宮澤さん、石橋さん)
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

            // 通常週4スタッフ（特別枠でない場合）は、指定公休を含まない各ISO週において原則として第2公休曜日は休み:
            // x[s, d] + y[s, w] - slide_slack <= 1.0
            // ※ 「必要に応じて週5日可」(is_special) の場合は第2公休日の出勤により週5勤務へ拡張するため slide_slack で阻害しない
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
                // 「必要に応じて週5日可」スタッフ (例: 石橋さん): target 以上、最大 target+5
                constraints.push(constraint!(month_work_sum.clone() >= target as f64));
                let extra_work = vars.add(variable().min(0.0));
                constraints.push(constraint!(month_work_sum.clone() - extra_work <= target as f64));
                constraints.push(constraint!(month_work_sum <= (target + 5) as f64));
                objective += extra_work * WEIGHT_SPECIAL_EXTRA_WORK as f64;
            } else if stage == 1 {
                constraints.push(constraint!(month_work_sum == target as f64));
            } else {
                let slack_under = vars.add(variable().min(0.0));
                let slack_over = vars.add(variable().min(0.0));
                constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                constraints.push(constraint!(month_work_sum - slack_over <= target as f64));
                objective += (slack_under + slack_over) * (WEIGHT_FIXED_OFF_SHORTAGE / 10) as f64;
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

            // target に対する不足ペナルティ
            let slack_under_target = vars.add(variable().min(0.0));
            constraints.push(constraint!(month_work_sum + slack_under_target >= target as f64));
            objective += slack_under_target * (WEIGHT_FIXED_OFF_SHORTAGE / 10) as f64;
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

            // 各 ISO 週において、指定公休を含まない週なら原則として主公休曜日は休み:
            // x[s, d] + y[s, w] - slide_slack <= 1.0 (HIGH1/HIGH2等の高優先制約と競合する週のみ一時スライドを許容)
            // ※ 「必要に応じて週5日可」(is_special) の場合は主公休日の出勤により週5勤務へ拡張するため slide_slack で阻害しない
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
                // 「必要に応じて週5」スタッフ: target 以上、最大 target+5
                constraints.push(constraint!(month_work_sum.clone() >= target as f64));
                let extra_work = vars.add(variable().min(0.0));
                constraints.push(constraint!(month_work_sum.clone() - extra_work <= target as f64));
                constraints.push(constraint!(month_work_sum <= (target + 5) as f64));
                objective += extra_work * WEIGHT_SPECIAL_EXTRA_WORK as f64;
            } else {
                if stage == 1 {
                    constraints.push(constraint!(month_work_sum == target as f64));
                } else {
                    let slack_under = vars.add(variable().min(0.0));
                    let slack_over = vars.add(variable().min(0.0));
                    constraints.push(constraint!(month_work_sum.clone() + slack_under >= target as f64));
                    constraints.push(constraint!(month_work_sum - slack_over <= target as f64));
                    objective += (slack_under + slack_over) * (WEIGHT_FIXED_OFF_SHORTAGE / 10) as f64;
                }
            }
        }

        // 4. 連勤上限制約 (月またぎ連勤含む)
        let max_k = config.max_consecutive_work_days; // 5
        let prev_tail = data
            .prev_month_tail_shifts
            .get(s_name)
            .cloned()
            .unwrap_or_default();

        // 連結列: 前月末尾 (最大 max_k 日分) + 当月全日
        let tail_take = max_k.min(prev_tail.len());
        let tail_slice = if prev_tail.len() >= tail_take {
            &prev_tail[prev_tail.len() - tail_take..]
        } else {
            &[]
        };

        // 窓長 k+1 の全スライディングウィンドウに対し sum <= k
        let total_days = ctx.days_in_month as usize;
        let padded_len = tail_take + total_days;

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

        // 6. 主公休不一致ペナルティ (有休・特有・指定公休・固定公休・共通休診日を除く診療日)
        if staff.is_core && staff.pattern != "除外" {
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

        // 7. 週単位の公休平準化・7日ローリングウィンドウ・月末公休偏り抑制
        if staff.is_core && staff.pattern != "除外" {
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

        // 8. 祝日週の振替公休・最低1日公休制約 (holiday_week_off_rule)
        if config.holiday_week_off_rule && staff.is_core && staff.pattern != "除外" {
            for week in &ctx.weeks {
                // 祝日を含み、診療日数が5日の週
                if week.has_holiday && week.clinic_days.len() == 5 {
                    let week_work: Expression = week.clinic_days.iter().map(|&d| x_vars[&(s_name.clone(), d)]).sum();
                    if stage == 1 {
                        // 最大4日出勤（最低1日公休）
                        constraints.push(constraint!(week_work <= 4.0));
                    } else {
                        let sub_slack = vars.add(variable().min(0.0));
                        constraints.push(constraint!(week_work - sub_slack <= 4.0));
                        objective += sub_slack * WEIGHT_SUBSTITUTE_OFF_SLACK as f64;
                    }
                }
            }
        }
    }

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
        .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
        .collect();
    let num_core_2f_b = core_2f_back.len();

    for &d in &ctx.clinic_days {
        let date = ctx.date_map[&d];
        if common_sp_vac.contains(&date) {
            continue;
        }
        let w = ctx.weekday_map[&d];

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
                .filter(|s| {
                    let fv = s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date));
                    let fb = (s.basic_off_type == OffType::And || !s.is_core)
                        && s.basic_off_days.contains(&w);
                    !fv && !fb
                })
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
                .filter(|s| {
                    let fv = s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date));
                    let fb = s.basic_off_type == OffType::And && s.basic_off_days.contains(&w);
                    !fv && !fb
                })
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
        // (C) 2階裏 (コアスタッフ): 絶対偏差 + 二乗偏差 + 2階裏_基本人数 + 3名/5名ペナルティ
        // --------------------------------------------------------------------
        let sum_2f_back: Expression = core_2f_back
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
            let max_dev_2f_b = num_core_2f_b.max(1);
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

        // 2階裏_基本人数 (sec_2f_back_base_cap = 3) の日次最低人数制約
        if num_core_2f_b > 0 && config.sec_2f_back_base_cap > 0 {
            let vac_2f_b_on_d = core_2f_back
                .iter()
                .filter(|s| {
                    s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date))
                })
                .count() as i32;
            let avail_2f_b_on_d = core_2f_back
                .iter()
                .filter(|s| {
                    let fv = s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || (config.specified_off_rule && s.specified_off_dates.contains(&date));
                    let fb = s.basic_off_type == OffType::And && s.basic_off_days.contains(&w);
                    !fv && !fb
                })
                .count() as i32;
            let eff_2f_min = (config.sec_2f_back_base_cap - vac_2f_b_on_d)
                .max(0)
                .min(avail_2f_b_on_d) as f64;
            if eff_2f_min > 0.0 {
                if stage == 1 {
                    constraints.push(constraint!(sum_2f_back.clone() >= eff_2f_min));
                } else {
                    let b2_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_2f_back.clone() + b2_slack >= eff_2f_min));
                    objective += b2_slack * WEIGHT_2F_BACK_MIN_SLACK as f64;
                }
            }
        }

        // 2階裏 人数ワンホット変数 delta[v] (v in 0..=num_core_2f_b)
        let mut delta_vars = Vec::new();
        for _ in 0..=num_core_2f_b {
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
        if num_core_2f_b >= 3 {
            objective += delta_vars[3] * WEIGHT_2F_BACK_3_PENALTY as f64;
        }
        // 5名ペナルティ (2M)
        if num_core_2f_b >= 5 {
            objective += delta_vars[5] * WEIGHT_2F_BACK_5_PENALTY as f64;
        }

        // 2階裏 看護師人数制約
        let nurses_2f_back: Vec<&&Staff> = core_2f_back
            .iter()
            .filter(|s| s.role.contains("看護師"))
            .collect();
        let num_nurses = nurses_2f_back.len();
        if num_nurses > 0 {
            let sum_nurses: Expression = nurses_2f_back
                .iter()
                .map(|s| x_vars[&(s.name.clone(), d)])
                .sum();

            let avail_nurses = nurses_2f_back
                .iter()
                .filter(|s| {
                    !s.vacation_dates.contains(&date)
                        && !s.special_vacation_dates.contains(&date)
                        && !s.preferred_off_dates.contains(&date)
                        && !(config.specified_off_rule && s.specified_off_dates.contains(&date))
                })
                .count();

            // 看護師最低人数 (nurse_min_count = 1、全員休暇等で物理的に0名の日は上限クランプ)
            let eff_nurse_min = (config.nurse_min_count as usize).min(avail_nurses) as f64;
            if eff_nurse_min > 0.0 {
                if stage == 1 {
                    constraints.push(constraint!(sum_nurses.clone() >= eff_nurse_min));
                } else {
                    let n_slack = vars.add(variable().min(0.0));
                    constraints.push(constraint!(sum_nurses.clone() + n_slack >= eff_nurse_min));
                    objective += n_slack * WEIGHT_NURSE_DEFICIT_SLACK as f64;
                }
            }

            // 看護師推奨人数 (nurse_rec_count = 2)
            if config.nurse_rec_count as usize > config.nurse_min_count as usize && num_nurses >= 2 {
                let n_rec_slack = vars.add(variable().min(0.0));
                constraints.push(constraint!(sum_nurses + n_rec_slack >= config.nurse_rec_count as f64));
                objective += n_rec_slack * WEIGHT_NURSE_LT2_PENALTY as f64;
            }
        }
    }

    // 2階裏 通常週（祝日・共通特別有休を含まない6稼働日の週）の4名体制最低日数制約
    let core_2f_back_names: Vec<String> = data
        .staffs
        .iter()
        .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
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
            // 各稼働日 d について、4名以上出勤フラグ is_4plus を定義
            let mut is_4plus_sum = Expression::from(0.0);
            for &d in &active_clinic_days {
                let is_4plus = vars.add(variable().binary());
                let sum_day: Expression = core_2f_back_names
                    .iter()
                    .map(|name| x_vars[&(name.clone(), d)])
                    .sum();
                // sum_day - 4 * is_4plus >= 0 (sum_day >= 4 のとき is_4plus は 1 になれる)
                constraints.push(constraint!(sum_day - is_4plus * 4.0 >= 0.0));
                is_4plus_sum += is_4plus;
            }

            let min_4days = config.sec_2f_back_weekly_4_min as f64;
            if stage == 1 {
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

        let target_staffs: Vec<String> = rule
            .target_staffs
            .iter()
            .filter(|&name| data.staffs.iter().any(|s| &s.name == name && s.pattern != "除外"))
            .cloned()
            .collect();

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

        for &d in &target_days {
            let date = ctx.date_map[&d];
            if common_sp_vac.contains(&date) {
                continue;
            }

            // 当日に確定休暇(有休・特有・希望休・指定公休・AND固定公休)で x=0 がハード固定されていない最大出勤可能人数
            let w = ctx.weekday_map[&d];
            let avail_on_d = target_staffs
                .iter()
                .filter(|&name| {
                    if let Some(st) = data.staffs.iter().find(|s| &s.name == name) {
                        let forced_vac = st.vacation_dates.contains(&date)
                            || st.special_vacation_dates.contains(&date)
                            || st.preferred_off_dates.contains(&date)
                            || (config.specified_off_rule && st.specified_off_dates.contains(&date));
                        let forced_basic = (st.basic_off_type == OffType::And || !st.is_core)
                            && st.basic_off_days.contains(&w);
                        !forced_vac && !forced_basic
                    } else {
                        false
                    }
                })
                .count() as i32;

            let day_eff_min = eff_min.min(avail_on_d);

            let sum_target: Expression = target_staffs
                .iter()
                .map(|name| x_vars[&(name.clone(), d)])
                .sum();

            if day_eff_min > 0 {
                if is_hard && stage == 1 {
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
                    && !is_week4_fixed_one;

                if staff.is_core && staff.pattern != "除外" && !is_pure_fixed_and {
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
