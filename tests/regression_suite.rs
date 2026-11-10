//! 勤務シフト自動生成システム 統合回帰テストスイート (`tests/regression_suite.rs`)
//!
//! Version: v1.3.2
//! Updated: 2026-11-10 - v1.3.2 test_15 Excel単体日付セル（2/15→2026-02-15シリアル値）の翌年補完＆2027年2月ソルブ反映検証追加
//! - v1.3.1 (2026-11-10): test_14 連勤上限・希望休超過・Ope集中週・HARDペア全休時のソルバー破綻＆重み逆転防止検証追加
//! - v1.3.0 (2026-11-08): test_13 2階裏職種判定・Ope日/注射日・石橋さん週4固定＆条件付水曜公休＆Ope外来除外・曜日別職種人数・土曜PM有休例外・早上がり重複補填・7ヶ月連続Stage1検証追加
//! - v1.2.1 (2026-11-05): 標準開発条件表として「勤務条件表_ひな形.xlsx」を採用
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

    // 3. 堀さん（週4日（必要に応じて週5日可））の特別枠および石橋さん（週4日固定＋土曜基本公休）の動作確認
    let hori = parsed
        .staffs
        .iter()
        .find(|s| s.name == "堀")
        .expect("堀さんが存在すること");
    assert!(
        hori.is_special,
        "堀さんが特別枠 (is_special=true) として認識されていること"
    );

    let res_sp = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res_sp.success && res_sp.stage == 1, "Stage 1 で解けること");
    let hori_target = res_sp.target_days["堀"];
    let hori_actual: i32 = res_sp.shifts["堀"].values().map(|&v| v as i32).sum();
    assert!(
        hori_actual >= hori_target && hori_actual <= hori_target + 5,
        "堀さんの出勤日数 ({}) が [target={}, target+5={}] の範囲内であること",
        hori_actual,
        hori_target,
        hori_target + 5
    );
    let ishi_target = res_sp.target_days["石橋"];
    let ishi_actual: i32 = res_sp.shifts["石橋"].values().map(|&v| v as i32).sum();
    assert_eq!(
        ishi_actual, ishi_target,
        "石橋さんの出勤日数 ({}) が週4固定の所定勤務日数 ({}) と完全一致すること",
        ishi_actual, ishi_target
    );
    let ctx_sep = MonthContext::new(2026, 9, &holidays);
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        if ctx_sep.weekday_map[&d] == 5 && !parsed.ope_dict.contains_key(&dt) {
            assert_eq!(
                res_sp.shifts["石橋"][&d], 0,
                "石橋さんのOpe・注射なし土曜（9/{}）は公休が厳守されること",
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

// ============================================================================
// 13. test_13_v1_3_0_2f_back_and_ishibashi_rules:
//     v1.3.0 2階裏人数・職種バランスおよび石橋さん・長谷川さん勤務ルールの網羅検証
// ============================================================================
#[test]
fn test_13_v1_3_0_2f_back_and_ishibashi_rules() {
    use shift_core::parser::{is_injection_date, is_ope_date, parse_pm_dates_for_months};

    let (parsed, holidays) = load_test_excel();

    // ------------------------------------------------------------------------
    // 1-1. 職種（看護師 / 検査）の判定検証
    // ------------------------------------------------------------------------
    for name in ["堀", "江原", "石橋"] {
        let st = parsed.staffs.iter().find(|s| s.name == name).unwrap();
        assert!(st.is_nurse() && !st.is_exam(), "{} さんは看護師として判定されること", name);
    }
    for name in ["長谷川", "小島"] {
        let st = parsed.staffs.iter().find(|s| s.name == name).unwrap();
        assert!(st.is_exam() && !st.is_nurse(), "{} さんは検査スタッフとして判定されること", name);
    }

    // 「勤務形態」列での看護師(Ns)・検査(スタッフ)表記も認識されること
    let mut custom_parsed = parsed.clone();
    // ------------------------------------------------------------------------
    // 1-2. 手術日（Ope日）と「注射日」の判定検証
    //   2026年9月に「水曜の注射日 (9/9)」「土曜の注射日 (9/19)」「水曜のOpe日 (9/16 下垂)」を設定して検証
    // ------------------------------------------------------------------------
    custom_parsed
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 9).unwrap(), "注射".to_string());
    custom_parsed
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 19).unwrap(), "注射".to_string());
    // 9/2(水) の Ope を外し「Ope・注射なし水曜」の週を作る（代わりに 9/1(火) に Vit,cat、9/3(木) に cat を設定）
    custom_parsed
        .ope_dict
        .remove(&NaiveDate::from_ymd_opt(2026, 9, 2).unwrap());
    custom_parsed
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), "Vit,cat".to_string());
    custom_parsed
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(), "cat".to_string());
    custom_parsed
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(), "Vit,cat".to_string());

    let d_sep1 = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    let d_sep9 = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    let d_sep19 = NaiveDate::from_ymd_opt(2026, 9, 19).unwrap();
    assert!(is_ope_date(&custom_parsed.ope_dict, &d_sep1));
    assert!(!is_injection_date(&custom_parsed.ope_dict, &d_sep1));
    assert!(!is_ope_date(&custom_parsed.ope_dict, &d_sep9), "注射のみの日は Ope日 ではないこと");
    assert!(is_injection_date(&custom_parsed.ope_dict, &d_sep9), "9/9 は注射日であること");
    assert!(is_injection_date(&custom_parsed.ope_dict, &d_sep19), "9/19(土) は注射日であること");

    let res_custom = solve_shift(&custom_parsed, 2026, 9, &holidays);
    assert!(
        res_custom.success && res_custom.stage == 1,
        "注射日・Ope日混在の9月カスタムケースが Stage 1 で解けること: {:?}",
        res_custom.logs
    );

    // ------------------------------------------------------------------------
    // 2-1. 石橋さんの個別勤務ルール検証
    // ------------------------------------------------------------------------
    let ishi_shifts = &res_custom.shifts["石橋"];
    let ctx_sep = MonthContext::new(2026, 9, &holidays);

    // (a) すべての Ope日 および 注射日（9/9水・9/16水・9/19土 含む）で必ず出勤 (1) していること
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        if is_ope_date(&custom_parsed.ope_dict, &dt) || is_injection_date(&custom_parsed.ope_dict, &dt) {
            assert_eq!(
                ishi_shifts[&d], 1,
                "石橋さんは Ope日/注射日 (9/{}) に曜日を問わず必ず出勤していること",
                d
            );
        }
    }

    // (b) 水曜に Ope・注射がない 9/2(水) は公休 (0) であること
    assert_eq!(
        ishi_shifts[&2], 0,
        "Ope・注射がない水曜日 (9/2) は石橋さんが公休 (0) であること"
    );

    // (c) 水曜(9/9 注射) や 土曜(9/19 注射)・水曜(9/16 下垂) に出勤した週も、他の曜日を公休に振り替えて「週4日出勤」を厳守していること
    for week in &ctx_sep.weeks {
        // 9/7〜9/12 (week 1) と 9/14〜9/19 (week 2) は当月完結週
        if week.days.len() == 7 {
            let w_sum: u32 = week.clinic_days.iter().map(|&d| ishi_shifts[&d] as u32).sum();
            if !week.has_holiday {
                assert_eq!(
                    w_sum, 4,
                    "石橋さんは当月完結週 (week {}, days={:?}) において厳密に週4日出勤であること",
                    week.week_index, week.days
                );
            }
        }
    }

    // (d) 祝日がある5診療日週（例: 2026年11月 第2週 11/2〜11/7 [11/3文化の日]、第5週 11/23〜11/28 [11/23勤労感謝の日]）でも週4日出勤を維持すること
    let res_nov = solve_shift(&parsed, 2026, 11, &holidays);
    assert!(res_nov.success && res_nov.stage == 1, "2026年11月が Stage 1 で解けること");
    let ctx_nov = MonthContext::new(2026, 11, &holidays);
    for week in &ctx_nov.weeks {
        if week.has_holiday && week.clinic_days.len() == 5 {
            let w_sum: u32 = week
                .clinic_days
                .iter()
                .map(|&d| res_nov.shifts["石橋"][&d] as u32)
                .sum();
            assert_eq!(
                w_sum, 4,
                "祝日を含む5診療日週 (11月 week {}) でも石橋さんは公休を減らして週4日出勤を維持すること",
                week.week_index
            );
        }
    }

    // ------------------------------------------------------------------------
    // 2-2 & 3-1 & 3-2 & 3-3. 2階裏（外来）の配置・職種バランス・Excel数式除外の検証
    // ------------------------------------------------------------------------
    let core_2f_b: Vec<&shift_core::parser::Staff> = custom_parsed
        .staffs
        .iter()
        .filter(|s| s.section == "2階裏" && s.is_core && s.pattern != "除外")
        .collect();

    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        let w = ctx_sep.weekday_map[&d];
        let is_ope = is_ope_date(&custom_parsed.ope_dict, &dt);

        // 2-2: Ope日の外来メンバーは石橋さんを除外
        let out_members: Vec<&&shift_core::parser::Staff> = core_2f_b
            .iter()
            .filter(|s| !(is_ope && s.name == "石橋"))
            .collect();

        let out_total: u32 = out_members
            .iter()
            .map(|s| res_custom.shifts[&s.name][&d] as u32)
            .sum();
        let out_nurse: u32 = out_members
            .iter()
            .filter(|s| s.is_nurse())
            .map(|s| res_custom.shifts[&s.name][&d] as u32)
            .sum();
        let out_exam: u32 = out_members
            .iter()
            .filter(|s| s.is_exam())
            .map(|s| res_custom.shifts[&s.name][&d] as u32)
            .sum();

        // 3-1: Ope日は石橋さんを除き最低3人以上（2人勤務不可）、看護師1以上、検査は物理上限(火曜以外は2)以上
        if is_ope {
            assert!(
                out_total >= 3,
                "9/{} (Ope日) の2階裏外来人数（石橋除外）が3人以上であること (実: {}人)",
                d,
                out_total
            );
            assert!(out_nurse >= 1, "9/{} (Ope日) の2階裏外来看護師が1人以上であること", d);
            if w != 1 {
                // 火曜は長谷川さんが固定公休のため検査上限1名、火曜以外のOpe日は検査2名以上必須
                assert!(
                    out_exam >= 2,
                    "9/{} (火曜以外のOpe日) の2階裏外来検査スタッフが2人以上であること (実: {}人)",
                    d,
                    out_exam
                );
                // 3-1 ソフト目標: 木曜・金曜等のOpe日で 看護師2＋検査2＝4名 が達成されていること
                assert_eq!(
                    (out_nurse, out_exam),
                    (2, 2),
                    "9/{} (Ope日) で推奨目標の看護師2＋検査2（石橋さん除く）が達成されていること",
                    d
                );
            }
        }

        // 3-2: 曜日別ハード制約
        if w == 1 {
            // 火曜日: 看護師2人以上、検査1人以上
            assert!(
                out_nurse >= 2 && out_exam >= 1,
                "9/{} (火曜) の2階裏外来で 看護師>=2, 検査>=1 が厳守されること (Ns={}, Exam={})",
                d,
                out_nurse,
                out_exam
            );
        } else if w == 3 {
            // 木曜日: 看護師1人以上、検査2人以上
            assert!(
                out_nurse >= 1 && out_exam >= 2,
                "9/{} (木曜) の2階裏外来で 看護師>=1, 検査>=2 が厳守されること (Ns={}, Exam={})",
                d,
                out_nurse,
                out_exam
            );
        } else if w == 5 {
            // 土曜日: 最低3人以上（PM有休なし）
            assert!(
                out_total >= 3,
                "9/{} (土曜) の2階裏外来で最低3人以上が厳守されること (実: {}人)",
                d,
                out_total
            );
        }

        // 3-3(2): 非Ope日に長谷川さんと石橋さんが同時出勤している場合、フルタイムが2人以上（外来計4人以上）配置されていること
        if !is_ope && res_custom.shifts["長谷川"][&d] == 1 && res_custom.shifts["石橋"][&d] == 1 {
            let ft_cnt: u32 = out_members
                .iter()
                .filter(|s| s.name != "長谷川" && s.name != "石橋")
                .map(|s| res_custom.shifts[&s.name][&d] as u32)
                .sum();
            assert!(
                ft_cnt >= 2 && out_total >= 4,
                "9/{} (非Ope日) に長谷川・石橋が重複出勤した際、フルタイム2名以上（外来計4名以上）が増員配置されていること (ft={}, total={})",
                d,
                ft_cnt,
                out_total
            );
        }
    }

    // Excel 出力で Ope日の「2階裏 人数」列数式から石橋さん列が減算され、注射日(9/9)は減算されないことの検証
    let out_custom_xlsx = PathBuf::from("target/test_v1_3_0_custom_sep.xlsx");
    let _ = fs::remove_file(&out_custom_xlsx);
    render_shifts_to_file(
        &out_custom_xlsx,
        &custom_parsed,
        &[(2026, 9, res_custom.clone())],
        &holidays,
    )
    .expect("Excel出力成功");

    let mut wb = open_workbook_auto(&out_custom_xlsx).expect("出力Excel再読込成功");
    let sheet_range = wb.worksheet_range("2026年09月シフト").unwrap();
    let header_row = sheet_range.rows().nth(1).unwrap();
    // 2階裏の「人数」列を特定（石橋さんの直後の列）
    let ishi_col_idx = header_row
        .iter()
        .position(|c| c.to_string().trim() == "石橋")
        .expect("石橋列が存在すること");
    let sec2_cnt_col = ishi_col_idx + 1;
    assert_eq!(header_row[sec2_cnt_col].to_string().trim(), "人数");

    let formula_range = wb
        .worksheet_formula("2026年09月シフト")
        .expect("数式レンジ取得成功");
    // 9/1 (Ope日: row_idx = 2) の数式には "-COUNTIF(" が含まれること
    let f_sep1 = formula_range
        .get_value((2, sec2_cnt_col as u32))
        .expect("9/1の2階裏人数数式が存在すること");
    assert!(
        f_sep1.contains("-COUNTIF("),
        "Ope日(9/1)の2階裏人数数式で石橋さん列が減算されていること: {}",
        f_sep1
    );
    // 9/9 (注射のみの日: row_idx = 10) の数式には "-COUNTIF(" が含まれないこと
    let f_sep9 = formula_range
        .get_value((10, sec2_cnt_col as u32))
        .expect("9/9の2階裏人数数式が存在すること");
    assert!(
        !f_sep9.contains("-COUNTIF("),
        "注射のみの日(9/9)の2階裏人数数式は石橋さんを含む通常のCOUNTIFであること: {}",
        f_sep9
    );
    let _ = fs::remove_file(&out_custom_xlsx);

    // ------------------------------------------------------------------------
    // 3-2 例外条件: 土曜日の午後（PM）有休設定時に実質2人勤務が許容されることの検証
    // ------------------------------------------------------------------------
    let mut sat_pm_parsed = parsed.clone();
    let cell_pm = calamine::Data::String("2026/09/12(PM)".to_string());
    let pm_dates = parse_pm_dates_for_months(&cell_pm, 2026, &[(2026, 9)], Some(&holidays));
    let sat_sep12 = NaiveDate::from_ymd_opt(2026, 9, 12).unwrap();
    assert!(pm_dates.contains(&sat_sep12));

    // 9/12(土) に江原さんが PM有休、小島さんが指定公休を取得して残り2名（長谷川・堀）のみ出勤可能なケースでも Stage 1 で成立すること
    for st in &mut sat_pm_parsed.staffs {
        if st.name == "江原" {
            st.vacation_dates.insert(sat_sep12);
            st.pm_leave_dates.insert(sat_sep12);
        } else if st.name == "小島" {
            st.specified_off_dates.insert(sat_sep12);
        }
    }
    let res_sat_pm = solve_shift(&sat_pm_parsed, 2026, 9, &holidays);
    assert!(
        res_sat_pm.success && res_sat_pm.stage == 1,
        "土曜PM有休による実質2人勤務例外が Stage 1 で成立すること"
    );
    let sat12_cnt: u32 = ["長谷川", "小島", "堀", "江原", "石橋"]
        .iter()
        .map(|&n| res_sat_pm.shifts[n][&12] as u32)
        .sum();
    assert_eq!(sat12_cnt, 2, "9/12(土) の終日出勤人数が2名として許容されること");

    // ------------------------------------------------------------------------
    // 全7ヶ月連続生成（2026年9月〜2027年3月）がすべて Stage 1 (Optimal) で解けることの検証
    // ------------------------------------------------------------------------
    let out_7m = PathBuf::from("target/test_v1_3_0_all_7months.xlsx");
    let _ = fs::remove_file(&out_7m);
    let opts_7m = GenerateOptions {
        target_months: None, // ひな形の全対象月 (2026/09〜2027/03)
        output_path: Some(out_7m.clone()),
    };
    let (_, results_7m) = ShiftGenerator::generate(&parsed, &holidays, &opts_7m, |_, _, _, _, _| {})
        .expect("全7ヶ月連続生成に成功すること");
    assert_eq!(results_7m.len(), 7);
    for (y, m, r) in &results_7m {
        assert!(
            r.success && r.stage == 1,
            "{}年{:02}月 が Stage 1 (厳格探索) で最適解を得られること (stage={})",
            y,
            m,
            r.stage
        );
    }
    let _ = fs::remove_file(&out_7m);
}

