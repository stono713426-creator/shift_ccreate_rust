//! 勤務シフト自動生成システム Excelデータ解析・日付パーサー (`parser.rs`)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 1 初期実装: NFKC正規化日付パース、全シート（マスタ・制約・OPE・前月実績・設定・既存シフト）読込
//!
//! SPECIFICATION.md セクション4 に基づく完全移植パーサー。

use crate::config::ShiftConfig;
use calamine::{open_workbook_auto, Data, Range, Reader};
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

/// 基本公休タイプ (AND: 全て休み, OR: いずれか1日が休み)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OffType {
    And,
    Or,
}

/// スタッフマスタ情報
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Staff {
    pub name: String,
    pub section: String,
    pub role: String,
    pub employment: String,
    pub pattern: String,
    /// 0: 月, 1: 火, 2: 水, 3: 木, 4: 金, 5: 土, 6: 日
    pub basic_off_days: Vec<u32>,
    pub basic_off_type: OffType,
    pub is_auto_rotated_off: bool,
    pub is_special: bool,
    pub vacation_dates: HashSet<NaiveDate>,
    pub special_vacation_dates: HashSet<NaiveDate>,
    pub preferred_off_dates: HashSet<NaiveDate>,
    pub specified_off_dates: HashSet<NaiveDate>,
    pub is_core: bool,
    pub is_part_1f: bool,
    pub order: usize,
}

/// シフト制約条件
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShiftRule {
    pub name: String,
    pub condition_desc: String,
    pub rule_type: String,
    pub target_staffs: Vec<String>,
    pub target_dates_desc: String,
    pub val_min: i32,
    pub val_rec: i32,
    pub val_max: i32,
    pub level: String,
}

/// Excel 全シート解析結果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedExcelData {
    pub staffs: Vec<Staff>,
    pub rules: Vec<ShiftRule>,
    pub ope_dict: HashMap<NaiveDate, String>,
    pub target_months: Vec<(i32, u32)>,
    pub marked_months: Vec<(i32, u32)>,
    pub config: ShiftConfig,
    pub prev_month_main_offs: HashMap<String, u32>,
    pub prev_month_tail_shifts: HashMap<String, Vec<u8>>,
    pub existing_shifts: HashMap<(i32, u32), HashMap<String, HashMap<u32, u8>>>,
}

// ============================================================================
// 4.1 堅牢な日付リストパーサー (parse_date_list)
// ============================================================================

