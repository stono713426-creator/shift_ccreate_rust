//! 勤務シフト自動生成システム 祝日計算・取得モジュール (`holidays.rs`)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 1 初期実装: 祝日Web API取得 (reqwest) + 内閣府祝日計算完全フォールバック
//!
//! SPECIFICATION.md セクション3 に基づく祝日判定ロジックの実装。

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration as StdDuration;

/// 内閣府「国民の祝日について」に基づく単一年の祝日計算（完全オフライン対応）
pub fn calculate_japanese_holidays(year: i32) -> HashMap<NaiveDate, String> {
    let mut holidays = BTreeMap::<NaiveDate, String>::new();

    // 1. 固定祝日
    holidays.insert(NaiveDate::from_ymd_opt(year, 1, 1).unwrap(), "元日".to_string());
    holidays.insert(NaiveDate::from_ymd_opt(year, 2, 11).unwrap(), "建国記念の日".to_string());
    if year >= 2020 {
        holidays.insert(NaiveDate::from_ymd_opt(year, 2, 23).unwrap(), "天皇誕生日".to_string());
    } else if (1989..=2018).contains(&year) {
        holidays.insert(NaiveDate::from_ymd_opt(year, 12, 23).unwrap(), "天皇誕生日".to_string());
    }
    holidays.insert(NaiveDate::from_ymd_opt(year, 4, 29).unwrap(), "昭和の日".to_string());
    holidays.insert(NaiveDate::from_ymd_opt(year, 5, 3).unwrap(), "憲法記念日".to_string());
    holidays.insert(NaiveDate::from_ymd_opt(year, 5, 4).unwrap(), "みどりの日".to_string());
    holidays.insert(NaiveDate::from_ymd_opt(year, 5, 5).unwrap(), "こどもの日".to_string());
    if year >= 2016 {
        holidays.insert(NaiveDate::from_ymd_opt(year, 8, 11).unwrap(), "山の日".to_string());
    }
    holidays.insert(NaiveDate::from_ymd_opt(year, 11, 3).unwrap(), "文化の日".to_string());
    holidays.insert(NaiveDate::from_ymd_opt(year, 11, 23).unwrap(), "勤労感謝の日".to_string());

    // 2. ハッピーマンデー（成人の日: 1月第2月曜、海の日: 7月第3月曜、敬老の日: 9月第3月曜、スポーツの日: 10月第2月曜）
    fn nth_monday(year: i32, month: u32, n: u32) -> NaiveDate {
        let first_day = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
        let w = first_day.weekday().num_days_from_monday(); // 0 = Mon, 6 = Sun
        let first_monday_day = 1 + (7 - w) % 7;
        let nth_monday_day = first_monday_day + (n - 1) * 7;
        NaiveDate::from_ymd_opt(year, month, nth_monday_day).unwrap()
    }

    holidays.insert(nth_monday(year, 1, 2), "成人の日".to_string());
    holidays.insert(nth_monday(year, 7, 3), "海の日".to_string());
    holidays.insert(nth_monday(year, 9, 3), "敬老の日".to_string());
    holidays.insert(nth_monday(year, 10, 2), "スポーツの日".to_string());

    // 3. 春分の日・秋分の日 (1980〜2099の簡易計算式)
    let vernal_day = if (1980..=2099).contains(&year) {
        let y_f = (year - 1980) as f64;
        (20.8431 + 0.242194 * y_f - ((year - 1980) / 4) as f64).floor() as u32
    } else {
        20
    };
    if let Some(date) = NaiveDate::from_ymd_opt(year, 3, vernal_day) {
        holidays.insert(date, "春分の日".to_string());
    }

    let autumnal_day = if (1980..=2099).contains(&year) {
        let y_f = (year - 1980) as f64;
        (23.2488 + 0.242194 * y_f - ((year - 1980) / 4) as f64).floor() as u32
    } else {
        23
    };
    if let Some(date) = NaiveDate::from_ymd_opt(year, 9, autumnal_day) {
        holidays.insert(date, "秋分の日".to_string());
    }

    // 4. 国民の休日（祝日と祝日に挟まれた平日）
    let dates: Vec<NaiveDate> = holidays.keys().cloned().collect();
    for window in dates.windows(2) {
        let d1 = window[0];
        let d2 = window[1];
        if (d2 - d1).num_days() == 2 {
            let mid = d1 + Duration::days(1);
            if mid.weekday() != Weekday::Sun && !holidays.contains_key(&mid) {
                holidays.insert(mid, "国民の休日".to_string());
            }
        }
    }

    // 5. 振替休日（祝日が日曜日の場合、翌日以降で祝日・振替休日でない最初の平日）
    let mut substitute_holidays = Vec::new();
    let current_holidays: Vec<NaiveDate> = holidays.keys().cloned().collect();
    for h_date in current_holidays {
        if h_date.weekday() == Weekday::Sun {
            let mut candidate = h_date + Duration::days(1);
            while holidays.contains_key(&candidate) || substitute_holidays.contains(&candidate) {
                candidate += Duration::days(1);
            }
            substitute_holidays.push(candidate);
        }
    }

    for sub in substitute_holidays {
        holidays.insert(sub, "振替休日".to_string());
    }

    holidays.into_iter().collect()
}