// ============================================================================
// 14. test_14_v1_3_1_conflict_and_crash_prevention:
//     v1.3.1 Category 1 & Category 2 ソルバー破綻（INFEASIBLE）および重み逆転防止の検証
// ============================================================================
#[test]
fn test_14_v1_3_1_conflict_and_crash_prevention() {
    let (parsed, holidays) = load_test_excel();

    // ------------------------------------------------------------------------
    // ケース1 (Category 1):
    //   - 石橋さんの前月末尾シフトが5連勤 [1,1,1,1,1] かつ 当月1日(9/1)が Ope日（必須出勤）
    //   - 非コアスタッフ（後藤さん）の基本公休が空欄（週6稼働設定）で連勤上限(5日)と衝突する設定
    // ------------------------------------------------------------------------
    let mut case1 = parsed.clone();
    case1
        .prev_month_tail_shifts
        .insert("石橋".to_string(), vec![0, 0, 1, 1, 1, 1, 1]);
    case1
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), "Vit,cat".to_string());
    let goto = case1
        .staffs
        .iter_mut()
        .find(|s| s.name == "後藤")
        .expect("ひな形に後藤さんが存在すること");
    goto.basic_off_days.clear();
    let res1 = solve_shift(&case1, 2026, 9, &holidays);
    assert!(
        res1.success && res1.stage == 1,
        "ケース1（前月5連勤＋1日Ope必須＋非コア週6設定）がクラッシュせず Stage 1 で解けること"
    );
    assert_eq!(res1.shifts["石橋"][&1], 1, "石橋さんが 9/1(Ope日) に出勤できていること");
    // 後藤さんが月内で5連勤上限を守っていること
    let mut goto_streak = 0;
    for d in 1..=30 {
        if res1.shifts["後藤"][&d] == 1 {
            goto_streak += 1;
            assert!(goto_streak <= 5, "非コアスタッフ（後藤）も5連勤上限を守っていること");
        } else {
            goto_streak = 0;
        }
    }

    // ------------------------------------------------------------------------
    // ケース2 (Category 1):
    //   - 特別枠（堀さん: is_special）および通常コアスタッフ（小島さん）に大量の希望休（14日分）を設定し、
    //     従来の所定日数(16日/20日)が物理的出勤可能日数を超過するケース
    // ------------------------------------------------------------------------
    let mut case2 = parsed.clone();
    for st in &mut case2.staffs {
        if st.name == "堀" || st.name == "小島" {
            for d in 1..=18 {
                if let Some(dt) = NaiveDate::from_ymd_opt(2026, 9, d) {
                    st.preferred_off_dates.insert(dt);
                }
            }
        }
    }
    let res2 = solve_shift(&case2, 2026, 9, &holidays);
    assert!(
        res2.success,
        "ケース2（堀さん・小島さんに大量の希望休設定）でも INFEASIBLE クラッシュせず解けること"
    );
    assert!(
        res2.target_days["堀"] <= 12 && res2.target_days["小島"] <= 12,
        "希望休超過時に target_days が物理的最大出勤可能日数以下に自動クランプされること: 堀={}, 小島={}",
        res2.target_days["堀"],
        res2.target_days["小島"]
    );

    // ------------------------------------------------------------------------
    // ケース3 (Category 1 & 2):
    //   - 同一週（9/7月〜9/12土の6日間すべて）に Ope日が6日連続で設定された極端ケース
    //     （石橋さんのOpe必須6日 vs 5連勤上限、および2階裏検査スタッフ2名必須×6日 vs 週5日上限）
    // ------------------------------------------------------------------------
    let mut case3 = parsed.clone();
    for d in 7..=12 {
        case3
            .ope_dict
            .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap(), "Vit,cat".to_string());
    }
    let res3 = solve_shift(&case3, 2026, 9, &holidays);
    assert!(
        res3.success && res3.stage == 1,
        "ケース3（週6日すべてOpe日の極端な週）でも供給量ガードにより Stage 1 で破綻せず解けること (success={}, stage={}, logs={:?})",
        res3.success,
        res3.stage,
        res3.logs
    );
    let ishi_w1_sum: u32 = (7..=12).map(|d| res3.shifts["石橋"][&d] as u32).sum();
    assert!(
        ishi_w1_sum <= 5,
        "週6日Opeがあっても石橋さんの5連勤上限が守られること (w1_sum={})",
        ishi_w1_sum
    );

    // ------------------------------------------------------------------------
    // ケース4 (Category 2):
    //   - df_rules の HARD ペア（田中島,梶原：全日最低1名）の片方（田中島さん）が1週間（9/7月〜9/12土）丸ごと有休を取得し、
    //     もう片方（梶原さん）が6連勤を強制されそうになるケース
    // ------------------------------------------------------------------------
    let mut case4 = parsed.clone();
    if let Some(st) = case4.staffs.iter_mut().find(|s| s.name == "田中島") {
        for d in 7..=12 {
            st.vacation_dates
                .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        }
    }
    let res4 = solve_shift(&case4, 2026, 9, &holidays);
    assert!(
        res4.success && res4.stage == 1,
        "ケース4（HARDペアの片方が1週間全休）でも連勤上限と衝突せず Stage 1 で解けること"
    );
    let kaji_w1_sum: u32 = (7..=12).map(|d| res4.shifts["梶原"][&d] as u32).sum();
    assert_eq!(
        kaji_w1_sum, 5,
        "相方が1週間全休のとき、梶原さんは連勤上限(5日)まで出勤して1日公休を確保すること"
    );

    // ------------------------------------------------------------------------
    // ケース5 (Category 3):
    //   - 標準ひな形で誤警告が出ないこと（除外スタッフの高橋・森川を含むルールでも警告0件）
    //   - 和暦（令和8年9月15日, R8/9/18）の日付パースが正常に動作すること
    //   - MonthContext::new に異常月（month=99）が渡されてもパニックせず安全にクランプされること
    //   - 事前診断ログ（⚠️ [事前診断]）がケース1・ケース3の SolveResult.logs に記録されていること
    // ------------------------------------------------------------------------
    assert!(
        parsed.parse_warnings.is_empty(),
        "標準ひな形では parse_warnings が空であること: {:?}",
        parsed.parse_warnings
    );

    let reiwa_cell = calamine::Data::String("令和8年9月15日, R8/9/18".to_string());
    let reiwa_dates = parse_date_list(&reiwa_cell, 2026, None);
    assert!(reiwa_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()));
    assert!(reiwa_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 18).unwrap()));

    let safe_ctx = MonthContext::new(2026, 99, &holidays);
    assert_eq!(safe_ctx.month, 12, "異常な月(99)でもパニックせず 12月にクランプされること");

    assert!(
        res1.logs.iter().any(|l| l.contains("⚠️ [事前診断]")),
        "ケース1で事前診断ログが出力されていること"
    );
    assert!(
        res3.logs.iter().any(|l| l.contains("⚠️ [事前診断]")),
        "ケース3で事前診断ログが出力されていること"
    );
}

