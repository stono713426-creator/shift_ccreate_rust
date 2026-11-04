//! 勤務シフト自動生成システム 統合回帰テストスイート (`tests/regression_suite.rs`)
//!
//! Version: v1.2.1
//! Updated: 2026-11-05 - v1.2.1 標準開発条件表として「勤務条件表_ひな形.xlsx」を採用
//! - v1.2.0 (2026-11-01): test_12 石橋特別枠・2階裏基本人数・3ヶ月ローテーション・OPEタイポ吸収・年跨ぎ年補完・前月実績年月ガード検証
//! - v1.1.0 (2026-10-31): test_11 制約条件パース・除外スタッフ補正・主公休スライド・金曜HIGH1の3名体制検証
//! - v1.0.1 (2026-10-31): test_10 小島さん11月特別有休パース・反映・Excel出力検証追加
//! - test_01_holidays: 祝日計算・春分秋分・国民の休日・振替休日検証
//! - test_02_date_parser_and_specified_off: 日付パーサー・NFKC・指定公休HARD固定＆ISO週義務免除検証
//! - test_03_substitute_off_in_holiday_week: 祝日と固定公休曜日重複時の振替公休自動設定検証
//! - test_04_holiday_week_min_1_off: 祝日を含む診療日5日週のコアスタッフ最大4日出勤（最低1日公休）検証
//! - test_05_weekly_balance_and_2f_back_4staff: 週単位公休平準化および2階裏4名体制最低日数確保検証
//! - test_06_prev_month_linking: 前月末尾実績からの末尾7日連勤引き継ぎと月初連勤上限遵守検証
//! - test_07_second_off_monthly_rotation: 週4日×固定1日スタッフの第1公休固定＋第2公休月内固定＆ローテーション検証
//! - test_08_renderer_excel_output: 現場フォーマット Excel 出力と calamine 再読込・書式検証 (Phase 3)
//! - test_09_multi_month_continuous_generation: ShiftGenerator による複数月連続生成＆月跨ぎ引き継ぎ検証 (Phase 3)