/// 指定した年範囲の祝日を計算
pub fn calculate_japanese_holidays_range(start_year: i32, end_year: i32) -> HashMap<NaiveDate, String> {
    let mut all_holidays = HashMap::new();
    for y in start_year..=end_year {
        all_holidays.extend(calculate_japanese_holidays(y));
    }
    all_holidays
}

/// 祝日Web API (`https://holidays-jp.github.io/api/v1/date.json`) から取得を試行。
/// タイムアウト3秒またはネットワークエラー、サンドボックス制限時は内閣府オフライン計算にフォールバック。
pub fn get_holidays(start_year: i32, end_year: i32) -> HashMap<NaiveDate, String> {
    const API_URL: &str = "https://holidays-jp.github.io/api/v1/date.json";

    let api_result: Result<HashMap<String, String>, reqwest::Error> = (|| {
        let client = reqwest::blocking::Client::builder()
            .timeout(StdDuration::from_secs(3))
            .build()?;
        let res = client.get(API_URL).send()?;
        res.json::<HashMap<String, String>>()
    })();

    match api_result {
        Ok(json_map) => {
            let mut result = HashMap::new();
            for (date_str, name) in json_map {
                if let Ok(d) = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d") {
                    if d.year() >= start_year && d.year() <= end_year {
                        result.insert(d, name);
                    }
                }
            }
            // API取得データが空でなければ採用
            if !result.is_empty() {
                return result;
            }
            calculate_japanese_holidays_range(start_year, end_year)
        }
        Err(_) => {
            // オフラインまたはエラー時は内閣府計算フォールバック
            calculate_japanese_holidays_range(start_year, end_year)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_holidays_2024() {
        let h2024 = calculate_japanese_holidays(2024);

        // 元日
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()).unwrap(), "元日");
        // 成人の日 (第2月曜: 1/8)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 1, 8).unwrap()).unwrap(), "成人の日");
        // 建国記念の日 (2/11が日曜 -> 2/12 振替休日)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 2, 11).unwrap()).unwrap(), "建国記念の日");
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 2, 12).unwrap()).unwrap(), "振替休日");
        // 春分の日 (2024/3/20)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 3, 20).unwrap()).unwrap(), "春分の日");
        // こどもの日 (5/5が日曜 -> 5/6 振替休日)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 5, 5).unwrap()).unwrap(), "こどもの日");
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 5, 6).unwrap()).unwrap(), "振替休日");
        // 敬老の日 (9/16)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 9, 16).unwrap()).unwrap(), "敬老の日");
        // 秋分の日 (9/22が日曜 -> 9/23 振替休日)
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 9, 22).unwrap()).unwrap(), "秋分の日");
        assert_eq!(h2024.get(&NaiveDate::from_ymd_opt(2024, 9, 23).unwrap()).unwrap(), "振替休日");
    }

    #[test]
    fn test_silver_week_national_holiday() {
        // 2026年: 9/21(敬老の日), 9/23(秋分の日) -> 9/22(国民の休日)
        let h2026 = calculate_japanese_holidays(2026);
        assert_eq!(h2026.get(&NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()).unwrap(), "敬老の日");
        assert_eq!(h2026.get(&NaiveDate::from_ymd_opt(2026, 9, 22).unwrap()).unwrap(), "国民の休日");
        assert_eq!(h2026.get(&NaiveDate::from_ymd_opt(2026, 9, 23).unwrap()).unwrap(), "秋分の日");
    }
}