/// 任意のセル値から日付リストをパースする。
/// `holidays_jp_dict` が `Some` の場合は日曜日および祝日を除外する（有休・特別有休・指定公休）。
/// `None` の場合は除外しない（希望休）。
pub fn parse_date_list(
    cell: &Data,
    default_year: i32,
    holidays_jp_dict: Option<&HashMap<NaiveDate, String>>,
) -> HashSet<NaiveDate> {
    let mut dates = HashSet::new();

    match cell {
        Data::Empty => return dates,
        Data::DateTime(dt) => {
            let (y, m, d, _, _, _, _) = dt.to_ymd_hms_milli();
            if let Some(date) = NaiveDate::from_ymd_opt(y as i32, m as u32, d as u32) {
                dates.insert(date);
            }
        }
        Data::DateTimeIso(s) => {
            if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                dates.insert(d);
            } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                dates.insert(dt.date());
            }
        }
        Data::Int(i) => {
            let val = *i as f64;
            if (30000.0..=60000.0).contains(&val) {
                if let Some(d) = excel_serial_to_date(val) {
                    dates.insert(d);
                }
            }
        }
        Data::Float(f) => {
            if (30000.0..=60000.0).contains(f) {
                if let Some(d) = excel_serial_to_date(*f) {
                    dates.insert(d);
                }
            }
        }
        Data::String(raw_s) => {
            // 4. Unicode NFKC 正規化
            let normalized: String = raw_s.nfkc().collect();
            let trimmed = normalized.trim();
            if trimmed.is_empty() {
                return dates;
            }

            // シリアル値文字列判定 (例: "45536" や "45536.0")
            let re_serial = Regex::new(r"^\d{5}(\.0+)?$").unwrap();
            if re_serial.is_match(trimmed) {
                if let Ok(num) = trimmed.parse::<f64>() {
                    if (30000.0..=60000.0).contains(&num) {
                        if let Some(d) = excel_serial_to_date(num) {
                            dates.insert(d);
                            return filter_holidays_sundays(dates, holidays_jp_dict);
                        }
                    }
                }
            }

            // 5. 時刻表記除去 (\b\d{1,2}:\d{2}(:\d{2})?\b)
            let re_time = Regex::new(r"\b\d{1,2}:\d{2}(:\d{2})?\b").unwrap();
            let cleaned = re_time.replace_all(trimmed, "");

            // 記号 [,、;・\n\r]+ でトークン分割
            let re_split = Regex::new(r"[,、;・\n\r]+").unwrap();
            let raw_tokens: Vec<&str> = re_split.split(&cleaned).map(|s| s.trim()).filter(|s| !s.is_empty()).collect();

            // 範囲記号 ([-〜~]) を含まない空白区切りトークンは空白で再分割
            let mut tokens = Vec::new();
            for tok in raw_tokens {
                if tok.contains('〜') || tok.contains('~') || tok.contains(" - ") {
                    tokens.push(tok.to_string());
                } else {
                    for part in tok.split_whitespace() {
                        if !part.is_empty() {
                            tokens.push(part.to_string());
                        }
                    }
                }
            }

            // 各トークンの日付・範囲パース
            for tok in tokens {
                if tok.contains('〜') || tok.contains('~') || tok.contains(" - ") {
                    // 範囲パース
                    let delimiter = if tok.contains('〜') {
                        '〜'
                    } else if tok.contains('~') {
                        '~'
                    } else {
                        '-'
                    };
                    let parts: Vec<&str> = if delimiter == '-' {
                        tok.split(" - ").collect()
                    } else {
                        tok.split(delimiter).collect()
                    };

                    if parts.len() == 2 {
                        let d1 = parse_single_date(parts[0], default_year);
                        let d2 = parse_single_date(parts[1], default_year);
                        if let (Some(mut start), Some(mut end)) = (d1, d2) {
                            if start > end {
                                std::mem::swap(&mut start, &mut end);
                            }
                            let mut curr = start;
                            while curr <= end {
                                dates.insert(curr);
                                curr += Duration::days(1);
                            }
                        }
                    }
                } else if let Some(d) = parse_single_date(&tok, default_year) {
                    dates.insert(d);
                }
            }
        }
        _ => {}
    }

    filter_holidays_sundays(dates, holidays_jp_dict)
}

/// 単一の日付文字列をパース
fn parse_single_date(s: &str, default_year: i32) -> Option<NaiveDate> {
    let cleaned = s.trim().replace('年', "/").replace('月', "/").replace('.', "/").replace('日', "");
    let parts: Vec<&str> = cleaned.split(['/', '-']).map(|p| p.trim()).filter(|p| !p.is_empty()).collect();

    match parts.len() {
        2 => {
            // M/D -> default_year 補完
            let m = parts[0].parse::<u32>().ok()?;
            let d = parts[1].parse::<u32>().ok()?;
            NaiveDate::from_ymd_opt(default_year, m, d)
        }
        3 => {
            // YYYY/M/D
            let mut y = parts[0].parse::<i32>().ok()?;
            if y < 100 {
                // 2桁年 (24 -> 2024)
                y += 2000;
            }
            let m = parts[1].parse::<u32>().ok()?;
            let d = parts[2].parse::<u32>().ok()?;
            NaiveDate::from_ymd_opt(y, m, d)
        }
        1 => {
            // 単一数字の日付指定 (例: "5" -> default_year, default_month は不明のためスキップするか、後続で補完)
            None
        }
        _ => None,
    }
}

/// Excel シリアル値（1899-12-30 基準）から NaiveDate を計算
pub fn excel_serial_to_date(serial: f64) -> Option<NaiveDate> {
    let days = serial.floor() as i64;
    let base = NaiveDate::from_ymd_opt(1899, 12, 30)?;
    base.checked_add_signed(Duration::days(days))
}