use calamine::{open_workbook_auto, Reader};
use chrono::{Datelike, NaiveDate};
use shift_core::generator::{GenerateOptions, ShiftGenerator};
use shift_core::holidays::{calculate_japanese_holidays, get_holidays};
use shift_core::parser::{
    excel_serial_to_date, parse_date_list, parse_date_list_for_months, parse_shift_excel,
    ParsedExcelData,
};
use shift_core::renderer::render_shifts_to_file;
use shift_core::solver::{parse_rule_target_dates, solve_shift, MonthContext};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// テスト用標準条件表Excel (`勤務条件表_ひな形.xlsx`) のパースヘルパー
fn load_test_excel() -> (ParsedExcelData, HashMap<NaiveDate, String>) {
    let excel_path = if Path::new("勤務条件表_ひな形.xlsx").exists() {
        "勤務条件表_ひな形.xlsx"
    } else if Path::new("勤務条件表_9.xlsx").exists() {
        "勤務条件表_9.xlsx"
    } else if Path::new("勤務条件表_9月.xlsx").exists() {
        "勤務条件表_9月.xlsx"
    } else {
        panic!("標準条件表Excel (勤務条件表_ひな形.xlsx) が見つかりません。");
    };

    let holidays = get_holidays(2026, 2027);
    let parsed = parse_shift_excel(excel_path, &holidays)
        .expect("標準条件表Excelのパースに失敗しました");
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

    // (2-B) ハイフンによる範囲指定 (スペースなし: "2026/11/17-2026/11/21,2026/12/29-2027/01/04")
    let cell_hyphen_range = calamine::Data::String("2026/11/17-2026/11/21,2026/12/29-2027/01/04".to_string());
    let dates_hyphen = parse_date_list(&cell_hyphen_range, 2026, None);
    println!("dates_hyphen parsed: {:?}", dates_hyphen);
    assert!(dates_hyphen.contains(&NaiveDate::from_ymd_opt(2026, 11, 17).unwrap()), "11/17が含まれること");
    assert!(dates_hyphen.contains(&NaiveDate::from_ymd_opt(2026, 11, 21).unwrap()), "11/21が含まれること");
    assert_eq!(dates_hyphen.len(), 5 + 7);

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

// ============================================================================
// 8. test_08_renderer_excel_output: 現場フォーマット Excel 出力と calamine 再読込・書式検証 (Phase 3)
// ============================================================================
#[test]
fn test_08_renderer_excel_output() {
    let (parsed, holidays) = load_test_excel();
    let res = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res.success, "2026年9月のシフト生成が成功すること");

    let out_path = PathBuf::from("target/test_output_2026_09.xlsx");
    let _ = fs::remove_file(&out_path);

    let results = vec![(2026, 9, res)];
    render_shifts_to_file(&out_path, &parsed, &results, &holidays)
        .expect("現場フォーマット Excel の出力に成功すること");

    assert!(out_path.exists(), "出力ファイルが存在すること");

    // calamine で生成された Excel ファイルを再読込して検証
    let mut workbook = open_workbook_auto(&out_path).expect("出力Excelをcalamineで開けること");
    let sheet_names = workbook.sheet_names();
    assert!(
        sheet_names.contains(&"2026年09月シフト".to_string()),
        "シート名 '2026年09月シフト' が存在すること: {:?}",
        sheet_names
    );

    let range = workbook
        .worksheet_range("2026年09月シフト")
        .expect("ワークシート '2026年09月シフト' が読み込めること");

    let rows: Vec<Vec<calamine::Data>> = range.rows().map(|r| r.to_vec()).collect();
    assert!(rows.len() >= 36, "タイトル+ヘッダー+30日+集計4行で最低36行存在すること (実: {}行)", rows.len());

    // 行1: タイトル
    let title_cell = rows[0][0].to_string();
    assert!(title_cell.contains("2026年9月"), "タイトル行に '2026年9月' が含まれること: {}", title_cell);

    // 行2: ヘッダー
    let h_date = rows[1][0].to_string();
    let h_wday = rows[1][1].to_string();
    let h_desc = rows[1][2].to_string();
    let h_warn = rows[1][3].to_string();
    assert_eq!(h_date, "日付");
    assert_eq!(h_wday, "曜日");
    assert_eq!(h_desc, "備考");
    assert_eq!(h_warn, "警告");

    // 日曜・祝日の休業確認: 2026/9/6 は日曜、9/21 は敬老の日
    // 日付1日は row[2], 6日は row[7], 21日は row[22]
    let row_sep6 = &rows[7];
    assert_eq!(row_sep6[0].to_string(), "6");
    assert_eq!(row_sep6[1].to_string(), "日");
    // コアスタッフのセルが "休業" であること
    assert_eq!(row_sep6[4].to_string(), "休業");

    let row_sep21 = &rows[22];
    assert_eq!(row_sep21[0].to_string(), "21");
    assert_eq!(row_sep21[1].to_string(), "月");
    assert_eq!(row_sep21[2].to_string(), "敬老の日");
    assert_eq!(row_sep21[4].to_string(), "休業");

    // 下部集計行のラベル確認
    // 30日月の最終日行は row[31], 集計行は row[32]〜row[35]
    let r_work = &rows[32];
    let r_target = &rows[33];
    let r_off = &rows[34];
    let r_main_off = &rows[35];

    assert_eq!(r_work[2].to_string(), "合計出勤日数");
    assert_eq!(r_target[2].to_string(), "所定勤務日数");
    assert_eq!(r_off[2].to_string(), "公休日数");
    assert_eq!(r_main_off[2].to_string(), "主公休曜日");

    // 所定勤務日数行のスタッフセルに数値が入っていること
    let target_val_str = r_target[4].to_string();
    let target_val: i32 = target_val_str.parse().unwrap_or(-1);
    assert!(target_val > 0, "所定勤務日数セルが正の整数値であること: {}", target_val_str);

    // テスト後の一時ファイル削除
    let _ = fs::remove_file(&out_path);
}

