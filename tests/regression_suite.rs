//! 勤務シフト自動生成システム 統合回帰テストスイート (`tests/regression_suite.rs`)
//!
//! Version: v2.0.0
//! Updated: 2026-11-21 - v2.0.0 厳格探索・適切解なし診断＆承認緩和フロー・バグ修正(除外/同時休み/未知条件/PM厳格化/属性列/共通休診⚠️除外/生成診断レポート)の統合検証対応
//! - v1.3.5 (2026-11-17): 2階裏の日次最低出勤人数（有休1名まで減算なし・2名以上減算）、堀さん週4基準維持・非常時のみ週5化、職種別隠れ必須人数の事前診断検証追加
//! - v1.3.4 (2026-11-17): vit（およびVit,cat）がシフト条件上でcatと同一扱いになることの検証追加
//! - v1.3.3 (2026-11-17): test_16 手術種別4種統一（Vit,cat→vit扱い）＆新ひな形「作成対象」列廃止検証追加
//! - v1.3.2 (2026-11-10): test_15 Excel単体日付セル（2/15→2026-02-15シリアル値）の翌年補完＆2027年2月ソルブ反映検証追加
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
use shift_core::config::{RelaxationKind, RelaxationScope};
use shift_core::generator::{GenerateOptions, MonthStepOutcome, ShiftGenerationSession, ShiftGenerator};
use shift_core::holidays::{calculate_japanese_holidays, get_holidays};
use shift_core::parser::{
    excel_serial_to_date, parse_date_list, parse_date_list_for_months, parse_shift_excel,
    ParsedExcelData,
};
use shift_core::renderer::render_shifts_to_file;
use shift_core::solver::{
    parse_rule_target_dates, solve_shift, solve_shift_with_relaxations, MonthContext, SolveStatus,
};
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
// 4. test_04_holiday_week_min_1_off: 祝日週の所定勤務日数最優先および合理的公休配分検証
// ============================================================================
#[test]
fn test_04_holiday_week_min_1_off() {
    let (parsed, holidays) = load_test_excel();
    // 2027年2月 (2/11木・2/23火が平日祝日)
    let res = solve_shift(&parsed, 2027, 2, &holidays);
    assert!(res.success && res.stage == 1, "2027年2月が厳格条件で成功すること");

    let ctx = MonthContext::new(2027, 2, &holidays);
    // 全コアスタッフの月間所定勤務日数が100%厳守されていること
    assert!(
        res.audit.target_mismatches.is_empty(),
        "所定勤務日数不一致がないこと: {:?}",
        res.audit.target_mismatches
    );

    // 平日祝日5診療日週において、固定公休曜日が祝日に当たらないORスタッフは公休を1日以上確保していること
    for week in &ctx.weeks {
        if week.has_weekday_holiday && week.clinic_days.len() == 5 {
            for staff in &parsed.staffs {
                if staff.is_core
                    && staff.pattern != "除外"
                    && staff.basic_off_type == shift_core::parser::OffType::Or
                {
                    let staff_shifts = &res.shifts[&staff.name];
                    let week_work_cnt: u32 =
                        week.clinic_days.iter().map(|d| staff_shifts[d] as u32).sum();
                    assert!(
                        week_work_cnt <= 4,
                        "ORスタッフ [{}] は平日祝日週 (week {}) で公休が優先配分されていること (実出勤: {}日)",
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
    // 2階裏（外来：Ope日のOpe専従者除く）コアスタッフの出勤数が 4名以上となる日が最低 2日以上存在すること
    let core_2f_back: Vec<&shift_core::parser::Staff> = parsed
        .staffs
        .iter()
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();

    for week in &ctx.weeks {
        if !week.has_holiday && week.clinic_days.len() == 6 {
            let mut days_with_4plus = 0;
            for &d in &week.clinic_days {
                let dt = ctx.date_map[&d];
                let is_ope = shift_core::parser::is_ope_date(&parsed.ope_dict, &dt);
                let cnt: u32 = core_2f_back
                    .iter()
                    .filter(|s| !(is_ope && s.is_ope_dedicated))
                    .map(|s| res.shifts[&s.name][&d] as u32)
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

    // 榊原さんの前月末尾実績として 4連勤 (2026年8月末 8/28〜8/31 が出勤: 1, 1, 1, 1) を設定
    parsed.prev_sheet_year_month = Some((2026, 8));
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
    assert!(
        sheet_names.contains(&"生成診断レポート".to_string()),
        "シート名 '生成診断レポート' が末尾に存在すること: {:?}",
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
        ..Default::default()
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

    // 2. 2026年11月シフト生成（小島さん全休週を含むため厳格探索では適切解なしとなり、推奨緩和承認後に成功すること）
    let res_strict = solve_shift(&parsed, 2026, 11, &holidays);
    assert!(
        !res_strict.success && res_strict.status == SolveStatus::Infeasible,
        "2026年11月は小島さん全休週の検査不足等により無緩和では適切解なし(Infeasible)と明示されること"
    );
    let diag = res_strict
        .diagnosis
        .as_ref()
        .expect("適切解なし時に診断結果が付与されること");
    assert!(!diag.tier0_unsolvable, "緩和候補により解決可能であること");
    let rec_relax: HashSet<RelaxationKind> = diag
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    assert!(!rec_relax.is_empty(), "推奨緩和候補が1件以上提示されること");

    let res = solve_shift_with_relaxations(&parsed, 2026, 11, &holidays, &rec_relax);
    assert!(
        res.success && res.stage == 2,
        "推奨緩和を承認して再実行すると2026年11月のシフト生成が成功すること: {:?}",
        res.logs
    );

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
        ..Default::default()
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
        "タイポ 'cay' および 'Viit,cat' が「cat」手術日として漏れなく検出されること"
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
    assert_eq!(
        hori_actual, hori_target,
        "v2.0.0: 通常月は承認なしでは堀さんの出勤日数 ({}) が週4基準の所定日数 ({}) 通りであること",
        hori_actual,
        hori_target
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

    // 4. 2階裏_基本人数 (sec_2f_back_base_cap = 3) の日次最低人数確保検証（有休1名までは減算せず3名維持、2名以上は減算）
    let res_sep = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res_sep.success && res_sep.stage == 1);
    let core_2f_b: Vec<&shift_core::parser::Staff> = parsed
        .staffs
        .iter()
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
        .collect();
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        let is_ope = shift_core::parser::is_ope_date(&parsed.ope_dict, &dt);
        let vac_cnt = core_2f_b
            .iter()
            .filter(|s| {
                !(is_ope && s.is_ope_dedicated)
                    && (s.vacation_dates.contains(&dt)
                        || s.special_vacation_dates.contains(&dt)
                        || s.preferred_off_dates.contains(&dt)
                        || s.specified_off_dates.contains(&dt))
            })
            .count() as i32;
        let vac_deduct = if is_ope { 0 } else { (vac_cnt - 1).max(0) };
        let expected_min = (parsed.config.sec_2f_back_base_cap - vac_deduct).max(0) as u32;
        let actual_2f_b: u32 = core_2f_b
            .iter()
            .filter(|s| !(is_ope && s.is_ope_dedicated))
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

    // 5. 過去最大3ヶ月分の主公休ローテーション検証 (2026/11の推奨緩和を事前承認した4ヶ月連続生成)
    let diag_nov_res = solve_shift(&parsed, 2026, 11, &holidays);
    assert!(!diag_nov_res.success && diag_nov_res.status == SolveStatus::Infeasible);
    let rec_nov: HashSet<RelaxationKind> = diag_nov_res
        .diagnosis
        .as_ref()
        .unwrap()
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    let mut approved_map = HashMap::new();
    approved_map.insert((2026, 11), rec_nov);

    let out_rot = PathBuf::from("target/test_v1_2_0_rot4m.xlsx");
    let _ = fs::remove_file(&out_rot);
    let rot_opts = GenerateOptions {
        target_months: Some(vec![(2026, 9), (2026, 10), (2026, 11), (2026, 12)]),
        output_path: Some(out_rot.clone()),
        approved_relaxations_by_month: approved_map,
    };
    let (_, rot_results) = ShiftGenerator::generate(&parsed, &holidays, &rot_opts, |_, _, _, _, _| {})
        .expect("4ヶ月連続生成が成功すること");
    assert!(
        rot_results.iter().all(|(_, _, r)| r.success),
        "承認済み緩和を含め4ヶ月すべて成功すること"
    );
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
        ..Default::default()
    };
    let (_, sep_guard_res) =
        ShiftGenerator::generate(&guard_parsed, &holidays, &guard_opts_sep, |_, _, _, _, _| {})
            .expect("9月生成成功");
    assert!(sep_guard_res[0].2.success);

    // 一方、2027年1月（2026年12月の翌月）を生成した場合は12月末尾実績が正しく適用されること
    let guard_opts_jan = GenerateOptions {
        target_months: Some(vec![(2027, 1)]),
        output_path: Some(out_guard.clone()),
        ..Default::default()
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
    assert_eq!(parsed.rules.len(), 9, "標準ひな形のシフト制約条件が重複整理後の9件であること");
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
    let res_nov_strict = solve_shift(&parsed, 2026, 11, &holidays);
    assert!(!res_nov_strict.success && res_nov_strict.status == SolveStatus::Infeasible);
    let rec_nov_for_13: HashSet<RelaxationKind> = res_nov_strict
        .diagnosis
        .as_ref()
        .unwrap()
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    let res_nov = solve_shift_with_relaxations(&parsed, 2026, 11, &holidays, &rec_nov_for_13);
    assert!(res_nov.success && res_nov.stage == 2, "2026年11月が推奨緩和承認で解けること");
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
        .filter(|s| {
            s.section == "2階裏"
                && (s.is_core || s.is_holiday_independent_week4 || s.is_ope_dedicated)
                && s.pattern != "除外"
        })
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
                // 3-1: 火曜以外のOpe日で 看護師1〜2＋検査2（計3〜4名、石橋さん除く）が達成されていること
                // （v1.3.5より堀さんの週4維持 10M > Ope推奨 3.5M のため、堀さんを無理に週5化せず 看護師>=1, 検査==2 を維持）
                assert!(
                    (out_nurse == 1 || out_nurse == 2) && out_exam == 2,
                    "9/{} (Ope日) で看護師1〜2名＋検査2名（石橋さん除く）が達成されていること",
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

    }

    // 3-3(2): 非Ope日に長谷川さんと石橋さんが重複出勤している場合、Ope推奨(5.5M)優先下でも可能な日はフルタイム2名以上（外来計4名以上）が増員配置されること
    // (※8日ものOpe＋2日の注射日を詰め込んだ custom_parsed では看護師総数30日 < 全推奨33日となるため過半数の重複日でFT+1達成を確認し、通常負荷月での全達成も別途検証)
    let overlap_non_ope_days: Vec<u32> = ctx_sep
        .clinic_days
        .iter()
        .copied()
        .filter(|&d| {
            let dt = ctx_sep.date_map[&d];
            !is_ope_date(&custom_parsed.ope_dict, &dt)
                && res_custom.shifts["長谷川"][&d] == 1
                && res_custom.shifts["石橋"][&d] == 1
        })
        .collect();
    if !overlap_non_ope_days.is_empty() {
        let fulfilled_overlap = overlap_non_ope_days
            .iter()
            .filter(|&&d| {
                let ft_cnt: u32 = core_2f_b
                    .iter()
                    .filter(|s| !s.is_early_leave)
                    .map(|s| res_custom.shifts[&s.name][&d] as u32)
                    .sum();
                let tot_cnt: u32 = core_2f_b
                    .iter()
                    .map(|s| res_custom.shifts[&s.name][&d] as u32)
                    .sum();
                ft_cnt >= 2 && tot_cnt >= 4
            })
            .count();
        assert!(
            fulfilled_overlap * 2 >= overlap_non_ope_days.len(),
            "過密Ope月でも非Ope早上がり重複日の過半数でFT増員(4名以上)が確保されること ({fulfilled_overlap}/{})",
            overlap_non_ope_days.len()
        );
    }

    // 通常負荷の9月（ひな形そのまま）の 4-A 優先順検証:
    // 2階裏4名枠（月8枠）に対し ① Ope推奨(5.5M) > ② 非Ope早上がり重複FT+1(3.5M) で優先配分されること
    let res_norm_sep = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res_norm_sep.success && res_norm_sep.stage == 1);
    let mut non_tue_ope_4_ok = 0usize;
    let mut non_tue_ope_total = 0usize;
    let mut norm_overlap_ok = 0usize;
    let mut norm_overlap_total = 0usize;
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        let w = ctx_sep.weekday_map[&d];
        let is_ope = is_ope_date(&parsed.ope_dict, &dt);
        if is_ope && w != 1 {
            non_tue_ope_total += 1;
            let ns_cnt: u32 = core_2f_b
                .iter()
                .filter(|s| !s.is_ope_dedicated && s.is_nurse())
                .map(|s| res_norm_sep.shifts[&s.name][&d] as u32)
                .sum();
            let ex_cnt: u32 = core_2f_b
                .iter()
                .filter(|s| !s.is_ope_dedicated && s.is_exam())
                .map(|s| res_norm_sep.shifts[&s.name][&d] as u32)
                .sum();
            if ns_cnt >= 2 && ex_cnt >= 2 {
                non_tue_ope_4_ok += 1;
            }
        }
        if !is_ope && res_norm_sep.shifts["長谷川"][&d] == 1 && res_norm_sep.shifts["石橋"][&d] == 1 {
            norm_overlap_total += 1;
            let ft_cnt: u32 = core_2f_b
                .iter()
                .filter(|s| !s.is_early_leave)
                .map(|s| res_norm_sep.shifts[&s.name][&d] as u32)
                .sum();
            let out_total: u32 = core_2f_b
                .iter()
                .map(|s| res_norm_sep.shifts[&s.name][&d] as u32)
                .sum();
            if ft_cnt >= 2 && out_total >= 4 {
                norm_overlap_ok += 1;
            }
        }
    }
    assert!(
        non_tue_ope_total > 0 && non_tue_ope_4_ok >= 4,
        "通常9月の火曜以外Ope日({non_tue_ope_total}日)でOpe推奨(Ns>=2&検>=2)が優先達成されること ({non_tue_ope_4_ok}/{non_tue_ope_total})"
    );
    if norm_overlap_total >= 2 {
        assert!(
            norm_overlap_ok >= 2,
            "残余の4名枠が非Ope早上がり重複日に複数日配分されること ({norm_overlap_ok}/{norm_overlap_total})"
        );
    }

    // 4名枠に余裕がある月（9月Opeを2日のみ残した場合）では、全非Ope早上がり重複日で100% フルタイム+1増員（計4名以上）が達成されること
    let mut low_ope_sep = parsed.clone();
    low_ope_sep.ope_dict.retain(|dt, _| {
        !(dt.year() == 2026
            && dt.month() == 9
            && !matches!(dt.day(), 2 | 4))
    });
    let res_low_ope = solve_shift(&low_ope_sep, 2026, 9, &holidays);
    assert!(res_low_ope.success && res_low_ope.stage == 1);
    for &d in &ctx_sep.clinic_days {
        let dt = ctx_sep.date_map[&d];
        if !is_ope_date(&low_ope_sep.ope_dict, &dt)
            && res_low_ope.shifts["長谷川"][&d] == 1
            && res_low_ope.shifts["石橋"][&d] == 1
        {
            let ft_cnt: u32 = core_2f_b
                .iter()
                .filter(|s| !s.is_early_leave)
                .map(|s| res_low_ope.shifts[&s.name][&d] as u32)
                .sum();
            let out_total: u32 = core_2f_b
                .iter()
                .map(|s| res_low_ope.shifts[&s.name][&d] as u32)
                .sum();
            assert!(
                ft_cnt >= 2 && out_total >= 4,
                "余裕枠月(low_ope_sep)の9/{d} (非Ope日) の早上がり重複時は100% フルタイム2名以上（計4名以上）であること (ft={ft_cnt}, total={out_total})"
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
    // 全7ヶ月検証: 通常6ヶ月は無緩和厳格 (stage==1)、11月は推奨緩和承認後 (stage==2) で連続生成成功
    // ------------------------------------------------------------------------
    for &(y, m) in &[(2026, 9), (2026, 10), (2026, 12), (2027, 1), (2027, 2), (2027, 3)] {
        let r = solve_shift(&parsed, y, m, &holidays);
        assert!(
            r.success && r.stage == 1,
            "通常月 {}年{:02}月が無緩和・厳格条件 (stage=1) で解けること: {:?}",
            y, m, r.logs
        );
    }

    let mut approved_map_7m = HashMap::new();
    approved_map_7m.insert((2026, 11), rec_nov_for_13);

    let out_7m = PathBuf::from("target/test_v1_3_0_all_7months.xlsx");
    let _ = fs::remove_file(&out_7m);
    let opts_7m = GenerateOptions {
        target_months: None, // ひな形の全対象月 (2026/09〜2027/03)
        output_path: Some(out_7m.clone()),
        approved_relaxations_by_month: approved_map_7m,
    };
    let (_, results_7m) = ShiftGenerator::generate(&parsed, &holidays, &opts_7m, |_, _, _, _, _| {})
        .expect("全7ヶ月連続生成に成功すること");
    assert_eq!(results_7m.len(), 7);
    for (y, m, r) in &results_7m {
        let expected_stage = if *y == 2026 && *m == 11 { 2 } else { 1 };
        assert!(
            r.success && r.stage == expected_stage,
            "{}年{:02}月 が期待通り stage={} で完了すること (actual stage={}, success={})",
            y,
            m,
            expected_stage,
            r.stage,
            r.success
        );
    }
    let _ = fs::remove_file(&out_7m);
}

// ============================================================================
// 14. test_14_v2_0_0_strict_stop_and_diagnosis:
//     v2.0.0 厳格停止（「適切解なし」明示・無断緩和廃止）・原因診断＆推奨緩和提案の検証
// ============================================================================
#[test]
fn test_14_v1_3_1_conflict_and_crash_prevention() {
    let (parsed, holidays) = load_test_excel();

    // ------------------------------------------------------------------------
    // ケース1a (Tier 0 物理矛盾・緩和不可):
    //   - 非コアスタッフ（後藤さん）の基本公休が空欄（週6稼働設定）で連勤上限(5日)と衝突する設定
    //   -> v2.0.0: 無断連勤スキップせず Tier 0 競合 (tier0_unsolvable=true) で停止すること
    // ------------------------------------------------------------------------
    let mut case1a = parsed.clone();
    let goto = case1a
        .staffs
        .iter_mut()
        .find(|s| s.name == "後藤")
        .expect("ひな形に後藤さんが存在すること");
    goto.basic_off_days.clear();
    let res1a = solve_shift(&case1a, 2026, 9, &holidays);
    assert!(
        !res1a.success && res1a.status == SolveStatus::Infeasible,
        "ケース1a（非コア週6設定）は安易な連勤スキップを行わず適切解なしとなること"
    );
    let diag1a = res1a.diagnosis.as_ref().expect("診断結果が付与されること");
    assert!(diag1a.tier0_unsolvable, "非コア連勤競合は tier0_unsolvable=true であること");
    assert!(
        diag1a.causes.iter().any(|c| c.category.contains("パート連勤上限競合")),
        "診断原因に「パート連勤上限競合」が記録されること: {:?}",
        diag1a.causes
    );

    // ------------------------------------------------------------------------
    // ケース1b (月跨ぎ連勤 vs Ope必須):
    //   - 石橋さんの前月末尾シフトが5連勤 [0,0,1,1,1,1,1] かつ 当月1日(9/1)が Ope日（必須出勤）
    //   -> v2.0.0: 前月実績を改変せず適切解なしとし、「月跨ぎ連勤上限」原因と OpeDedicatedWork(Day(1)) を提案。
    //      承認後は stage=2 で成功し、9/1は休み(5連勤上限厳守)となること。
    // ------------------------------------------------------------------------
    let mut case1b = parsed.clone();
    case1b.prev_sheet_year_month = Some((2026, 8));
    case1b
        .prev_month_tail_shifts
        .insert("石橋".to_string(), vec![0, 0, 1, 1, 1, 1, 1]);
    case1b
        .ope_dict
        .insert(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(), "Vit,cat".to_string());
    let res1b_strict = solve_shift(&case1b, 2026, 9, &holidays);
    assert!(
        !res1b_strict.success && res1b_strict.status == SolveStatus::Infeasible,
        "ケース1b（前月5連勤＋1日Ope必須）は前月改変せず厳格に適切解なしとなること"
    );
    let diag1b = res1b_strict.diagnosis.as_ref().expect("診断結果が付与されること");
    assert!(
        diag1b.causes.iter().any(|c| c.category.contains("月跨ぎ連勤上限")),
        "原因に「月跨ぎ連勤上限」が含まれること: {:?}",
        diag1b.causes
    );
    let expected_relax_1b = RelaxationKind::OpeDedicatedWork {
        staff_name: "石橋".to_string(),
        scope: RelaxationScope::Day(1),
    };
    assert!(
        diag1b.proposals.iter().any(|p| p.kind == expected_relax_1b),
        "緩和提案に 9/1 Ope必須免除が含まれること: {:?}",
        diag1b.proposals
    );
    let mut app_1b = HashSet::new();
    app_1b.insert(expected_relax_1b);
    let res1b_relaxed = solve_shift_with_relaxations(&case1b, 2026, 9, &holidays, &app_1b);
    assert!(
        res1b_relaxed.success && res1b_relaxed.stage == 2,
        "Ope必須免除承認後は stage=2 で成功すること: {:?}",
        res1b_relaxed.logs
    );
    assert_eq!(res1b_relaxed.shifts["石橋"][&1], 0, "連勤上限(5日)により9/1は休みであること");

    // ------------------------------------------------------------------------
    // ケース2 (希望休超過 -> 無断クランプ廃止 & 最終手段 TargetDaysAdjust):
    //   - 2a: 大量希望休（14日超） -> target無断クランプせず Infeasible & 個人出勤上限不足原因
    //   - 2b: 小島さんの希望休を増やして所定勤務日数より1日だけ出勤不能にする -> TargetDaysAdjust(-1日) 提案＆承認で成功
    // ------------------------------------------------------------------------
    let mut case2a = parsed.clone();
    for st in &mut case2a.staffs {
        if st.name == "堀" || st.name == "小島" {
            for d in 1..=18 {
                if let Some(dt) = NaiveDate::from_ymd_opt(2026, 9, d) {
                    st.preferred_off_dates.insert(dt);
                }
            }
        }
    }
    let res2a = solve_shift(&case2a, 2026, 9, &holidays);
    assert!(
        !res2a.success && res2a.status == SolveStatus::Infeasible,
        "ケース2a（大量希望休）は無断クランプせず適切解なしとなること"
    );
    let diag2a = res2a.diagnosis.as_ref().unwrap();
    assert!(
        diag2a.causes.iter().any(|c| c.category.contains("個人出勤上限不足")),
        "原因に「個人出勤上限不足」が含まれること: {:?}",
        diag2a.causes
    );

    // 2b: 小島さん(9月稼働可能21日, target=19日)に対し、連勤休みが入らない週(9/5土, 9/26土, 9/28月)に3日希望休を与えて
    // 最大出勤可能日数をちょうど18日 (< target=19日: 1日だけ不足) とする -> TargetDaysAdjust(-1日) 提案＆承認で成功すること
    let mut case2b = parsed.clone();
    if let Some(kojima) = case2b.staffs.iter_mut().find(|s| s.name == "小島") {
        for d in [5u32, 26, 28] {
            kojima
                .preferred_off_dates
                .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        }
    }
    let res2b = solve_shift(&case2b, 2026, 9, &holidays);
    assert!(!res2b.success && res2b.status == SolveStatus::Infeasible);
    let diag2b = res2b.diagnosis.as_ref().unwrap();
    assert!(!diag2b.tier0_unsolvable, "±1日以内の不足はTargetDaysAdjustで救済提案可能なこと");
    let has_target_adj = diag2b.proposals.iter().any(|p| {
        p.is_last_resort
            && matches!(
                p.kind,
                RelaxationKind::TargetDaysAdjust {
                    staff_name: Some(ref n),
                    delta: -1
                } if n == "小島"
            )
    });
    assert!(
        has_target_adj,
        "小島さんの所定日数-1日調整が最終手段として提案されること: {:?}",
        diag2b.proposals
    );
    let app_2b: HashSet<RelaxationKind> = diag2b
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    let res2b_ok = solve_shift_with_relaxations(&case2b, 2026, 9, &holidays, &app_2b);
    assert!(
        res2b_ok.success && res2b_ok.stage == 2,
        "推奨緩和承認後に解けること: {:?}",
        res2b_ok.logs
    );

    // ------------------------------------------------------------------------
    // ケース3:
    //   - 同一週（9/7月〜9/12土の6日間すべて）に Ope日が6日連続で設定された極端ケース
    //   -> v2.0.0: 厳格探索では破綻として明示停止し、推奨緩和承認後に5連勤上限を守って解けること
    // ------------------------------------------------------------------------
    let mut case3 = parsed.clone();
    for d in 7..=12 {
        case3
            .ope_dict
            .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap(), "Vit,cat".to_string());
    }
    let res3_strict = solve_shift(&case3, 2026, 9, &holidays);
    assert!(
        !res3_strict.success && res3_strict.status == SolveStatus::Infeasible,
        "ケース3（週6日Ope）は厳格探索で適切解なしとなること"
    );
    let diag3 = res3_strict.diagnosis.as_ref().unwrap();
    assert!(!diag3.tier0_unsolvable && !diag3.proposals.is_empty());
    let app_3: HashSet<RelaxationKind> = diag3
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    let res3_ok = solve_shift_with_relaxations(&case3, 2026, 9, &holidays, &app_3);
    assert!(
        res3_ok.success && res3_ok.stage == 2,
        "ケース3の推奨緩和承認後は成功すること: {:?}",
        res3_ok.logs
    );
    let ishi_w1_sum: u32 = (7..=12).map(|d| res3_ok.shifts["石橋"][&d] as u32).sum();
    assert!(
        ishi_w1_sum <= 5,
        "週6日Opeがあっても石橋さんの5連勤上限が守られること (w1_sum={})",
        ishi_w1_sum
    );

    // ------------------------------------------------------------------------
    // ケース4:
    //   - df_rules の HARD ペア（田中島,梶原：全日最低1名）の片方（田中島さん）が1週間（9/7月〜9/12土）丸ごと有休
    //   -> v2.0.0: 厳格探索で適切解なし＆SheetHardRule緩和候補提示、承認後に梶原さん5連勤上限厳守で解決すること
    // ------------------------------------------------------------------------
    let mut case4 = parsed.clone();
    if let Some(st) = case4.staffs.iter_mut().find(|s| s.name == "田中島") {
        for d in 7..=12 {
            st.vacation_dates
                .insert(NaiveDate::from_ymd_opt(2026, 9, d).unwrap());
        }
    }
    let res4_strict = solve_shift(&case4, 2026, 9, &holidays);
    assert!(
        !res4_strict.success && res4_strict.status == SolveStatus::Infeasible,
        "ケース4（HARDペア片方1週間全休）は厳格探索で適切解なしとなること"
    );
    let diag4 = res4_strict.diagnosis.as_ref().unwrap();
    assert!(
        diag4.proposals.iter().any(|p| matches!(p.kind, RelaxationKind::SheetHardRule { .. })),
        "緩和候補に SheetHardRule が含まれること: {:?}",
        diag4.proposals
    );
    let app_4: HashSet<RelaxationKind> = diag4
        .proposals
        .iter()
        .filter(|p| p.recommended)
        .map(|p| p.kind.clone())
        .collect();
    let res4_ok = solve_shift_with_relaxations(&case4, 2026, 9, &holidays, &app_4);
    assert!(
        res4_ok.success && res4_ok.stage == 2,
        "ケース4の推奨緩和承認後は成功すること: {:?}",
        res4_ok.logs
    );
    let kaji_w1_sum: u32 = (7..=12).map(|d| res4_ok.shifts["梶原"][&d] as u32).sum();
    assert_eq!(
        kaji_w1_sum, 5,
        "相方が1週間全休のとき、梶原さんは連勤上限(5日)まで出勤して1日公休を確保すること"
    );

    // ------------------------------------------------------------------------
    // ケース5:
    //   - 標準ひな形で誤警告が出ないこと
    //   - 和暦（令和8年9月15日, R8/9/18）の日付パース
    //   - MonthContext::new に異常月（month=99）が渡されてもパニックせず安全にクランプされること
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

// ============================================================================
// 16. test_16_v1_3_3_ope_types_and_no_marked_months:
//     手術種別4種統一（Vit,cat→vit扱い）および手術スケジュール「作成対象」列削除の検証
// ============================================================================
#[test]
fn test_16_v1_3_3_ope_types_and_no_marked_months() {
    use rust_xlsxwriter::Workbook as WxWorkbook;

    // 1. 新しい標準ひな形（勤務条件表_ひな形.xlsx）の「手術スケジュール」シートに「作成対象」列が存在しないこと
    let mut wb = open_workbook_auto("勤務条件表_ひな形.xlsx").expect("ひな形Excelを開けること");
    let ope_range = wb
        .worksheet_range("手術スケジュール")
        .expect("手術スケジュールシートが存在すること");
    let header: Vec<String> = ope_range
        .rows()
        .next()
        .expect("ヘッダー行が存在すること")
        .iter()
        .map(|c| c.to_string().trim().to_string())
        .collect();
    assert!(
        !header.iter().any(|h| h.contains("作成対象")),
        "手術スケジュールシートから「作成対象」列が完全に削除されていること: {:?}",
        header
    );
    assert_eq!(
        header,
        vec!["日付", "手術種別", "担当医(未参照)", "備考(未参照)"]
    );

    // 2. ひな形内の手術種別が vit / cat / gla / 下垂 の4種で読み込まれていること
    let (parsed, holidays) = load_test_excel();
    let allowed_kinds: HashSet<&str> = ["vit", "cat", "gla", "下垂"].into_iter().collect();
    for (dt, kind) in &parsed.ope_dict {
        assert!(
            allowed_kinds.contains(kind.as_str()),
            "ひな形の手術種別が4種のいずれかであること ({dt}: {kind})"
        );
    }

    // 3. 「Vit,cat」と記述された旧フォーマットファイルでも "vit" として扱われ、「cat」手術日には含まれないこと
    let tmp_xlsx = PathBuf::from("target/test_v1_3_3_vit_cat.xlsx");
    let _ = fs::remove_file(&tmp_xlsx);
    {
        let mut wx = WxWorkbook::new();
        {
            let ws = wx.add_worksheet();
            ws.set_name("スタッフマスタ").unwrap();
            ws.write_string(0, 0, "氏名").unwrap();
            ws.write_string(0, 1, "所属").unwrap();
            ws.write_string(0, 2, "職種").unwrap();
            ws.write_string(0, 3, "雇用形態").unwrap();
            ws.write_string(0, 4, "基本勤務パターン").unwrap();
            ws.write_string(1, 0, "テストA").unwrap();
            ws.write_string(1, 1, "1階").unwrap();
            ws.write_string(1, 2, "スタッフ").unwrap();
            ws.write_string(1, 3, "正社員").unwrap();
            ws.write_string(1, 4, "週5日").unwrap();
        }
        {
            let ws = wx.add_worksheet();
            ws.set_name("手術スケジュール").unwrap();
            ws.write_string(0, 0, "日付").unwrap();
            ws.write_string(0, 1, "手術種別").unwrap();
            ws.write_string(1, 0, "2026-09-02").unwrap();
            ws.write_string(1, 1, "Vit,cat").unwrap();
            ws.write_string(2, 0, "2026-09-04").unwrap();
            ws.write_string(2, 1, "cat").unwrap();
            ws.write_string(3, 0, "2026-09-09").unwrap();
            ws.write_string(3, 1, "vit").unwrap();
        }
        wx.save(&tmp_xlsx).unwrap();
    }

    let parsed_tmp = parse_shift_excel(&tmp_xlsx, &holidays).expect("Vit,catテストExcel読込成功");
    let _ = fs::remove_file(&tmp_xlsx);

    let d_sep2 = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let d_sep4 = NaiveDate::from_ymd_opt(2026, 9, 4).unwrap();
    let d_sep9 = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
    assert_eq!(parsed_tmp.ope_dict.get(&d_sep2).map(|s| s.as_str()), Some("vit"));
    assert_eq!(parsed_tmp.ope_dict.get(&d_sep4).map(|s| s.as_str()), Some("cat"));
    assert_eq!(parsed_tmp.ope_dict.get(&d_sep9).map(|s| s.as_str()), Some("vit"));

    let ctx = MonthContext::new(2026, 9, &holidays);
    let vit_days = parse_rule_target_dates("「Vit」手術日", &ctx, &parsed_tmp.ope_dict);
    let cat_days = parse_rule_target_dates("「cat」手術日", &ctx, &parsed_tmp.ope_dict);
    assert_eq!(vit_days, vec![2, 9], "vit および Vit,cat の日が Vit 手術日として扱われること");
    assert_eq!(
        cat_days,
        vec![2, 4, 9],
        "勤務シフト条件上は vit / Vit,cat も cat と同じ条件として扱われること"
    );
}

// ============================================================================
// 17. test_17_v1_3_4t_no_2f_back_vac_auto_deduction:
//     v1.3.4t テスト仕様:
//       (1) 2階裏の日次最低出勤人数は有休・指定公休1名までは減算せず3名維持、2名以上の日は(人数-1)分を減算
//       (2) 堀さん（必要に応じて週5）の超過出勤ペナルティ引き上げにより、通常月は所定日数（週4基準）に抑えられること
//       (3) 職種別の隠れ必要人数が事前診断に反映され Stage 1 で破綻しないこと
// ============================================================================
#[test]
fn test_17_v1_3_4t_no_2f_back_vac_auto_deduction() {
    let (parsed, holidays) = load_test_excel();

    // (A) WEIGHT_SPECIAL_EXTRA_WORK 引き上げ (150k -> 10M) および週休平準化基準の週4化により、
    //     通常月(2026年9月)では堀さんの週5超過出勤が一切発生せず、所定日数 (target_days=15日, 週4日勤務) ピッタリに抑えられること
    let res_sep_base = solve_shift(&parsed, 2026, 9, &holidays);
    assert!(res_sep_base.success && res_sep_base.stage == 1);
    let hori_target_sep = res_sep_base.target_days["堀"];
    let hori_actual_sep: i32 = res_sep_base.shifts["堀"].values().map(|&v| v as i32).sum();
    assert_eq!(
        hori_actual_sep, hori_target_sep,
        "2026年9月で堀さんの出勤日数({})が週4所定日数(target={})ピッタリに抑えられること",
        hori_actual_sep,
        hori_target_sep
    );

    // (B) 2026年9月カスタム:
    //   - 9/7(月): 小島=有休 (1名休み → 減算せず 3名以上維持)
    //   - 9/11(金): 堀=指定公休 (1名休み → 減算せず 3名以上維持)
    //   - 9/2(水, Ope除外): 小島=有休, 堀=指定公休 (2名休み → 1名減算して2名体制許容＆石橋さんの水曜公休も維持)
    let mut custom = parsed.clone();
    custom
        .ope_dict
        .remove(&NaiveDate::from_ymd_opt(2026, 9, 2).unwrap());

    let d_sep2 = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let d_sep7 = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
    let d_sep11 = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap();
    for st in &mut custom.staffs {
        if st.name == "小島" {
            st.vacation_dates.insert(d_sep2);
            st.vacation_dates.insert(d_sep7);
        } else if st.name == "堀" {
            st.specified_off_dates.insert(d_sep2);
            st.specified_off_dates.insert(d_sep11);
        }
    }

    let res = solve_shift(&custom, 2026, 9, &holidays);
    assert!(
        res.success && res.stage == 1,
        "2階裏(1名減算なし・2名以上減算)テストケースが Stage 1 で解けること: {:?}",
        res.logs
    );

    let names_2f_b = ["長谷川", "小島", "堀", "江原", "石橋"];
    for d in [7u32, 11u32] {
        let cnt: u32 = names_2f_b.iter().map(|&n| res.shifts[n][&d] as u32).sum();
        assert!(
            cnt >= 3,
            "9/{} は有休/指定公休が1名のため減算せず 2階裏3名以上を維持すること (実: {}名)",
            d,
            cnt
        );
    }
    let cnt_sep2: u32 = names_2f_b.iter().map(|&n| res.shifts[n][&2] as u32).sum();
    assert!(
        cnt_sep2 >= 2,
        "9/2(水) は有休/指定公休が2名のため1名減算の2名以上が確保されること (実: {}名)",
        cnt_sep2
    );
    assert_eq!(
        res.shifts["石橋"][&2], 0,
        "9/2(水) は2名有休により最低人数が2名へ減算されるため、石橋さんの非Ope水曜公休(0)も維持されること"
    );
}

// ============================================================================
// 18. test_18_v2_0_0_comprehensive_verification:
//     v2.0.0 全面改修の網羅的検証（実務7ヶ月検証・セッション停止＆再試行・全バグ修正・旧互換・レポート）
// ============================================================================
#[test]
fn test_18_v2_0_0_comprehensive_verification() {
    use rust_xlsxwriter::Workbook as WxWorkbook;
    use shift_core::parser::{parse_pm_dates_for_months, Staff};

    let (parsed, holidays) = load_test_excel();

    // ------------------------------------------------------------------------
    // 1. 通常6ヶ月（2026/09, 10, 12, 2027/01, 02, 03）連続セッション検証
    //    - 全月 stage==1 (厳格達成)
    //    - 全スタッフ target_mismatches 空
    //    - 2027年3月の小田切(22/22)・長谷川(22/22, 火曜5回中1回振替, <=5連勤)
    //    - 全通常6ヶ月で堀さんが厳格週4維持
    // ------------------------------------------------------------------------
    let mut sess = ShiftGenerationSession::new(
        &parsed,
        &holidays,
        &GenerateOptions {
            target_months: None,
            output_path: Some(PathBuf::from("target/test_v2_0_0_full7m.xlsx")),
            ..Default::default()
        },
    )
    .expect("セッション作成成功");

    let mut solved_map: HashMap<(i32, u32), shift_core::solver::SolveResult> = HashMap::new();
    let mut paused_nov_diag = None;

    while let Some((cy, cm)) = sess.current_month() {
        match sess.solve_current_month(&HashSet::new(), |_, _, _, _, _| {}) {
            MonthStepOutcome::Solved { year, month, result, .. } => {
                assert_ne!((year, month), (2026, 11), "2026/11 は無緩和では停止すること");
                assert_eq!(result.stage, 1, "{year}年{month}月は無緩和 stage=1 であること");
                assert!(
                    result.audit.target_mismatches.is_empty(),
                    "{year}年{month}月で所定勤務日数不一致がないこと: {:?}",
                    result.audit.target_mismatches
                );
                let h_tgt = result.target_days["堀"];
                let h_act: i32 = result.shifts["堀"].values().map(|&v| v as i32).sum();
                assert_eq!(h_act, h_tgt, "{year}年{month}月で堀さんが厳格週4({h_tgt}日)維持");
                solved_map.insert((year, month), result);
            }
            MonthStepOutcome::NeedsRelaxation { year, month, result } => {
                assert_eq!((year, month), (2026, 11), "一時停止は2026/11のみであること: ({year},{month})");
                let diag = result.diagnosis.clone().expect("診断結果があること");
                assert!(!diag.tier0_unsolvable);
                paused_nov_diag = Some(diag.clone());

                // 推奨候補を承認して再試行 -> stage == 2 で成功＆次月12月へ引き継ぐ
                let rec: HashSet<RelaxationKind> = diag
                    .proposals
                    .iter()
                    .filter(|p| p.recommended)
                    .map(|p| p.kind.clone())
                    .collect();
                assert!(!rec.is_empty());
                match sess.solve_current_month(&rec, |_, _, _, _, _| {}) {
                    MonthStepOutcome::Solved {
                        year: ry,
                        month: rm,
                        result: rres,
                        ..
                    } => {
                        assert_eq!((ry, rm), (2026, 11));
                        assert_eq!(rres.stage, 2);
                        assert!(
                            rres.audit.target_mismatches.is_empty(),
                            "11月も承認緩和後は全コア所定日数一致: {:?}",
                            rres.audit.target_mismatches
                        );
                        solved_map.insert((ry, rm), rres);
                    }
                    other => panic!("11月の推奨緩和再試行が失敗: {:?}", matches!(other, MonthStepOutcome::AllDone)),
                }
            }
            MonthStepOutcome::AllDone => break,
        }
        let _ = (cy, cm);
    }

    // 2026年11月診断原因の内容検証（11/17検査0名、11/18・11/20検査不足、第3週供給不足）
    let nov_diag = paused_nov_diag.expect("2026/11の診断が取得されていること");
    assert!(
        nov_diag.causes.iter().any(|c| c.scope_label.contains("11/17") && c.category.contains("検査")),
        "11/17 検査不足が原因に含まれること: {:?}",
        nov_diag.causes
    );
    assert!(
        nov_diag.causes.iter().any(|c| c.scope_label.contains("11/18")),
        "11/18 が原因に含まれること: {:?}",
        nov_diag.causes
    );
    assert!(
        nov_diag.causes.iter().any(|c| c.scope_label.contains("11/20")),
        "11/20 が原因に含まれること: {:?}",
        nov_diag.causes
    );
    assert!(
        nov_diag.causes.iter().any(|c| c.category.contains("週次総供給不足")),
        "週次総供給不足が原因に含まれること: {:?}",
        nov_diag.causes
    );

    // 2027年3月：小田切(22/22)と長谷川(22/22、火曜5回中4休1出勤、<=5連勤)
    let res_mar = &solved_map[&(2027, 3)];
    assert_eq!(res_mar.target_days["小田切"], 22);
    assert_eq!(
        res_mar.shifts["小田切"].values().map(|&v| v as i32).sum::<i32>(),
        22,
        "2027年3月の小田切さんが22/22達成"
    );
    assert_eq!(res_mar.target_days["長谷川"], 22);
    assert_eq!(
        res_mar.shifts["長谷川"].values().map(|&v| v as i32).sum::<i32>(),
        22,
        "2027年3月の長谷川さんが22/22達成"
    );
    let mut hase_streak = 0;
    for d in 1..=31 {
        if res_mar.shifts["長谷川"][&d] == 1 {
            hase_streak += 1;
            assert!(hase_streak <= 5, "長谷川さんが2027/03で5連勤上限厳守");
        } else {
            hase_streak = 0;
        }
    }

    // Excel出力検証：生成診断レポートシート＆共通休診日(12/29-31, 1/2, 1/4)に⚠️なし
    let _ = fs::remove_file("target/test_v2_0_0_full7m.xlsx");
    let (saved_7m, _) = sess.finalize_excel(|_, _, _, _, _| {}).expect("Excel出力成功");
    let mut wb = open_workbook_auto(&saved_7m).expect("7ヶ月Excel再読込");
    let names = wb.sheet_names().to_vec();
    assert_eq!(
        names.last().map(|s| s.as_str()),
        Some("生成診断レポート"),
        "末尾シートが『生成診断レポート』であること: {:?}",
        names
    );
    for (sheet, days) in [("2026年12月シフト", vec![29u32, 30, 31]), ("2027年01月シフト", vec![2u32, 4])] {
        let range = wb.worksheet_range(sheet).unwrap();
        let rows: Vec<Vec<calamine::Data>> = range.rows().map(|r| r.to_vec()).collect();
        for d in days {
            let warn_cell = rows[(d + 1) as usize][3].to_string();
            assert_eq!(
                warn_cell.trim(),
                "",
                "{sheet} のクリニック全体休診日 {d}日 に ⚠️ が出ないこと (actual: '{warn_cell}')"
            );
        }
    }
    let _ = fs::remove_file(&saved_7m);

    // ------------------------------------------------------------------------
    // 2. バグ修正網羅テスト
    //    (a) 3名ルールで1名除外時に eff_min = 1 (val_min.min(valid_count)) が有効
    //    (b) 4名「同時休み不可」val_min=1 が >= 1（3 ではない）で動作
    //    (c) 未知条件文字列（「月末」「第1金曜」）で tier0_unsolvable=true
    //    (d) PM有休厳格判定（午前半休・半休は非該当）＆全角「週４日」NFKC正規化
    //    (e) 旧形式Excel読み込み互換（属性自動推定・重複ルール11/12自動除外）
    // ------------------------------------------------------------------------
    let mut mini = parsed.clone();
    // 高橋・森川は既にロード時除外なので、さらに宮澤を除外して 3名ルール(小島,長谷川,宮澤 など)を作る代わりに
    // 小型合成データで (a)(b)(c) をピンポイント検証
    let make_staff = |name: &str| Staff {
        name: name.to_string(),
        section: "1階".to_string(),
        role: "スタッフ".to_string(),
        employment: "正社員".to_string(),
        pattern: "週5日".to_string(),
        basic_off_days: vec![],
        basic_off_type: shift_core::parser::OffType::Or,
        is_auto_rotated_off: false,
        is_core: true,
        is_special: false,
        is_early_leave: false,
        is_ope_dedicated: false,
        is_holiday_independent_week4: false,
        has_conditional_wed_off: false,
        vacation_dates: HashSet::new(),
        special_vacation_dates: HashSet::new(),
        preferred_off_dates: HashSet::new(),
        specified_off_dates: HashSet::new(),
        pm_leave_dates: HashSet::new(),
        is_part_1f: false,
        order: 0,
    };

    // (a) 3名中1名除外, val_min=1 -> 残2名のうち最低1名必須（他1名を有休にしても残り1名は出勤すること）
    let mut rule_a_data = parsed.clone();
    rule_a_data.rules.clear();
    let mut s_excl = make_staff("除外者A");
    s_excl.pattern = "除外".to_string();
    let mut s_vac = make_staff("有休者B");
    s_vac.vacation_dates.insert(NaiveDate::from_ymd_opt(2026, 9, 1).unwrap());
    let s_rem = make_staff("出勤者C");
    rule_a_data.staffs = vec![s_excl, s_vac, s_rem];
    rule_a_data.rules.push(shift_core::parser::ShiftRule {
        name: "3名除外テスト".to_string(),
        condition_desc: "火曜".to_string(),
        rule_type: String::new(),
        level: "HARD1".to_string(),
        target_staffs: vec!["除外者A".to_string(), "有休者B".to_string(), "出勤者C".to_string()],
        target_dates_desc: String::new(),
        val_min: 1,
        val_max: 99,
        val_rec: 1,
        is_invalid_condition: false,
    });
    let res_a = solve_shift(&rule_a_data, 2026, 9, &holidays);
    assert!(res_a.success, "3名中1名除外・val_min=1 が解けること");
    assert_eq!(
        res_a.shifts["出勤者C"][&1], 1,
        "バグ修正確認: 除外1名があっても val_min=1 が維持され、有休者B以外の出勤者Cが9/1(火)に必ず出勤すること"
    );

    // (b) 4名「同時休み不可」 val_min=1: 3人が土曜休み希望でも 1名出勤すれば満たされること（旧式の max(1, 4-1)=3 なら破綻する）
    let mut rule_b_data = parsed.clone();
    rule_b_data.rules.clear();
    let mut s1 = make_staff("四名A");
    let mut s2 = make_staff("四名B");
    let mut s3 = make_staff("四名C");
    let s4 = make_staff("四名D");
    let sat_sep5 = NaiveDate::from_ymd_opt(2026, 9, 5).unwrap();
    s1.preferred_off_dates.insert(sat_sep5);
    s2.preferred_off_dates.insert(sat_sep5);
    s3.preferred_off_dates.insert(sat_sep5);
    rule_b_data.staffs = vec![s1, s2, s3, s4];
    rule_b_data.rules.push(shift_core::parser::ShiftRule {
        name: "同時休み不可テスト".to_string(),
        condition_desc: "全日".to_string(),
        rule_type: String::new(),
        level: "HARD1".to_string(),
        target_staffs: vec![
            "四名A".to_string(),
            "四名B".to_string(),
            "四名C".to_string(),
            "四名D".to_string(),
        ],
        target_dates_desc: String::new(),
        val_min: 1,
        val_max: 99,
        val_rec: 1,
        is_invalid_condition: false,
    });
    let res_b = solve_shift(&rule_b_data, 2026, 9, &holidays);
    assert!(
        res_b.success && res_b.shifts["四名D"][&5] == 1,
        "4名同時休み不可(val_min=1)が >=1 で正しく動作すること"
    );

    // (c) 未知条件文字列（「月末」）-> is_invalid_condition -> tier0_unsolvable=true で即停止
    mini.rules[0].is_invalid_condition = true;
    mini.rules[0].condition_desc = "月末".to_string();
    let res_c = solve_shift(&mini, 2026, 9, &holidays);
    assert!(!res_c.success && res_c.diagnosis.as_ref().map(|d| d.tier0_unsolvable).unwrap_or(false));

    // (d) PM有休厳格判定
    let pm_cell = calamine::Data::String("9/12(午前半休), 9/19(半休), 9/26(午後半休)".to_string());
    let pm_res = parse_pm_dates_for_months(&pm_cell, 2026, &[(2026, 9)], Some(&holidays));
    assert_eq!(pm_res.len(), 1);
    assert!(pm_res.contains(&NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()));

    // (e) 旧形式互換Excel（個別勤務ルール列なし + 重複ルール + 全角「週４日」）
    let legacy_xlsx = PathBuf::from("target/test_v2_0_0_legacy.xlsx");
    let _ = fs::remove_file(&legacy_xlsx);
    {
        let mut wx = WxWorkbook::new();
        {
            let ws = wx.add_worksheet();
            ws.set_name("スタッフマスタ").unwrap();
            for (c, h) in [
                "氏名",
                "所属",
                "職種",
                "雇用形態",
                "基本勤務パターン",
                "基本公休曜日",
                "基本公休判定",
                "除外フラグ",
            ]
            .iter()
            .enumerate()
            {
                ws.write_string(0, c as u16, *h).unwrap();
            }
            ws.write_string(1, 0, "長谷川").unwrap();
            ws.write_string(1, 1, "2階裏").unwrap();
            ws.write_string(1, 2, "検査スタッフ").unwrap();
            ws.write_string(1, 3, "正社員").unwrap();
            ws.write_string(1, 4, "週5日").unwrap();
            ws.write_string(1, 5, "火曜").unwrap();
            ws.write_string(1, 6, "AND").unwrap();
            ws.write_string(1, 7, "").unwrap();

            ws.write_string(2, 0, "石橋").unwrap();
            ws.write_string(2, 1, "2階裏").unwrap();
            ws.write_string(2, 2, "看護師").unwrap();
            ws.write_string(2, 3, "パート").unwrap();
            ws.write_string(2, 4, "週４日").unwrap(); // 全角４
            ws.write_string(2, 5, "土曜").unwrap();
            ws.write_string(2, 6, "AND").unwrap();
            ws.write_string(2, 7, "").unwrap();
        }
        {
            let ws = wx.add_worksheet();
            ws.set_name("シフト制約条件").unwrap();
            for (c, h) in ["ルール名", "適用条件", "レベル", "対象スタッフ", "最小人数", "最大人数", "推奨人数"]
                .iter()
                .enumerate()
            {
                ws.write_string(0, c as u16, *h).unwrap();
            }
            // 旧重複ルール（石橋 / 堀・江原の「手術のある日」）-> 自動除外対象
            ws.write_string(1, 0, "Ope日2階裏体制（火曜以外）").unwrap();
            ws.write_string(1, 1, "手術のある日").unwrap();
            ws.write_string(1, 2, "HARD1").unwrap();
            ws.write_string(1, 3, "石橋").unwrap();
            ws.write_number(1, 4, 1.0).unwrap();
            ws.write_number(1, 5, 99.0).unwrap();
            ws.write_number(1, 6, 1.0).unwrap();

            ws.write_string(2, 0, "Ope日看護師体制").unwrap();
            ws.write_string(2, 1, "手術のある日").unwrap();
            ws.write_string(2, 2, "HIGH2").unwrap();
            ws.write_string(2, 3, "堀,江原").unwrap();
            ws.write_number(2, 4, 1.0).unwrap();
            ws.write_number(2, 5, 99.0).unwrap();
            ws.write_number(2, 6, 2.0).unwrap();

            // 未知条件文字列 -> 警告＆エラーフラグ
            ws.write_string(3, 0, "不明条件テスト").unwrap();
            ws.write_string(3, 1, "月末").unwrap();
            ws.write_string(3, 2, "HARD1").unwrap();
            ws.write_string(3, 3, "長谷川").unwrap();
            ws.write_number(3, 4, 1.0).unwrap();
            ws.write_number(3, 5, 99.0).unwrap();
            ws.write_number(3, 6, 1.0).unwrap();
        }
        {
            let ws = wx.add_worksheet();
            ws.set_name("手術スケジュール").unwrap();
            ws.write_string(0, 0, "日付").unwrap();
            ws.write_string(0, 1, "手術種別").unwrap();
            ws.write_string(1, 0, "2026-09-02").unwrap();
            ws.write_string(1, 1, "cat").unwrap();
        }
        wx.save(&legacy_xlsx).unwrap();
    }

    let legacy_parsed = parse_shift_excel(&legacy_xlsx, &holidays).expect("旧Excel読込成功");
    let _ = fs::remove_file(&legacy_xlsx);
    let leg_hase = legacy_parsed.staffs.iter().find(|s| s.name == "長谷川").unwrap();
    assert!(leg_hase.is_early_leave, "旧フォーマットでは氏名フォールバックで長谷川の早上がりが有効");
    let leg_ishi = legacy_parsed.staffs.iter().find(|s| s.name == "石橋").unwrap();
    assert!(leg_ishi.pattern == "週4日", "全角「週４日」が「週4日」へNFKC正規化されること");
    assert!(leg_ishi.is_ope_dedicated && leg_ishi.is_holiday_independent_week4 && leg_ishi.has_conditional_wed_off);
    assert!(!leg_ishi.is_core, "属性付き固定週4スタッフはis_core=false");
    assert!(
        !legacy_parsed.rules.iter().any(|r| r.name.contains("Ope日2階裏体制")),
        "旧重複Opeルールは自動除外されること"
    );
    assert!(
        legacy_parsed.rules.iter().any(|r| r.is_invalid_condition),
        "未知条件ルールは is_invalid_condition=true となること"
    );
}







