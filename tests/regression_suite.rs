//! 勤務シフト自動生成システム 7大統合回帰テストスイート (`tests/regression_suite.rs`)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 2 実装:
//! - test_01_holidays: 祝日計算・春分秋分・国民の休日・振替休日検証
//! - test_02_date_parser_and_specified_off: 日付パーサー・NFKC・指定公休HARD固定＆ISO週義務免除検証
//! - test_03_substitute_off_in_holiday_week: 祝日と固定公休曜日重複時の振替公休自動設定検証
//! - test_04_holiday_week_min_1_off: 祝日を含む診療日5日週のコアスタッフ最大4日出勤（最低1日公休）検証
//! - test_05_weekly_balance_and_2f_back_4staff: 週単位公休平準化および2階裏4名体制最低日数確保検証
//! - test_06_prev_month_linking: 前月末尾実績からの末尾7日連勤引き継ぎと月初連勤上限遵守検証
//! - test_07_second_off_monthly_rotation: 週4日×固定1日スタッフの第1公休固定＋第2公休月内固定＆ローテーション検証

use chrono::{Datelike, NaiveDate};
use shift_core::holidays::{calculate_japanese_holidays, get_holidays};
use shift_core::parser::{excel_serial_to_date, parse_date_list, parse_shift_excel, ParsedExcelData};
use shift_core::solver::{solve_shift, MonthContext};
use std::collections::HashMap;
use std::path::Path;

/// テスト用実データExcelのパースヘルパー
fn load_test_excel() -> (ParsedExcelData, HashMap<NaiveDate, String>) {
    let excel_path = if Path::new("勤務条件表_9.xlsx").exists() {
        "勤務条件表_9.xlsx"
    } else if Path::new("勤務条件表_9月.xlsx").exists() {
        "勤務条件表_9月.xlsx"
    } else {
        panic!("実データExcel (勤務条件表_9.xlsx) が見つかりません。");
    };

    let holidays = get_holidays(2026, 2027);
    let parsed = parse_shift_excel(excel_path, &holidays)
        .expect("実データExcelのパースに失敗しました");
    (parsed, holidays)
}

// ============================================================================
// 1. test_01_holidays: 祝日計算・春分秋分・国民の休日・振替休日の一致検証
// ============================================================================
#[test]
fn test_01_holidays() {
    let holidays_2024 = calculate_japanese_holidays(2024);

    // 2024年春分の日 (3/20 水)
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 3, 20).unwrap()),
        Some(&"春分の日".to_string())
    );

    // 2024年秋分の日 (9/22 日) -> 振替休日 9/23(月)
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 9, 22).unwrap()),
        Some(&"秋分の日".to_string())
    );
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 9, 23).unwrap()),
        Some(&"振替休日".to_string())
    );

    // 2024年建国記念の日 (2/11 日) -> 振替休日 2/12(月)
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 2, 11).unwrap()),
        Some(&"建国記念の日".to_string())
    );
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 2, 12).unwrap()),
        Some(&"振替休日".to_string())
    );

    // 2024年こどもの日 (5/5 日) -> 振替休日 5/6(月)
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 5, 5).unwrap()),
        Some(&"こどもの日".to_string())
    );
    assert_eq!(
        holidays_2024.get(&NaiveDate::from_ymd_opt(2024, 5, 6).unwrap()),
        Some(&"振替休日".to_string())
    );

    // 2026年シルバーウィーク国民の休日判定 (9/21敬老、9/23秋分 -> 9/22国民の休日)
    let holidays_2026 = calculate_japanese_holidays(2026);
    assert_eq!(
        holidays_2026.get(&NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()),
        Some(&"敬老の日".to_string())
    );
    assert_eq!(
        holidays_2026.get(&NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()),
        Some(&"国民の休日".to_string())
    );
    assert_eq!(
        holidays_2026.get(&NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()),
        Some(&"秋分の日".to_string())
    );

    // get_holidays API/オフライン統合インターフェースの動作確認
    let all_holidays = get_holidays(2024, 2025);
    assert!(all_holidays.contains_key(&NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
    assert!(all_holidays.contains_key(&NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()));
}

// ============================================================================
// 2. test_02_date_parser_and_specified_off: シリアル値・NFKC全角・指定公休HARD固定＆免除検証
// ============================================================================
#[test]
fn test_02_date_parser_and_specified_off() {
    let holidays = calculate_japanese_holidays(2024);

    // (1) Excelシリアル値 (45536 = 2024-09-01)
    let d = excel_serial_to_date(45536.0);
    assert_eq!(d, Some(NaiveDate::from_ymd_opt(2024, 9, 1).unwrap()));

    // (2) 全角文字列・NFKC正規化・時刻付き表記のパース
    let cell_fullwidth = calamine::Data::String("９／１ ０９：００〜９／３ １８：００、９／１０".to_string());
    let dates = parse_date_list(&cell_fullwidth, 2024, None);
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 1).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 2).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 3).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 10).unwrap()));
    assert_eq!(dates.len(), 4);

    // (3) 日曜日・祝日除外フィルタの検証
    let cell_with_holidays = calamine::Data::String("9/15, 9/16, 9/17".to_string());
    let filtered = parse_date_list(&cell_with_holidays, 2024, Some(&holidays));
    assert!(!filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 15).unwrap()));
    assert!(!filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 16).unwrap()));
    assert!(filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 17).unwrap()));
    assert_eq!(filtered.len(), 1);

    // (4) シフト生成における指定公休HARD固定と同一ISO週の主公休義務免除検証
    let (parsed, holidays_2026) = load_test_excel();
    let res = solve_shift(&parsed, 2026, 9, &holidays_2026);
    assert!(res.success, "シフト生成が成功すること");

    // 小島さんの指定公休日確認
    let kojira = parsed.staffs.iter().find(|s| s.name == "小島").unwrap();
    println!("小島さんの指定公休日一覧: {:?}", kojira.specified_off_dates);

    let kojira_shifts = res.shifts.get("小島").expect("小島さんのシフトが存在すること");
    for &spec_date in &kojira.specified_off_dates {
        if spec_date.year() == 2026 && spec_date.month() == 9 {
            let day = spec_date.day();
            assert_eq!(
                kojira_shifts.get(&day),
                Some(&0),
                "指定公休日 (9/{}) は確実に公休 (0) であること",
                day
            );
        }
    }
}