// ============================================================================
// 9. test_09_multi_month_continuous_generation: ShiftGenerator 連続生成＆月跨ぎ引き継ぎ検証 (Phase 3)
// ============================================================================
#[test]
fn test_09_multi_month_continuous_generation() {
    let (parsed, holidays) = load_test_excel();

    // 2ヶ月連続生成テスト (2026年9月、2026年10月)
    let test_months = vec![(2026, 9), (2026, 10)];
    let out_path = PathBuf::from("target/test_output_multi_month.xlsx");
    let _ = fs::remove_file(&out_path);

    let options = GenerateOptions {
        target_months: Some(test_months),
        output_path: Some(out_path.clone()),
    };

    let mut log_count = 0;
    let res = ShiftGenerator::generate(&parsed, &holidays, &options, |_idx, _total, _y, _m, msg| {
        println!("[ShiftGenerator Progress] {}", msg);
        log_count += 1;
    });

    assert!(res.is_ok(), "2ヶ月連続シフト生成が成功すること: {:?}", res.err());
    let (saved_path, month_results) = res.unwrap();
    assert_eq!(saved_path, out_path);
    assert_eq!(month_results.len(), 2, "2ヶ月分の結果が返されること");

    assert!(month_results[0].2.success, "2026年9月のソルブが成功すること");
    assert!(month_results[1].2.success, "2026年10月のソルブが成功すること");
    assert!(log_count > 0, "進捗コールバックが複数回呼び出されていること");

    // 生成された Excel に両方のシートが含まれていること
    let workbook = open_workbook_auto(&out_path).expect("出力Excelが開けること");
    let sheets = workbook.sheet_names();
    assert!(sheets.contains(&"2026年09月シフト".to_string()), "9月シートが存在すること");
    assert!(sheets.contains(&"2026年10月シフト".to_string()), "10月シートが存在すること");

    // テスト後の一時ファイル削除
    let _ = fs::remove_file(&out_path);
}

// ============================================================================
// 10. test_10_kojima_november_special_vacation: 小島さんの11/17-11/21特別有休反映検証
// ============================================================================
#[test]
fn test_10_kojima_november_special_vacation() {
    let (parsed, holidays) = load_test_excel();

    // 1. パース結果における小島さんの特別有休と指定公休の保持検証
    let kojima = parsed
        .staffs
        .iter()
        .find(|s| s.name == "小島")
        .expect("小島さんがスタッフマスタに存在すること");

    println!("小島さんの特別有休一覧: {:?}", kojima.special_vacation_dates);
    for d in 17..=21 {
        let date = NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert!(
            kojima.special_vacation_dates.contains(&date),
            "11/{} が小島さんの特別有休に含まれていること",
            d
        );
    }
    let spec_date = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
    assert!(
        kojima.specified_off_dates.contains(&spec_date),
        "11/16 が小島さんの指定公休に含まれていること"
    );

    // 2. 2026年11月シフト生成
    let res = solve_shift(&parsed, 2026, 11, &holidays);
    assert!(res.success, "2026年11月のシフト生成が成功すること");

    let kojima_shifts = res.shifts.get("小島").expect("小島さんのシフトが存在すること");
    for d in 17..=21 {
        assert_eq!(
            kojima_shifts.get(&d),
            Some(&0),
            "特別有休期間中の 11/{} は確実に休み (0) であること",
            d
        );
    }
    assert_eq!(
        kojima_shifts.get(&16),
        Some(&0),
        "指定公休日の 11/16 は確実に休み (0) であること"
    );

    // 3. Excel レンダリングで「特有」表記が正しく出力されることの検証
    let out_path = PathBuf::from("target/test_kojima_nov_shift.xlsx");
    let _ = fs::remove_file(&out_path);

    let render_res = render_shifts_to_file(&out_path, &parsed, &[(2026, 11, res)], &holidays);
    assert!(render_res.is_ok(), "Excel出力が成功すること: {:?}", render_res.err());

    let mut workbook = open_workbook_auto(&out_path).expect("出力Excelが開けること");
    let range = workbook
        .worksheet_range("2026年11月シフト")
        .expect("11月シフトシートが存在すること");
    let rows: Vec<Vec<calamine::Data>> = range.rows().map(|r| r.to_vec()).collect();

    // 2行目のスタッフ名から小島さんの列インデックスを特定 (行0はタイトル行、行1がヘッダー行)
    let staff_row = &rows[1];
    let mut kojima_col = None;
    for (col_idx, cell) in staff_row.iter().enumerate() {
        if cell.to_string().trim() == "小島" {
            kojima_col = Some(col_idx);
            break;
        }
    }
    let k_col = kojima_col.expect("ヘッダー行に小島さんが見つかること");

    // 日付1日は row[2], 16日は row[17], 17日は row[18], 21日は row[22]
    // 11/17〜11/21 が "特有" と出力されていること
    for d in 17..=21 {
        let row_idx = d + 1; // 1日はrow[2] -> d日は row[d+1]
        let cell_val = rows[row_idx as usize][k_col].to_string();
        assert_eq!(
            cell_val, "特有",
            "11/{} の小島さんのセル表記は '特有' であること (実際: {})",
            d, cell_val
        );
    }

    // 11/16 の指定公休は "公" と出力されていること
    let cell_nov16 = rows[17][k_col].to_string();
    assert_eq!(cell_nov16, "公", "11/16 の小島さんのセル表記は '公' であること");

    let _ = fs::remove_file(&out_path);
}