/// `holidays_jp_dict` が `Some` の場合、日曜および祝日を除外
fn filter_holidays_sundays(
    dates: HashSet<NaiveDate>,
    holidays_jp_dict: Option<&HashMap<NaiveDate, String>>,
) -> HashSet<NaiveDate> {
    if let Some(holidays) = holidays_jp_dict {
        dates
            .into_iter()
            .filter(|d| d.weekday() != Weekday::Sun && !holidays.contains_key(d))
            .collect()
    } else {
        dates
    }
}

// ============================================================================
// 4.2 各シートの読み込み
// ============================================================================

/// Excel 全シートを読み込み、スタッフ、制約ルール、手術、設定、前月実績などを抽出
pub fn parse_shift_excel<P: AsRef<Path>>(
    excel_path: P,
    holidays: &HashMap<NaiveDate, String>,
) -> Result<ParsedExcelData, String> {
    let path = excel_path.as_ref();
    let mut workbook = open_workbook_auto(path).map_err(|e| format!("Excelファイルオープンエラー: {e}"))?;

    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err("Excel内にシートが存在しません。".to_string());
    }

    // 1. 設定シート (config) の探索とパース
    let mut config = ShiftConfig::default();
    for s_name in &sheet_names {
        let lower = s_name.to_lowercase();
        if lower.contains("設定") || lower.contains("パラメータ") || lower.contains("config") {
            if let Ok(range) = workbook.worksheet_range(s_name) {
                parse_config_sheet(&range, &mut config);
            }
        }
    }

    // 2. 手術スケジュールシートの探索とパース
    let mut ope_dict = HashMap::new();
    let mut target_months_set = HashSet::new();
    let mut marked_months_set = HashSet::new();

    for s_name in &sheet_names {
        let lower = s_name.to_lowercase();
        if lower.contains("手術") || lower.contains("オペ") || lower.contains("ope") {
            if let Ok(range) = workbook.worksheet_range(s_name) {
                parse_ope_sheet(&range, &mut ope_dict, &mut target_months_set, &mut marked_months_set);
            }
        }
    }

    // 対象月リストのソート
    let mut target_months: Vec<(i32, u32)> = target_months_set.into_iter().collect();
    target_months.sort_unstable();

    let mut marked_months: Vec<(i32, u32)> = marked_months_set.into_iter().collect();
    marked_months.sort_unstable();

    // デフォルト年（対象月の先頭、なければ現在の年または2024）
    let default_year = target_months.first().map(|(y, _)| *y).unwrap_or(2024);

    // 3. スタッフマスタシートの探索とパース
    // 優先順: 「スタッフ」/「マスタ」/「勤務条件」を含むシート、なければ0番目
    let staff_sheet_name = sheet_names
        .iter()
        .find(|s| s.contains("スタッフ") || s.contains("マスタ") || s.contains("条件表"))
        .unwrap_or(&sheet_names[0]);

    let staffs = if let Ok(range) = workbook.worksheet_range(staff_sheet_name) {
        parse_staff_sheet(&range, default_year, holidays, &config)?
    } else {
        return Err(format!("スタッフマスタシート '{staff_sheet_name}' を読み込めません。"));
    };

    // 4. シフト制約条件シートの探索とパース
    let mut rules = Vec::new();
    for s_name in &sheet_names {
        if s_name.contains("制約") || s_name.contains("ルール") || s_name.contains("条件") {
            if s_name == staff_sheet_name {
                continue;
            }
            if let Ok(range) = workbook.worksheet_range(s_name) {
                parse_rules_sheet(&range, &mut rules);
            }
        }
    }
    // もし制約シートが見つからず、シート数が2以上ならインデックス1を試す
    if rules.is_empty() && sheet_names.len() >= 2 && &sheet_names[1] != staff_sheet_name {
        if let Ok(range) = workbook.worksheet_range(&sheet_names[1]) {
            parse_rules_sheet(&range, &mut rules);
        }
    }

    // 5. 前月実績シートの探索とパース
    let mut prev_month_main_offs = HashMap::new();
    let mut prev_month_tail_shifts = HashMap::new();
    for s_name in &sheet_names {
        if s_name.contains("前月") || s_name.contains("実績") {
            if let Ok(range) = workbook.worksheet_range(s_name) {
                parse_prev_month_sheet(&range, &mut prev_month_main_offs, &mut prev_month_tail_shifts);
            }
        }
    }

    // 6. 既存生成シフトシートの探索 (例: "2024年09月シフト", "24年09月シフト")
    let mut existing_shifts = HashMap::new();
    let re_shift_sheet = Regex::new(r"(\d{2,4})年(\d{1,2})月(?:シフト)?").unwrap();
    for s_name in &sheet_names {
        if let Some(caps) = re_shift_sheet.captures(s_name) {
            let mut y = caps[1].parse::<i32>().unwrap_or(2024);
            if y < 100 {
                y += 2000;
            }
            let m = caps[2].parse::<u32>().unwrap_or(9);
            if let Ok(range) = workbook.worksheet_range(s_name) {
                if let Some(shifts) = parse_existing_shift_sheet(&range) {
                    existing_shifts.insert((y, m), shifts);
                }
            }
        }
    }

    Ok(ParsedExcelData {
        staffs,
        rules,
        ope_dict,
        target_months,
        marked_months,
        config,
        prev_month_main_offs,
        prev_month_tail_shifts,
        existing_shifts,
    })
}

