//! 勤務シフト自動生成システム 統合回帰テストスイート (`tests/regression_suite.rs`)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 1 回帰テスト実装:
//! - test_01_holidays: 祝日計算・春分秋分・国民の休日・振替休日検証
//! - test_02_date_parser_and_specified_off: 日付パーサー・NFKC・除外フィルタ検証
//! - test_real_excel_loading: 実データ Excel (勤務条件表_9.xlsx) 解析検証

use chrono::NaiveDate;
use shift_core::holidays::{calculate_japanese_holidays, get_holidays};
use shift_core::parser::{excel_serial_to_date, parse_date_list, parse_shift_excel};
use std::path::Path;

/// 1. test_01_holidays: 祝日計算・春分秋分・国民の休日・振替休日の一致検証
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

/// 2. test_02_date_parser_and_specified_off: シリアル値・全角NFKC・範囲指定・除外フィルタ検証
#[test]
fn test_02_date_parser_and_specified_off() {
    let holidays = calculate_japanese_holidays(2024);

    // (1) Excelシリアル値 (45536 = 2024-09-01)
    let d = excel_serial_to_date(45536.0);
    assert_eq!(d, Some(NaiveDate::from_ymd_opt(2024, 9, 1).unwrap()));

    // (2) 全角文字列・NFKC正規化・時刻付き表記のパース
    // "９／１ ０９：００〜９／３ １８：００、９／１０"
    let cell_fullwidth = calamine::Data::String("９／１ ０９：００〜９／３ １８：００、９／１０".to_string());
    let dates = parse_date_list(&cell_fullwidth, 2024, None);
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 1).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 2).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 3).unwrap()));
    assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 10).unwrap()));
    assert_eq!(dates.len(), 4);

    // (3) 日曜日・祝日除外フィルタの検証
    // 2024/9/15 (日), 2024/9/16 (月・敬老の日), 2024/9/17 (火)
    let cell_with_holidays = calamine::Data::String("9/15, 9/16, 9/17".to_string());

    // 有休・特有・指定公休など holidays_jp_dict が Some の場合: 日曜(9/15) と 祝日(9/16) は除外
    let filtered = parse_date_list(&cell_with_holidays, 2024, Some(&holidays));
    assert!(!filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 15).unwrap()));
    assert!(!filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 16).unwrap()));
    assert!(filtered.contains(&NaiveDate::from_ymd_opt(2024, 9, 17).unwrap()));
    assert_eq!(filtered.len(), 1);

    // 希望休など holidays_jp_dict が None の場合: 日曜・祝日も除外されない
    let unfiltered = parse_date_list(&cell_with_holidays, 2024, None);
    assert!(unfiltered.contains(&NaiveDate::from_ymd_opt(2024, 9, 15).unwrap()));
    assert!(unfiltered.contains(&NaiveDate::from_ymd_opt(2024, 9, 16).unwrap()));
    assert!(unfiltered.contains(&NaiveDate::from_ymd_opt(2024, 9, 17).unwrap()));
    assert_eq!(unfiltered.len(), 3);
}

/// 3. test_03_real_excel_loading: 勤務条件表実データ Excel (勤務条件表_9.xlsx) 解析テスト
#[test]
fn test_03_real_excel_loading() {
    let excel_path = if Path::new("勤務条件表_9.xlsx").exists() {
        "勤務条件表_9.xlsx"
    } else if Path::new("勤務条件表_9月.xlsx").exists() {
        "勤務条件表_9月.xlsx"
    } else {
        panic!("実データExcel (勤務条件表_9.xlsx または 勤務条件表_9月.xlsx) が見つかりません。");
    };

    let holidays = calculate_japanese_holidays(2024);
    let parsed = parse_shift_excel(excel_path, &holidays)
        .expect("実データExcelのパースに失敗しました");

    println!("=== 実データ解析結果 ===");
    println!("スタッフ数: {}", parsed.staffs.len());
    println!("制約ルール数: {}", parsed.rules.len());
    println!("手術登録件数: {}", parsed.ope_dict.len());
    println!("対象年月リスト: {:?}", parsed.target_months);
    println!("前月実績 主公休情報件数: {}", parsed.prev_month_main_offs.len());
    println!("前月実績 末尾シフト件数: {}", parsed.prev_month_tail_shifts.len());
    println!("既存生成シフト年月数: {}", parsed.existing_shifts.len());

    // スタッフマスタの検証
    assert!(!parsed.staffs.is_empty(), "スタッフが1名以上読み込まれること");
    for staff in &parsed.staffs {
        assert!(!staff.name.is_empty(), "スタッフ名は空でないこと");
        println!(
            "  [{}] 所属:{} 役職:{} 形態:{} パターン:{} 公休曜:{:?} Type:{:?} 自動ローテ:{} 有休数:{} 特有数:{} 指定公休数:{}",
            staff.name,
            staff.section,
            staff.role,
            staff.employment,
            staff.pattern,
            staff.basic_off_days,
            staff.basic_off_type,
            staff.is_auto_rotated_off,
            staff.vacation_dates.len(),
            staff.special_vacation_dates.len(),
            staff.specified_off_dates.len(),
        );
    }

    // 制約ルールの検証（"小西" を含む行が除外されていること）
    for rule in &parsed.rules {
        assert!(!rule.name.contains("小西"), "小西を含むルールが除外されていること");
        assert!(!rule.condition_desc.contains("小西"), "小西を含む条件が除外されていること");
    }

    // 手術スケジュールの検証
    assert!(!parsed.target_months.is_empty(), "対象年月が1つ以上抽出されていること");
}