// ============================================================================
// 11. test_11_friday_high1_and_rule_priority_v1_1_0:
//     シフト制約条件パース・除外スタッフ補正・主公休スライド・金曜HIGH1の3名体制検証
// ============================================================================
#[test]
fn test_11_friday_high1_and_rule_priority_v1_1_0() {
    let (parsed, holidays) = load_test_excel();

    // 1. シフト制約条件シートの適用条件 (condition_desc) が空文字にならず正確にパースされていること
    assert!(
        !parsed.rules.is_empty(),
        "シフト制約条件が1件以上パースされていること"
    );
    for rule in &parsed.rules {
        assert!(
            !rule.condition_desc.is_empty(),
            "ルール '{}' の適用条件 (condition_desc) が空でないこと",
            rule.name
        );
    }

    let has_friday_high1 = parsed.rules.iter().any(|r| {
        r.condition_desc.contains("金曜")
            && r.level.to_uppercase() == "HIGH1"
            && r.val_min == 2
            && r.val_rec == 3
    });
    assert!(
        has_friday_high1,
        "金曜の HIGH1 (最小2・推奨3) ルールが正確にパースされていること"
    );

    // 2. parse_rule_target_dates: 「Vit」手術日のUTF-8安全性・タイポ吸収("Viit")および 火曜(OPE日) の絞り込み検証
    let ctx_jan = MonthContext::new(2027, 1, &holidays);
    let vit_days = parse_rule_target_dates("「Vit」手術日", &ctx_jan, &parsed.ope_dict);
    assert!(
        vit_days.contains(&6),
        "標準条件表 2027-01-06 ('Vit,cat') が「Vit」手術日として抽出されること"
    );
    for &d in &vit_days {
        let date = ctx_jan.date_map[&d];
        let ope_str = parsed.ope_dict.get(&date).expect("OPE日であること");
        let ope_lower = ope_str.to_lowercase();
        assert!(
            ope_lower.contains("vit") || ope_lower.contains("viit") || ope_str.contains("硝子体"),
            "「Vit」手術日に該当する日のみ抽出されること: {} -> {}",
            date,
            ope_str
        );
    }

    let tue_ope_days = parse_rule_target_dates("火曜(OPE日)", &ctx_jan, &parsed.ope_dict);
    for &d in &tue_ope_days {
        assert_eq!(ctx_jan.weekday_map[&d], 1, "火曜日のみであること");
        assert!(
            parsed.ope_dict.contains_key(&ctx_jan.date_map[&d]),
            "1/{} は手術のある火曜日であること",
            d
        );
    }

    // 3. 高橋さん除外 (除外フラグ=True 相当) かつ 12月実績引き継ぎ状態での 2027年1〜3月連続生成検証
    let mut jan_mar_parsed = parsed.clone();
    // まず 2026年12月を解いて前月実績を作成
    let res_dec = solve_shift(&parsed, 2026, 12, &holidays);
    assert!(res_dec.success, "2026年12月のシフト生成が成功すること");
    jan_mar_parsed.prev_month_main_offs = res_dec.main_offs.clone();
    let ctx_dec = MonthContext::new(2026, 12, &holidays);
    let mut dec_tail = HashMap::new();
    for s in &jan_mar_parsed.staffs {
        if let Some(sm) = res_dec.shifts.get(&s.name) {
            let tail: Vec<u8> = (25..=ctx_dec.days_in_month)
                .map(|d| sm.get(&d).copied().unwrap_or(0))
                .collect();
            dec_tail.insert(s.name.clone(), tail);
        }
    }
    jan_mar_parsed.prev_month_tail_shifts = dec_tail;

    // 高橋さんを除外 (宮澤,高橋 のグループ必須ルールで Stage 1 Infeasible にならないことを検証)
    jan_mar_parsed.staffs.retain(|s| s.name != "高橋");

    let out_path = PathBuf::from("target/test_v1_1_0_jan_mar.xlsx");
    let _ = fs::remove_file(&out_path);
    let options = GenerateOptions {
        target_months: Some(vec![(2027, 1), (2027, 2), (2027, 3)]),
        output_path: Some(out_path.clone()),
    };

    let (_, results) =
        ShiftGenerator::generate(&jan_mar_parsed, &holidays, &options, |_, _, _, _, _| {})
            .expect("2027年1〜3月の連続生成が成功すること");

    for (y, m, res) in &results {
        assert!(res.success, "{}年{}月のソルブが成功すること", y, m);
        assert_eq!(
            res.stage, 1,
            "{}年{}月が Stage 2 フォールバックではなく Stage 1 (厳格探索) で解けること",
            y, m
        );
    }

    // 4. 2027年1月の金曜 (1/8, 1/15, 1/22, 1/29) において 小田切・鈴木(孝)・榊原 の3名全員が出勤していること
    let res_jan = &results[0].2;
    let fri_group = ["小田切", "鈴木(孝)", "榊原"];
    for &d in &[8u32, 15, 22, 29] {
        let count: u8 = fri_group.iter().map(|&name| res_jan.shifts[name][&d]).sum();
        assert_eq!(
            count, 3,
            "2027年1月{}日(金曜)において 小田切・鈴木(孝)・榊原 の3名体制(HIGH1推奨人数3)が達成されていること (実: {}名)",
            d, count
        );
    }

    // 5. 月曜 HARD ルール (田中島・梶原 最小2名) が Stage 1 で厳守されていること (有休等を除く診療月曜)
    let mon_group = ["田中島", "梶原"];
    for &d in &ctx_jan.clinic_days {
        if ctx_jan.weekday_map[&d] == 0 {
            let date = ctx_jan.date_map[&d];
            let has_vac = jan_mar_parsed.staffs.iter().any(|s| {
                mon_group.contains(&s.name.as_str())
                    && (s.vacation_dates.contains(&date)
                        || s.special_vacation_dates.contains(&date)
                        || s.preferred_off_dates.contains(&date)
                        || s.specified_off_dates.contains(&date))
            });
            if !has_vac {
                let count: u8 = mon_group.iter().map(|&name| res_jan.shifts[name][&d]).sum();
                assert_eq!(
                    count, 2,
                    "2027年1月{}日(月曜)において 田中島・梶原 の2名出勤(HARD)が厳守されていること",
                    d
                );
            }
        }
    }

    let _ = fs::remove_file(&out_path);
}