// ============================================================================
// 各シートの個別パーサー実装
// ============================================================================

/// 設定シートのパース
fn parse_config_sheet(range: &Range<Data>, config: &mut ShiftConfig) {
    for row in range.rows() {
        if row.len() >= 2 {
            let key = row[0].to_string().trim().to_string();
            let val = row[1].to_string().trim().to_string();
            if !key.is_empty() && !val.is_empty() {
                config.update_from_key_value(&key, &val);
            }
        }
    }
}

/// 手術スケジュールシートのパース
fn parse_ope_sheet(
    range: &Range<Data>,
    ope_dict: &mut HashMap<NaiveDate, String>,
    target_months: &mut HashSet<(i32, u32)>,
    marked_months: &mut HashSet<(i32, u32)>,
) {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return;
    }

    // ヘッダー行探索 (日付, 種別, 作成対象)
    let mut col_date = None;
    let mut col_type = None;
    let mut col_target = None;
    let mut header_row_idx = 0;

    for (r_idx, row) in rows.iter().enumerate().take(5) {
        for (c_idx, cell) in row.iter().enumerate() {
            let s = cell.to_string();
            if s.contains("日付") || s.contains("日") || s.to_lowercase().contains("date") {
                col_date = Some(c_idx);
            }
            if s.contains("種別") || s.contains("内容") || s.contains("手術") || s.contains("OPE") || s.contains("オペ") {
                col_type = Some(c_idx);
            }
            if s.contains("作成") || s.contains("対象") || s.contains("出力") || s.contains("生成") {
                col_target = Some(c_idx);
            }
        }
        if col_date.is_some() {
            header_row_idx = r_idx;
            break;
        }
    }

    let c_date = match col_date {
        Some(c) => c,
        None => 0,
    };
    let c_type = col_type.unwrap_or(1);

    for row in rows.iter().skip(header_row_idx + 1) {
        if row.len() <= c_date {
            continue;
        }
        let date_cell = &row[c_date];
        let type_cell = if row.len() > c_type { &row[c_type] } else { &Data::Empty };

        let parsed_dates = parse_date_list(date_cell, 2024, None);
        let type_str = type_cell.to_string().trim().to_string();

        let is_marked = if let Some(ct) = col_target {
            if row.len() > ct {
                let val_str = row[ct].to_string().trim().to_string();
                val_str == "○" || val_str == "◯" || val_str == "1" || val_str.to_lowercase() == "true"
            } else {
                false
            }
        } else {
            false
        };

        for d in parsed_dates {
            target_months.insert((d.year(), d.month()));
            if is_marked {
                marked_months.insert((d.year(), d.month()));
            }
            if !type_str.is_empty() {
                ope_dict.insert(d, type_str.clone());
            }
        }
    }
}