// ============================================================================
// 15. test_15_v1_3_2_excel_datetime_single_date_and_regression:
//     Excel で「2/15」を単体入力して「2月15日(2026-02-15)」の日付型セルに変換されても、
//     対象月（2026年9月〜2027年3月）に合わせて 2027-02-15 の有休として正しく生成に反映されること
// ============================================================================
#[test]
fn test_15_v1_3_2_excel_datetime_single_date_and_regression() {
    use calamine::{ExcelDateTime, ExcelDateTimeType};
    use rust_xlsxwriter::{ExcelDateTime as WxDateTime, Format as WxFormat, Workbook as WxWorkbook};

    let holidays = get_holidays(2024, 2030);

    // 1. 実 Excel ファイル上での単体日付セル（DateTime 2026-02-15、表示書式 "m月d日"）読み込み E2E 検証
    let tmp_xlsx = PathBuf::from("target/test_v1_3_2_single_date.xlsx");
    let _ = fs::remove_file(&tmp_xlsx);
    {
        let mut wb = WxWorkbook::new();
        {
            let ws = wb.add_worksheet();
            ws.set_name("スタッフマスタ").unwrap();
            ws.write_string(0, 0, "氏名").unwrap();
            ws.write_string(0, 1, "所属").unwrap();
            ws.write_string(0, 2, "職種").unwrap();
            ws.write_string(0, 3, "雇用形態").unwrap();
            ws.write_string(0, 4, "基本勤務パターン").unwrap();
            ws.write_string(0, 5, "有休（指定日）").unwrap();

            ws.write_string(1, 0, "小島").unwrap();
            ws.write_string(1, 1, "2階裏").unwrap();
            ws.write_string(1, 2, "スタッフ").unwrap();
            ws.write_string(1, 3, "正社員").unwrap();
            ws.write_string(1, 4, "週5日").unwrap();
            // Excel が「2/15」単体入力時に自動作成する 2026-02-15 の日付シリアルセル＋「2月15日」書式
            let dt = WxDateTime::from_ymd(2026, 2, 15).unwrap();
            let fmt_md = WxFormat::new().set_num_format("m\"月\"d\"日\"");
            ws.write_with_format(1, 5, &dt, &fmt_md).unwrap();
        }
        {
            let ws = wb.add_worksheet();
            ws.set_name("手術スケジュール").unwrap();
            ws.write_string(0, 0, "日付").unwrap();
            ws.write_string(0, 1, "手術種別").unwrap();
            ws.write_string(1, 0, "2026-09-02").unwrap();
            ws.write_string(1, 1, "Vit,cat").unwrap();
            ws.write_string(2, 0, "2027-02-03").unwrap();
            ws.write_string(2, 1, "cat").unwrap();
        }
        wb.save(&tmp_xlsx).unwrap();
    }

    let parsed_tmp = parse_shift_excel(&tmp_xlsx, &holidays).expect("単体日付テストExcel読込成功");
    let _ = fs::remove_file(&tmp_xlsx);
    let kojima_tmp = parsed_tmp
        .staffs
        .iter()
        .find(|s| s.name == "小島")
        .expect("小島さんが読み込まれていること");
    assert!(
        kojima_tmp
            .vacation_dates
            .contains(&NaiveDate::from_ymd_opt(2027, 2, 15).unwrap()),
        "Excel日付型セル 2026-02-15 が対象月の 2027-02-15 としてパースされること: {:?}",
        kojima_tmp.vacation_dates
    );

    // 2. 標準ひな形で小島さんの有休欄に ExcelDateTime(2026-02-15) が入力された場合の 2027年2月 シフト生成検証
    let (mut parsed, _) = load_test_excel();
    let single_dt_cell = calamine::Data::DateTime(ExcelDateTime::new(
        46068.0, // 2026-02-15
        ExcelDateTimeType::DateTime,
        false,
    ));
    let parsed_vac = parse_date_list_for_months(
        &single_dt_cell,
        2026,
        &parsed.target_months,
        Some(&holidays),
    );
    assert!(parsed_vac.contains(&NaiveDate::from_ymd_opt(2027, 2, 15).unwrap()));

    if let Some(kojima) = parsed.staffs.iter_mut().find(|s| s.name == "小島") {
        kojima.vacation_dates.extend(parsed_vac);
    }

    let res_feb = solve_shift(&parsed, 2027, 2, &holidays);
    assert!(
        res_feb.success && res_feb.stage == 1,
        "2027年2月のシフトが Stage 1 で解けること"
    );
    assert_eq!(
        res_feb.shifts["小島"][&15], 0,
        "2027年2月15日(月)が有休指定どおり休み(0)になること"
    );
}