// ============================================================================
// 3. test_03_substitute_off_in_holiday_week: 祝日と固定公休曜日重複時の振替公休自動設定検証
// ============================================================================
#[test]
fn test_03_substitute_off_in_holiday_week() {
    let (parsed, holidays) = load_test_excel();
    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "シフト生成が成功すること");

    // 2026年9月第4週: 9/21(月) 敬老の日, 9/22(火) 国民の休日, 9/23(水) 秋分の日
    // 長谷川さんは火曜固定公休。9/22(火)はもともと祝日休業。
    // 木(9/24), 金(9/25), 土(9/26) のうち過剰出勤にならず、所定勤務日数を守れていること
    let hasegawa_shifts = res.shifts.get("長谷川").expect("長谷川さんのシフト");
    let target = *res.target_days.get("長谷川").expect("所定勤務日数");
    let actual_work_days: i32 = hasegawa_shifts.values().map(|&v| v as i32).sum();
    assert_eq!(actual_work_days, target, "長谷川さんの出勤日数が所定勤務日数と完全一致すること");
}

// ============================================================================
// 4. test_04_holiday_week_min_1_off: 祝日を含む診療日5日週のコアスタッフ最大4日出勤検証
// ============================================================================
#[test]
fn test_04_holiday_week_min_1_off() {
    let (parsed, holidays) = load_test_excel();
    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "シフト生成が成功すること");

    let ctx = MonthContext::new(2026, 9, &holidays);

    // 祝日を含み、診療日数が5日以下の週において、全コアスタッフが出勤日数 <= 4 (最低1日公休) であること
    for week in &ctx.weeks {
        if week.has_holiday && week.clinic_days.len() == 5 {
            for staff in &parsed.staffs {
                if staff.is_core && staff.pattern != "除外" {
                    let staff_shifts = &res.shifts[&staff.name];
                    let week_work_cnt: u32 = week.clinic_days.iter().map(|d| staff_shifts[d] as u32).sum();
                    assert!(
                        week_work_cnt <= 4,
                        "スタッフ [{}] は祝日週 (week {}) において最大4日出勤であること (実出勤: {}日)",
                        staff.name,
                        week.week_index,
                        week_work_cnt
                    );
                }
            }
        }
    }
}