/// スタッフマスタシートのパース
fn parse_staff_sheet(
    range: &Range<Data>,
    default_year: i32,
    holidays: &HashMap<NaiveDate, String>,
    config: &ShiftConfig,
) -> Result<Vec<Staff>, String> {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return Err("スタッフシートが空です。".to_string());
    }

    // ヘッダー行探索 (氏名, 所属, 雇用形態, 基本公休, 有休, 特別有休, 希望休, 指定公休, 除外)
    let mut col_name = None;
    let mut col_section = None;
    let mut col_role = None;
    let mut col_employment = None;
    let mut col_pattern = None;
    let mut col_basic_off = None;
    let mut col_vacation = None;
    let mut col_special_vacation = None;
    let mut col_preferred_off = None;
    let mut col_specified_off = None;
    let mut col_exclude = None;
    let mut header_row_idx = 0;

    for (r_idx, row) in rows.iter().enumerate().take(5) {
        for (c_idx, cell) in row.iter().enumerate() {
            let s = cell.to_string().trim().to_string();
            if s.contains("除外") {
                col_exclude = Some(c_idx);
            } else if s.contains("氏名") || s.contains("名前") || s.contains("スタッフ") {
                col_name = Some(c_idx);
            } else if s.contains("所属") || s.contains("セクション") || s.contains("フロア") || s.contains("部署") {
                col_section = Some(c_idx);
            } else if s.contains("役職") || s.contains("役割") || s.contains("資格") || s.contains("職種") {
                col_role = Some(c_idx);
            } else if s.contains("雇用形態") || s.contains("形態") {
                col_employment = Some(c_idx);
            } else if s.contains("勤務パターン") || s.contains("パターン") || (s.contains("週") && !s.contains("休")) {
                col_pattern = Some(c_idx);
            } else if s.contains("希望公休曜日") || s.contains("基本公休") || s.contains("公休曜日") {
                col_basic_off = Some(c_idx);
            } else if s.contains("特別") {
                col_special_vacation = Some(c_idx);
            } else if s.contains("有休") {
                col_vacation = Some(c_idx);
            } else if s.contains("希望休") {
                col_preferred_off = Some(c_idx);
            } else if s.contains("指定公休") || s.contains("公休指定") {
                col_specified_off = Some(c_idx);
            }
        }
        if col_name.is_some() || col_section.is_some() {
            header_row_idx = r_idx;
            break;
        }
    }

    let c_name = col_name.unwrap_or(0);
    let mut staffs = Vec::new();

    for (r_idx, row) in rows.iter().enumerate().skip(header_row_idx + 1) {
        if row.len() <= c_name {
            continue;
        }

        let name = row[c_name].to_string().trim().to_string();
        if name.is_empty() || name.contains("合計") || name.contains("人数") {
            continue;
        }

        // 除外列判定
        if let Some(c_ex) = col_exclude {
            if row.len() > c_ex {
                let ex_val = row[c_ex].to_string().trim().to_string();
                if ex_val == "1" || ex_val == "true" || ex_val == "○" || ex_val == "◯" {
                    continue;
                }
            }
        }

        let section = col_section.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let role = col_role.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let employment = col_employment.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let pattern = col_pattern.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        let basic_off_str = col_basic_off.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        // 曜日抽出: 月火水木金土日 -> 0..=6
        let mut basic_off_days = Vec::new();
        for ch in basic_off_str.chars() {
            let day_num = match ch {
                '月' => Some(0),
                '火' => Some(1),
                '水' => Some(2),
                '木' => Some(3),
                '金' => Some(4),
                '土' => Some(5),
                '日' => Some(6),
                _ => None,
            };
            if let Some(w) = day_num {
                if !basic_off_days.contains(&w) {
                    basic_off_days.push(w);
                }
            }
        }

        // basic_off_type
        let basic_off_type = if basic_off_str.contains("もしくは") || basic_off_str.contains("あるいは") {
            OffType::Or
        } else if !basic_off_days.is_empty() {
            OffType::And
        } else {
            OffType::Or
        };

        // 有休（日・祝除外）
        let vacation_dates = col_vacation
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list(d, default_year, Some(holidays)))
            .unwrap_or_default();

        // 特別有休（日・祝除外）
        let special_vacation_dates = col_special_vacation
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list(d, default_year, Some(holidays)))
            .unwrap_or_default();

        // 希望休（日・祝除外しない）
        let preferred_off_dates = col_preferred_off
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list(d, default_year, None))
            .unwrap_or_default();

        // 指定公休（日・祝除外）
        let specified_off_dates = col_specified_off
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list(d, default_year, Some(holidays)))
            .unwrap_or_default();

        let is_core = employment == "正社員" || employment == "パート社員";
        let is_part_1f = section == "1階" && employment == "パート";
        let is_special = pattern.contains("必要に応じて週5") || pattern.contains("必要に応じて");

        let mut staff = Staff {
            name,
            section,
            role,
            employment,
            pattern,
            basic_off_days,
            basic_off_type,
            is_auto_rotated_off: false,
            is_special,
            vacation_dates,
            special_vacation_dates,
            preferred_off_dates,
            specified_off_dates,
            is_core,
            is_part_1f,
            order: r_idx,
        };

        // 空欄スタッフ自動ローテーション初期化
        if config.auto_rotate_off_rule
            && staff.is_core
            && staff.pattern != "除外"
            && !staff.is_part_1f
            && staff.basic_off_days.is_empty()
        {
            staff.basic_off_days = vec![0, 1, 2, 3, 4]; // 月〜金
            staff.basic_off_type = OffType::Or;
            staff.is_auto_rotated_off = true;
        }

        staffs.push(staff);
    }

    Ok(staffs)
}