// ============================================================================
// 12. test_12_full_spec_audit_v1_2_0:
//     v1.2.0 全仕様監査項目の統合検証
// ============================================================================
#[test]
fn test_12_full_spec_audit_v1_2_0() {
    let (parsed, holidays) = load_test_excel();

    // 1. 年省略 M/D 表記の年跨ぎ補完検証 (2026/09〜2027/03 の対象月で "1/15, 2/10" -> 2027年)
    let span_months = vec![
        (2026, 9),
        (2026, 10),
        (2026, 11),
        (2026, 12),
        (2027, 1),
        (2027, 2),
        (2027, 3),
    ];
    let cell_md = calamine::Data::String("11/18, 1/15, 2/10".to_string());
    let resolved_dates = parse_date_list_for_months(&cell_md, 2026, &span_months, None);
    assert!(
        resolved_dates.contains(&NaiveDate::from_ymd_opt(2026, 11, 18).unwrap()),
        "11/18 が 2026-11-18 として解決されること"
    );
    assert!(
        resolved_dates.contains(&NaiveDate::from_ymd_opt(2027, 1, 15).unwrap()),
        "1/15 が年跨ぎ対象月に合わせて 2027-01-15 として解決されること"
    );
    assert!(
        resolved_dates.contains(&NaiveDate::from_ymd_opt(2027, 2, 10).unwrap()),
        "2/10 が年跨ぎ対象月に合わせて 2027-02-10 として解決されること"
    );

    // 2. 手術スケジュール表記ゆれ ("Viit,cat" / "cay") の吸収検証
    let ctx_jan = MonthContext::new(2027, 1, &holidays);
    let mut typo_ope = HashMap::new();
    typo_ope.insert(
        NaiveDate::from_ymd_opt(2027, 1, 6).unwrap(),
        "Viit,cat".to_string(),
    );
    typo_ope.insert(
        NaiveDate::from_ymd_opt(2027, 1, 12).unwrap(),
        "cay".to_string(),
    );
    let vit_matched = parse_rule_target_dates("「Vit」手術日", &ctx_jan, &typo_ope);
    assert_eq!(
        vit_matched,
        vec![6],
        "タイポ 'Viit,cat' が「Vit」手術日として漏れなく検出されること"
    );
    let cat_matched = parse_rule_target_dates("「cat」手術日", &ctx_jan, &typo_ope);
    assert_eq!(
        cat_matched,
        vec![6, 12],
        "タイポ 'cay' が「cat」手術日として漏れなく検出されること"
    );

    // 3. 石橋さん（週4日（必要に応じて週5日可）＋土曜固定公休）の特別枠動作確認
    let ishibashi = parsed
        .staffs
        .iter()
        .find(|s| s.name == "石橋")
        .expect("石橋さんが存在すること");
    assert!(
        ishibashi.is_special,
        "石橋さんが特別枠 (is_special=true) として認識されていること"
    );

    let mut special_test_parsed = parsed.clone();
    // 2階裏の他スタッフに有休を追加し、石橋さんが target を超えて応援出勤できるか検証
    if let Some(st) = special_test_parsed
        .staffs
        .iter_mut()
        .find(|s| s.name == "大久保")
    {
        for d in [2u32, 9, 16, 30] {
            st.vacation_dates
                .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        }
    }
    let res_sp = solve_shift(&special_test_parsed, 2026, 9, &holidays);
    assert!(res_sp.success && res_sp.stage == 1, "Stage 1 で解けること");
    let ishi_target = res_sp.target_days["石橋"];
    let ishi_actual: i32 = res_sp.shifts["石橋"].values().map(|&v| v as i32).sum();
    assert!(
        ishi_actual >= ishi_target && ishi_actual <= ishi_target + 5,
        "石橋さんの出勤日数 ({}) が [target={}, target+5={}] の範囲内であること",
        ishi_actual,
        ishi_target,
        ishi_target + 5
    );
    let ctx_sep = MonthContext::new(2026, 9, &holidays);
    for &d in &ctx_sep.clinic_days {
        if ctx_sep.weekday_map[&d] == 5 {
            assert_eq!(
                res_sp.shifts["石橋"][&d], 0,
                "石橋さんの固定公休（土曜: 9/{}）は引き続き厳守されること",
                d
            );
        }
    }

    // 4. 2階裏_基本人数 (sec_2f_back_base_cap = 3) の日次最低人数確保検証
    let res_sep = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res_sep.success && res_sep.stage == 1);
    let core_2f_b: Vec<&shift_core::parser::Staff> = parsed
        .staffs
        .iter()
        .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
        .collect();
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        let vac_cnt = core_2f_b
            .iter()
            .filter(|s| {
                s.vacation_dates.contains(&dt)
                    || s.special_vacation_dates.contains(&dt)
                    || s.preferred_off_dates.contains(&dt)
                    || s.specified_off_dates.contains(&dt)
            })
            .count() as i32;
        let expected_min = (parsed.config.sec_2f_back_base_cap - vac_cnt).max(0) as u32;
        let actual_2f_b: u32 = core_2f_b
            .iter()
            .map(|s| res_sep.shifts[&s.name][&d] as u32)
            .sum();
        assert!(
            actual_2f_b >= expected_min,
            "9/{} の2階裏出勤人数 ({}) が基本人数下限 ({}) 以上であること",
            d,
            actual_2f_b,
            expected_min
        );
    }

    // 5. 過去最大3ヶ月分の主公休ローテーション検証 (2ヶ月周期ピンポンの防止)
    let out_rot = PathBuf::from("target/test_v1_2_0_rot4m.xlsx");
    let _ = fs::remove_file(&out_rot);
    let rot_opts = GenerateOptions {
        target_months: Some(vec![(2026, 9), (2026, 10), (2026, 11), (2026, 12)]),
        output_path: Some(out_rot.clone()),
    };
    let (_, rot_results) = ShiftGenerator::generate(&parsed, &holidays, &rot_opts, |_, _, _, _, _| {})
        .expect("4ヶ月連続生成が成功すること");
    let miyazawa_offs: Vec<u32> = rot_results
        .iter()
        .map(|(_, _, r)| r.main_offs["宮澤"])
        .collect();
    let unique_offs: HashSet<u32> = miyazawa_offs.iter().copied().collect();
    assert!(
        unique_offs.len() >= 3,
        "3ヶ月履歴ローテーションにより宮澤さんの第2公休が4ヶ月で3種類以上に分散すること: {:?}",
        miyazawa_offs
    );
    let _ = fs::remove_file(&out_rot);

    // 6. 前月シフト実績シートの年月整合ガード検証
    let mut guard_parsed = parsed.clone();
    guard_parsed.prev_sheet_year_month = Some((2026, 12)); // 2026年12月の実績シートと仮定
    guard_parsed
        .prev_month_tail_shifts
        .insert("榊原".to_string(), vec![1, 1, 1, 1, 1]); // 5連勤 (本来なら翌月1日目は強制公休)

    // 2026年9月を単発生成した場合、2026年12月の実績は誤適用されない（クリアされる）こと
    let out_guard = PathBuf::from("target/test_v1_2_0_guard.xlsx");
    let _ = fs::remove_file(&out_guard);
    let guard_opts_sep = GenerateOptions {
        target_months: Some(vec![(2026, 9)]),
        output_path: Some(out_guard.clone()),
    };
    let (_, sep_guard_res) =
        ShiftGenerator::generate(&guard_parsed, &holidays, &guard_opts_sep, |_, _, _, _, _| {})
            .expect("9月生成成功");
    assert!(sep_guard_res[0].2.success);

    // 一方、2027年1月（2026年12月の翌月）を生成した場合は12月末尾実績が正しく適用されること
    let guard_opts_jan = GenerateOptions {
        target_months: Some(vec![(2027, 1)]),
        output_path: Some(out_guard.clone()),
    };
    let (_, jan_guard_res) =
        ShiftGenerator::generate(&guard_parsed, &holidays, &guard_opts_jan, |_, _, _, _, _| {})
            .expect("1月生成成功");
    assert!(jan_guard_res[0].2.success);
    let _ = fs::remove_file(&out_guard);

    // 7. 標準条件表 (勤務条件表_ひな形.xlsx) の構造整合検証
    assert_eq!(
        parsed.staffs.len(),
        25,
        "標準ひな形の有効スタッフ数が25名（並木さん含む、除外フラグの高橋・森川さん除く）であること"
    );
    assert!(
        parsed
            .staffs
            .iter()
            .any(|s| s.name == "並木" && s.employment == "パート社員" && s.is_core),
        "標準ひな形にパート社員（コアスタッフ）の並木さんが含まれること"
    );
    assert!(
        !parsed.staffs.iter().any(|s| s.name == "高橋" || s.name == "森川"),
        "除外フラグ=True のスタッフ（高橋・森川）が除外されていること"
    );
    assert_eq!(parsed.rules.len(), 12, "標準ひな形のシフト制約条件が12件であること");
}