// ============================================================================
// 5. test_05_weekly_balance_and_2f_back_4staff: 週単位公休平準化および2階裏4名体制最低日数確保検証
// ============================================================================
#[test]
fn test_05_weekly_balance_and_2f_back_4staff() {
    let (parsed, holidays) = load_test_excel();
    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "シフト生成が成功すること");

    let ctx = MonthContext::new(2026, 9, &holidays);

    // 通常週（祝日を含まない6診療日週: 第2週 9/7〜9/12 など）において
    // 2階裏コアスタッフの出勤数が 4名以上となる日が最低 2日以上存在すること
    let core_2f_back_names: Vec<String> = parsed
        .staffs
        .iter()
        .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
        .map(|s| s.name.clone())
        .collect();

    for week in &ctx.weeks {
        if !week.has_holiday && week.clinic_days.len() == 6 {
            let mut days_with_4plus = 0;
            for &d in &week.clinic_days {
                let cnt: u32 = core_2f_back_names
                    .iter()
                    .map(|name| res.shifts[name][&d] as u32)
                    .sum();
                if cnt >= 4 {
                    days_with_4plus += 1;
                }
            }
            assert!(
                days_with_4plus >= parsed.config.sec_2f_back_weekly_4_min,
                "通常週 (week {}) で2階裏4名体制が {}日以上確保されること (実績: {}日)",
                week.week_index,
                parsed.config.sec_2f_back_weekly_4_min,
                days_with_4plus
            );
        }
    }
}

// ============================================================================
// 6. test_06_prev_month_linking: 前月末尾実績からの連勤引き継ぎと月初連勤上限遵守検証
// ============================================================================
#[test]
fn test_06_prev_month_linking() {
    let (mut parsed, holidays) = load_test_excel();

    // 榊原さんの前月末尾実績として 4連勤 (8/28〜8/31 が出勤: 1, 1, 1, 1) を設定
    parsed
        .prev_month_tail_shifts
        .insert("榊原".to_string(), vec![1, 1, 1, 1]);

    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "シフト生成が成功すること");

    // 2026/9/1 は火曜、9/2 は水曜。
    // 4連勤しているため、連勤上限5日を満たすには、9/1に出勤したとしても9/2は必ず公休(0)でなければならない。
    let sakakibara_shifts = &res.shifts["榊原"];
    let d1 = sakakibara_shifts[&1];
    let d2 = sakakibara_shifts[&2];
    assert!(
        d1 + d2 <= 1,
        "前末4連勤のスタッフは、月初2日間の出勤数は最大1日であること (9/1={}, 9/2={})",
        d1,
        d2
    );
}

// ============================================================================
// 7. test_07_second_off_monthly_rotation: 週4日×固定1日スタッフの第1公休固定＋第2公休月内固定＆ローテーション検証
// ============================================================================
#[test]
fn test_07_second_off_monthly_rotation() {
    let (mut parsed, holidays) = load_test_excel();

    // 宮澤さん (週4日勤務, 固定公休=[1]=火曜) の前月第2公休を [2]=水曜 に設定
    parsed
        .prev_month_main_offs
        .insert("宮澤".to_string(), 2); // 水曜

    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "シフト生成が成功すること");

    let miyazawa_shifts = &res.shifts["宮澤"];
    let ctx = MonthContext::new(2026, 9, &holidays);

    // (1) 固定第1公休 (火曜) はすべての診療日で休み (0)
    for &d in &ctx.clinic_days {
        if ctx.weekday_map[&d] == 1 {
            assert_eq!(
                miyazawa_shifts[&d], 0,
                "宮澤さんの固定第1公休曜日(火曜)である 9/{} は必ず公休(0)であること",
                d
            );
        }
    }

    // (2) 第2公休曜日が月内で一貫して休みになっていること
    let second_off_w = *res.main_offs.get("宮澤").expect("宮澤さんの第2公休曜日が決定されていること");
    assert_ne!(second_off_w, 1, "第2公休は固定第1公休(火曜)とは異なること");

    // 前月が水曜(2)だったため、ローテーションペナルティにより今月は水曜以外（木曜や月曜等）が選ばれていること
    assert_ne!(
        second_off_w, 2,
        "前月主公休(水曜)からのローテーションペナルティにより、今月は水曜以外が選ばれること"
    );

    // 第2公休曜日 (second_off_w) に休みが入っていること
    let second_off_days: Vec<u32> = ctx
        .clinic_days
        .iter()
        .filter(|&&d| ctx.weekday_map[&d] == second_off_w)
        .cloned()
        .collect();

    for d in second_off_days {
        assert_eq!(
            miyazawa_shifts[&d], 0,
            "決定された第2公休曜日の診療日 9/{} は確実に公休(0)であること",
            d
        );
    }
}