/// シフト制約条件シートのパース
fn parse_rules_sheet(range: &Range<Data>, rules: &mut Vec<ShiftRule>) {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return;
    }

    let mut col_name = None;
    let mut col_type = None;
    let mut col_staffs = None;
    let mut col_dates = None;
    let mut col_min = None;
    let mut col_rec = None;
    let mut col_max = None;
    let mut col_level = None;
    let mut header_row_idx = 0;

    for (r_idx, row) in rows.iter().enumerate().take(5) {
        for (c_idx, cell) in row.iter().enumerate() {
            let s = cell.to_string().trim().to_string();
            if s.contains("ルール") || s.contains("名称") || s.contains("条件") {
                col_name = Some(c_idx);
            } else if s.contains("種別") || s.contains("タイプ") || s.contains("区分") {
                col_type = Some(c_idx);
            } else if s.contains("対象") || s.contains("スタッフ") {
                col_staffs = Some(c_idx);
            } else if s.contains("適用日") || s.contains("日付") || s.contains("曜日") {
                col_dates = Some(c_idx);
            } else if s.contains("最低") || s.contains("最小") || s.to_lowercase().contains("min") {
                col_min = Some(c_idx);
            } else if s.contains("推奨") || s.to_lowercase().contains("rec") {
                col_rec = Some(c_idx);
            } else if s.contains("最大") || s.to_lowercase().contains("max") {
                col_max = Some(c_idx);
            } else if s.contains("レベル") || s.contains("重要度") || s.contains("優先度") {
                col_level = Some(c_idx);
            }
        }
        if col_name.is_some() && (col_min.is_some() || col_level.is_some()) {
            header_row_idx = r_idx;
            break;
        }
    }

    let c_name = col_name.unwrap_or(0);

    for row in rows.iter().skip(header_row_idx + 1) {
        if row.len() <= c_name {
            continue;
        }

        let name = row[c_name].to_string().trim().to_string();
        if name.is_empty() {
            continue;
        }

        // 重要: 条件列に "小西" を含む行は外部発生イベントとして除外
        let condition_desc = col_dates.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        if name.contains("小西") || condition_desc.contains("小西") {
            continue;
        }

        let rule_type = col_type.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        let staffs_str = col_staffs.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let target_staffs: Vec<String> = staffs_str
            .split([',', '、', ';', '・', '/', ' '])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let val_min = col_min.and_then(|c| row.get(c)).and_then(|d| d.to_string().trim().parse::<i32>().ok()).unwrap_or(0);
        let val_rec = col_rec.and_then(|c| row.get(c)).and_then(|d| d.to_string().trim().parse::<i32>().ok()).unwrap_or(0);
        let val_max = col_max.and_then(|c| row.get(c)).and_then(|d| d.to_string().trim().parse::<i32>().ok()).unwrap_or(99);
        let level = col_level.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_else(|| "SOFT".to_string());

        rules.push(ShiftRule {
            name,
            condition_desc: condition_desc.clone(),
            rule_type,
            target_staffs,
            target_dates_desc: condition_desc,
            val_min,
            val_rec,
            val_max,
            level,
        });
    }
}

/// 前月実績シートのパース（縦型フォーマット・横型フォーマット両対応）
fn parse_prev_month_sheet(
    range: &Range<Data>,
    main_offs: &mut HashMap<String, u32>,
    tail_shifts: &mut HashMap<String, Vec<u8>>,
) {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return;
    }

    // 縦型フォーマット判定: 先頭5行以内に「日付」と「曜日」を含む
    let mut is_vertical = false;
    let mut header_row_idx = 0;
    for (r_idx, row) in rows.iter().enumerate().take(5) {
        let row_str = row.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" ");
        if row_str.contains("日付") && row_str.contains("曜日") {
            is_vertical = true;
            header_row_idx = r_idx;
            break;
        }
    }

    if is_vertical {
        // 縦型: 列4以降がスタッフ名
        let header_row = rows[header_row_idx];
        let mut staff_cols = Vec::new();

        for (c_idx, cell) in header_row.iter().enumerate().skip(4) {
            let s = cell.to_string().trim().to_string();
            if !s.is_empty()
                && !s.contains("人数")
                && !s.contains("総出勤")
                && !s.contains("合計")
                && !s.contains("所定")
                && !s.contains("公休")
                && !s.contains("曜日")
            {
                staff_cols.push((c_idx, s));
            }
        }

        // 備考列 (列2) から主公休曜日の抽出
        // 日付データ行の抽出
        let mut date_rows = Vec::new();
        for row in rows.iter().skip(header_row_idx + 1) {
            if row.len() <= 2 {
                continue;
            }
            let desc_col = row.get(2).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
            if desc_col.contains("主公休") {
                // 主公休行
                for &(c_idx, ref staff_name) in &staff_cols {
                    if let Some(cell) = row.get(c_idx) {
                        let text = cell.to_string();
                        let mut w_chars = Vec::new();
                        for ch in text.chars() {
                            match ch {
                                '月' => w_chars.push(0),
                                '火' => w_chars.push(1),
                                '水' => w_chars.push(2),
                                '木' => w_chars.push(3),
                                '金' => w_chars.push(4),
                                '土' => w_chars.push(5),
                                '日' => w_chars.push(6),
                                _ => {}
                            }
                        }
                        // 複数文字（例: "火・水"）がある場合は最後の曜日文字を採用
                        if let Some(&last_w) = w_chars.last() {
                            main_offs.insert(staff_name.clone(), last_w);
                        }
                    }
                }
            }

            // 日付セル判定 (列0が1〜31の数値または日付)
            if let Some(c0) = row.first() {
                let s = c0.to_string().trim().to_string();
                if let Ok(d_num) = s.parse::<u32>() {
                    if (1..=31).contains(&d_num) {
                        date_rows.push(row);
                    }
                }
            }
        }

        // 末尾7行分のシフト実績
        let tail_count = 7.min(date_rows.len());
        let tail_rows = &date_rows[date_rows.len() - tail_count..];

        for &(c_idx, ref staff_name) in &staff_cols {
            let mut shifts = Vec::new();
            for r in tail_rows {
                let val_str = r.get(c_idx).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
                if val_str == "○" || val_str == "◯" || val_str == "1" {
                    shifts.push(1);
                } else {
                    shifts.push(0);
                }
            }
            tail_shifts.insert(staff_name.clone(), shifts);
        }
    } else {
        // 横型フォーマット: 1列目が氏名、列ヘッダーが日付
        let header = rows[0];
        let mut date_cols = Vec::new();
        for (c_idx, cell) in header.iter().enumerate().skip(1) {
            let s = cell.to_string().trim().to_string();
            if let Ok(day) = s.parse::<u32>() {
                if (1..=31).contains(&day) {
                    date_cols.push(c_idx);
                }
            }
        }

        let tail_cols = if date_cols.len() > 7 {
            &date_cols[date_cols.len() - 7..]
        } else {
            &date_cols[..]
        };

        for row in rows.iter().skip(1) {
            if row.is_empty() {
                continue;
            }
            let name = row[0].to_string().trim().to_string();
            if name.is_empty() || name.contains("合計") {
                continue;
            }
            let mut shifts = Vec::new();
            for &c_idx in tail_cols {
                let val_str = row.get(c_idx).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
                if val_str == "○" || val_str == "◯" || val_str == "1" {
                    shifts.push(1);
                } else {
                    shifts.push(0);
                }
            }
            tail_shifts.insert(name, shifts);
        }
    }
}

/// 既存生成シフトシートのパース (スタッフ名 -> 日付 -> 出勤フラグ 1/0)
fn parse_existing_shift_sheet(range: &Range<Data>) -> Option<HashMap<String, HashMap<u32, u8>>> {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.len() < 3 {
        return None;
    }

    // 縦型レイアウト前提 (ヘッダー行にスタッフ名、行に日付)
    let header_row = rows[1]; // 通常行2がヘッダー
    let mut staff_cols = Vec::new();
    for (c_idx, cell) in header_row.iter().enumerate().skip(4) {
        let s = cell.to_string().trim().to_string();
        if !s.is_empty()
            && !s.contains("人数")
            && !s.contains("総出勤")
            && !s.contains("合計")
            && !s.contains("所定")
            && !s.contains("公休")
            && !s.contains("曜日")
        {
            staff_cols.push((c_idx, s));
        }
    }

    if staff_cols.is_empty() {
        return None;
    }

    let mut result: HashMap<String, HashMap<u32, u8>> = HashMap::new();
    for &(c_idx, ref staff_name) in &staff_cols {
        let mut day_map = HashMap::new();
        for row in rows.iter().skip(2) {
            if let Some(c0) = row.first() {
                let s = c0.to_string().trim().to_string();
                if let Ok(d_num) = s.parse::<u32>() {
                    if (1..=31).contains(&d_num) {
                        let val_str = row.get(c_idx).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
                        let flag = if val_str == "○" || val_str == "◯" || val_str == "1" { 1 } else { 0 };
                        day_map.insert(d_num, flag);
                    }
                }
            }
        }
        result.insert(staff_name.clone(), day_map);
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_excel_serial_to_date() {
        // 45536 = 2024-09-01
        let d = excel_serial_to_date(45536.0).unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2024, 9, 1).unwrap());
    }

    #[test]
    fn test_parse_date_list_simple_and_range() {
        let cell = Data::String("9/1〜9/3, 9/5, 9/7".to_string());
        let dates = parse_date_list(&cell, 2024, None);
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 1).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 2).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 3).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 5).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 7).unwrap()));
        assert_eq!(dates.len(), 5);
    }

    #[test]
    fn test_parse_date_list_with_holidays_and_sundays_filter() {
        let mut holidays = HashMap::new();
        // 2024/9/16: 敬老の日
        holidays.insert(NaiveDate::from_ymd_opt(2024, 9, 16).unwrap(), "敬老の日".to_string());

        // 2024/9/15 は日曜日、9/16 は祝日、9/17 は火曜日
        let cell = Data::String("9/15, 9/16, 9/17".to_string());
        let dates = parse_date_list(&cell, 2024, Some(&holidays));

        // 9/15(日) と 9/16(祝) は除外されるべき
        assert!(!dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 15).unwrap()));
        assert!(!dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 16).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2024, 9, 17).unwrap()));
        assert_eq!(dates.len(), 1);
    }
}
