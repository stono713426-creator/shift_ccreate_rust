//! 勤務シフト自動生成システム Excelデータ解析・日付パーサー (`parser.rs`)
//!
//! Version: v1.3.1
//! Updated: 2026-11-10 - v1.3.1 入力検証・防御的パース強化（重複スタッフ排除・和暦対応・存在しない日付[9/31等]の警告検出・未知スタッフ名警告・対象月自動補完）
//! - v1.3.0 (2026-11-08): 「勤務形態」列・既存「職種」列からの職種(看護師/検査)判定、手術スケジュール「注射日」vs「Ope日」分離判定、PM有休(午後/半休)日付抽出対応
//! - v1.2.0 (2026-11-01): パーサー全面堅牢化: 年跨ぎM/D自動年補完、同日複数OPE統合、前月実績年月認識、3ヶ月履歴初期化、パート希望出勤曜日補完、OR表記拡張
//! - v1.1.0 (2026-10-31): シフト制約条件シートの列ヘッダー検出修正および除外フラグ大文字小文字対応
//! - v1.0.1 (2026-10-31): 範囲日付パーサー改修(スペースなしハイフン範囲・年跨ぎ範囲対応)
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
    /// 午後（PM）有休・半休等があらかじめ設定されている日付集合（土曜2人勤務許容判定用）
    #[serde(default)]
    pub pm_leave_dates: HashSet<NaiveDate>,
    pub is_core: bool,
    pub is_part_1f: bool,
    pub order: usize,
}

impl Staff {
    /// 職種が「看護師（看護師 / Ns）」であるか判定
    pub fn is_nurse(&self) -> bool {
        let norm: String = self.role.nfkc().collect();
        let lower = norm.to_lowercase();
        norm.contains("看護師") || lower.contains("ns")
    }

    /// 職種が「検査（検査 / スタッフ 等、看護師以外）」であるか判定
    pub fn is_exam(&self) -> bool {
        if self.is_nurse() {
            return false;
        }
        let norm: String = self.role.nfkc().collect();
        let lower = norm.trim().to_lowercase();
        lower.is_empty()
            || norm.contains("検査")
            || norm.contains("スタッフ")
            || lower.contains("ort")
    }
}

/// 手術種別テキストを個別種別トークンへ分割する
pub fn split_ope_tokens(ope_str: &str) -> Vec<String> {
    let norm: String = ope_str.nfkc().collect();
    norm.split([',', '、', ';', '/', '／', '・', ' ', '\t', '\n', '\r'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 手術種別文字列に「注射」以外のオペ（Vit, cat, gla, 下垂 等）が含まれる「Ope日」か判定
pub fn is_ope_text(ope_str: &str) -> bool {
    split_ope_tokens(ope_str)
        .iter()
        .any(|tok| !tok.contains("注射"))
}

/// 手術種別文字列に「注射」が含まれる「注射日」か判定
pub fn is_injection_text(ope_str: &str) -> bool {
    split_ope_tokens(ope_str)
        .iter()
        .any(|tok| tok.contains("注射"))
}

/// 指定日が「Ope日」（`注射` 以外のオペがある日）か判定
pub fn is_ope_date(ope_dict: &HashMap<NaiveDate, String>, date: &NaiveDate) -> bool {
    ope_dict.get(date).is_some_and(|s| is_ope_text(s))
}

/// 指定日が「注射日」（`注射` がある日）か判定
pub fn is_injection_date(ope_dict: &HashMap<NaiveDate, String>, date: &NaiveDate) -> bool {
    ope_dict.get(date).is_some_and(|s| is_injection_text(s))
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
    /// 直近最大3ヶ月分の主公休（または週4第2公休）曜日履歴（先頭が直近1ヶ月前: `[prev_1m, prev_2m, prev_3m]`）
    #[serde(default)]
    pub prev_month_main_offs_history: HashMap<String, Vec<u32>>,
    /// 前月シフト実績シートのタイトル等から検出された年月 `(year, month)`
    #[serde(default)]
    pub prev_sheet_year_month: Option<(i32, u32)>,
    pub prev_month_tail_shifts: HashMap<String, Vec<u8>>,
    pub existing_shifts: HashMap<(i32, u32), HashMap<String, HashMap<u32, u8>>>,
    /// 解析時に検出された入力データの注意・警告メッセージ一覧
    #[serde(default)]
    pub parse_warnings: Vec<String>,
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
    parse_date_list_for_months(cell, default_year, &[], holidays_jp_dict)
}

/// 対象年月リスト (`target_months`) を考慮してセル値から日付リストをパースする。
/// 年省略表記 (`M/D`) が入力された場合、`target_months` に該当月があればその年（例: 9月〜翌3月表での `1/15` -> 翌年1月）を優先補完する。
pub fn parse_date_list_for_months(
    cell: &Data,
    default_year: i32,
    target_months: &[(i32, u32)],
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

            // 5. 時刻表記除去、曜日表記除去 (例: (月), （火）)、および半休/AM・PM付記除去 (例: 9/12(PM), 9/12午後)
            let re_time = Regex::new(r"\b\d{1,2}:\d{2}(:\d{2})?\b").unwrap();
            let cleaned_time = re_time.replace_all(trimmed, "");
            let re_weekday = Regex::new(r"[\(（][月火水木金土日][\)）]").unwrap();
            let cleaned_wday = re_weekday.replace_all(&cleaned_time, "");
            let re_ampm =
                Regex::new(r"(?i)[\(（]?\s*(?:am|pm|午前|午後|半休|早退|後休|前休)(?:有休|休)?\s*[\)）]?").unwrap();
            let cleaned = re_ampm.replace_all(&cleaned_wday, "");

            // 記号 [,、;・\n\r]+ でトークン分割
            let re_split = Regex::new(r"[,、;・\n\r]+").unwrap();
            let raw_tokens: Vec<&str> = re_split
                .split(&cleaned)
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();

            // 各トークンの日付・範囲パース
            for tok in raw_tokens {
                parse_date_token(tok, default_year, target_months, &mut dates);
            }
        }
        _ => {}
    }

    filter_holidays_sundays(dates, holidays_jp_dict)
}

/// セル文字列から「午後（PM）有休・半休・早退」指定が付いた日付のみを抽出する
pub fn parse_pm_dates_for_months(
    cell: &Data,
    default_year: i32,
    target_months: &[(i32, u32)],
    holidays_jp_dict: Option<&HashMap<NaiveDate, String>>,
) -> HashSet<NaiveDate> {
    let mut dates = HashSet::new();
    let Data::String(raw_s) = cell else {
        return dates;
    };

    let normalized: String = raw_s.nfkc().collect();
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return dates;
    }

    let re_split = Regex::new(r"[,、;・\n\r]+").unwrap();
    let re_pm_marker = Regex::new(r"(?i)(?:pm|午後|半休|早退|後休)").unwrap();
    let re_time = Regex::new(r"\b\d{1,2}:\d{2}(:\d{2})?\b").unwrap();
    let re_weekday = Regex::new(r"[\(（][月火水木金土日][\)）]").unwrap();
    let re_ampm =
        Regex::new(r"(?i)[\(（]?\s*(?:am|pm|午前|午後|半休|早退|後休|前休)(?:有休|休)?\s*[\)）]?").unwrap();

    for seg in re_split.split(trimmed) {
        let seg_trim = seg.trim();
        if seg_trim.is_empty() || !re_pm_marker.is_match(seg_trim) {
            continue;
        }
        let c1 = re_time.replace_all(seg_trim, "");
        let c2 = re_weekday.replace_all(&c1, "");
        let cleaned = re_ampm.replace_all(&c2, "");
        parse_date_token(cleaned.trim(), default_year, target_months, &mut dates);
    }

    filter_holidays_sundays(dates, holidays_jp_dict)
}

/// 年省略 (`M/D`) 時の年を対象月リスト `target_months` から解決するヘルパー
fn resolve_year_for_month(month: u32, default_year: i32, target_months: &[(i32, u32)]) -> i32 {
    if target_months.is_empty() || target_months.contains(&(default_year, month)) {
        return default_year;
    }
    if let Some(&(y, _)) = target_months.iter().find(|&&(_, m)| m == month) {
        return y;
    }
    default_year
}

/// トークン文字列から日付範囲または単一日付を抽出し dates に追加
fn parse_date_token(
    tok: &str,
    default_year: i32,
    target_months: &[(i32, u32)],
    dates: &mut HashSet<NaiveDate>,
) {
    // 範囲表現正規表現:
    // 開始日付: YYYY/M/D または M/D (区切りは / または . または -)
    // 範囲記号: 〜, ~, ～, ー, ―, –, —, またはハイフン - (前後の空白許容)
    // 終了日付: YYYY/M/D または M/D (区切りは / または . または -)
    let re_range = Regex::new(
        r"(\d{2,4}[/.-]\d{1,2}[/.-]\d{1,2}|\d{1,2}[/.-]\d{1,2})\s*(?:[〜~～ー―–—-]|-\s*)\s*(\d{2,4}[/.-]\d{1,2}[/.-]\d{1,2}|\d{1,2}[/.-]\d{1,2})"
    ).unwrap();

    let mut remaining = tok.to_string();

    for cap in re_range.captures_iter(tok) {
        let whole_match = cap.get(0).unwrap();
        let s1 = cap.get(1).unwrap().as_str();
        let s2 = cap.get(2).unwrap().as_str();

        let d1 = parse_single_date_with_months(s1, default_year, target_months);
        // 開始日に年が確定していれば終了日の基準年として引き継ぐ
        let year_for_d2 = d1.map(|d| d.year()).unwrap_or(default_year);
        let d2 = if has_explicit_year(s1) {
            parse_single_date(s2, year_for_d2)
        } else {
            parse_single_date_with_months(s2, year_for_d2, target_months)
        };

        if let (Some(mut start), Some(mut end)) = (d1, d2) {
            // 年跨ぎ補正 (例: 12/29 - 1/4)
            if !has_explicit_year(s2) && end < start && end.month() < start.month() {
                if let Some(next_year_end) = NaiveDate::from_ymd_opt(start.year() + 1, end.month(), end.day()) {
                    end = next_year_end;
                }
            }

            if start > end {
                std::mem::swap(&mut start, &mut end);
            }

            let days_diff = (end - start).num_days();
            if days_diff <= 100 {
                let mut curr = start;
                while curr <= end {
                    dates.insert(curr);
                    curr += Duration::days(1);
                }
            }
        }
        remaining = remaining.replace(whole_match.as_str(), " ");
    }

    // 残りの文字列から単一日付をパース (空白区切り)
    for part in remaining.split_whitespace() {
        if let Some(d) = parse_single_date_with_months(part, default_year, target_months) {
            dates.insert(d);
        }
    }
}

/// 日付文字列に年情報が明示されているか判定 (パーツ数が3つ以上: YYYY/M/D 等)
fn has_explicit_year(s: &str) -> bool {
    let cleaned = s
        .replace('年', "/")
        .replace('月', "/")
        .replace('.', "/")
        .replace('日', "");
    let parts: Vec<&str> = cleaned
        .split(['/', '-'])
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    parts.len() >= 3
}

/// 単一の日付文字列をパース (`target_months` による年補完対応)
fn parse_single_date_with_months(
    s: &str,
    default_year: i32,
    target_months: &[(i32, u32)],
) -> Option<NaiveDate> {
    let cleaned = s
        .trim()
        .replace('年', "/")
        .replace('月', "/")
        .replace('.', "/")
        .replace('日', "");
    let parts: Vec<&str> = cleaned
        .split(['/', '-'])
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();

    if parts.len() == 2 {
        let m = parts[0].parse::<u32>().ok()?;
        let d = parts[1].parse::<u32>().ok()?;
        let resolved_y = resolve_year_for_month(m, default_year, target_months);
        return NaiveDate::from_ymd_opt(resolved_y, m, d);
    }
    parse_single_date(s, default_year)
}

/// 単一の日付文字列をパース
fn parse_single_date(s: &str, default_year: i32) -> Option<NaiveDate> {
    let mut norm: String = s.nfkc().collect();
    norm = norm.trim().to_string();
    // 和暦（令和 / R）表記の西暦変換 (例: "令和8年9月1日", "R8.9.1", "r8/9/1")
    let re_reiwa = Regex::new(r"^(?:令和|R|r)\s*(\d{1,2})\s*[年/.-]\s*(.+)$").unwrap();
    if let Some(caps) = re_reiwa.captures(&norm) {
        if let Ok(r_year) = caps[1].parse::<i32>() {
            norm = format!("{}/{}", 2018 + r_year, &caps[2]);
        }
    }

    let cleaned = norm
        .replace('年', "/")
        .replace('月', "/")
        .replace('.', "/")
        .replace('日', "");
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

/// セル文字列内に「9/31」「2/29(平年)」などの暦上存在しない無効な日付指定が含まれていないか検査する
fn detect_invalid_date_strings(
    cell: &Data,
    default_year: i32,
    target_months: &[(i32, u32)],
) -> Vec<String> {
    let mut invalid_list = Vec::new();
    let Data::String(raw_s) = cell else {
        return invalid_list;
    };
    let norm: String = raw_s.nfkc().collect();
    let re_md = Regex::new(r"(?:(\d{2,4})[年/.-])?(\d{1,2})[月/.-](\d{1,2})日?").unwrap();
    for caps in re_md.captures_iter(&norm) {
        let m = match caps[2].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let d = match caps[3].parse::<u32>() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let y = if let Some(y_m) = caps.get(1) {
            let mut parsed_y = y_m.as_str().parse::<i32>().unwrap_or(default_year);
            if parsed_y < 100 {
                parsed_y += 2000;
            }
            parsed_y
        } else {
            resolve_year_for_month(m, default_year, target_months)
        };
        if !(1..=12).contains(&m) || NaiveDate::from_ymd_opt(y, m, d).is_none() {
            invalid_list.push(caps[0].to_string());
        }
    }
    invalid_list
}

/// スタッフ氏名の正規化（NFKC正規化＋前後空白・全角空白除去）
fn normalize_staff_name(raw: &str) -> String {
    let norm: String = raw.nfkc().collect();
    norm.replace('\u{3000}', " ").trim().to_string()
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
    let mut workbook = open_workbook_auto(path).map_err(|e| {
        format!(
            "Excelファイルオープンエラー: {e}（Excel等で開かれている場合や壊れている場合は閉じてから再度お試しください）"
        )
    })?;

    let sheet_names = workbook.sheet_names().to_vec();
    if sheet_names.is_empty() {
        return Err("Excel内にシートが存在しません。".to_string());
    }

    let mut parse_warnings: Vec<String> = Vec::new();
    let mut all_known_staff_names: HashSet<String> = HashSet::new();

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

    // デフォルト年（対象月の先頭、なければ現在の年）
    let current_year = chrono::Local::now().year();
    let default_year = target_months.first().map(|(y, _)| *y).unwrap_or(current_year);

    // 3. スタッフマスタシートの探索とパース
    // 優先順: 「スタッフ」/「マスタ」/「勤務条件」を含むシート、なければ0番目
    let staff_sheet_name = sheet_names
        .iter()
        .find(|s| s.contains("スタッフ") || s.contains("マスタ") || s.contains("条件表"))
        .unwrap_or(&sheet_names[0]);

    let staffs = if let Ok(range) = workbook.worksheet_range(staff_sheet_name) {
        parse_staff_sheet(
            &range,
            default_year,
            &target_months,
            holidays,
            &config,
            &mut parse_warnings,
            &mut all_known_staff_names,
        )?
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

    // 4.1 制約ルールの対象スタッフ名チェック（タイポ・未登録名検知）
    if !all_known_staff_names.is_empty() {
        for rule in &rules {
            for target_name in &rule.target_staffs {
                if !all_known_staff_names.contains(target_name) {
                    parse_warnings.push(format!(
                        "⚠️ [制約スタッフ名警告] シフト制約条件『{}』の対象スタッフ『{}』がスタッフマスタに存在しません（表記ゆれ・誤字の可能性があります）。",
                        rule.name, target_name
                    ));
                }
            }
        }
    }

    // 5. 前月実績シートの探索とパース
    let mut prev_month_main_offs = HashMap::new();
    let mut prev_month_main_offs_history: HashMap<String, Vec<u32>> = HashMap::new();
    let mut prev_sheet_year_month = None;
    let mut prev_month_tail_shifts = HashMap::new();
    for s_name in &sheet_names {
        if s_name.contains("前月") || s_name.contains("実績") {
            if let Ok(range) = workbook.worksheet_range(s_name) {
                parse_prev_month_sheet(
                    &range,
                    &mut prev_month_main_offs,
                    &mut prev_month_tail_shifts,
                    &mut prev_sheet_year_month,
                );
            }
        }
    }
    for (staff_name, &w) in &prev_month_main_offs {
        prev_month_main_offs_history.insert(staff_name.clone(), vec![w]);
    }

    // 5.1 手術スケジュールから対象月が1件も得られなかった場合のフォールバック補完
    if target_months.is_empty() {
        let mut fallback_months = HashSet::new();
        for s in &staffs {
            for d in s
                .vacation_dates
                .iter()
                .chain(s.special_vacation_dates.iter())
                .chain(s.preferred_off_dates.iter())
                .chain(s.specified_off_dates.iter())
            {
                fallback_months.insert((d.year(), d.month()));
            }
        }
        if fallback_months.is_empty() {
            if let Some((py, pm)) = prev_sheet_year_month {
                let (ny, nm) = if pm == 12 { (py + 1, 1) } else { (py, pm + 1) };
                fallback_months.insert((ny, nm));
            }
        }
        if !fallback_months.is_empty() {
            let mut sorted_fb: Vec<(i32, u32)> = fallback_months.into_iter().collect();
            sorted_fb.sort_unstable();
            let desc = sorted_fb
                .iter()
                .map(|(y, m)| format!("{y}年{m}月"))
                .collect::<Vec<_>>()
                .join(", ");
            parse_warnings.push(format!(
                "⚠️ [対象月補完] 手術スケジュールから対象月を検出できなかったため、スタッフ休暇日・前月実績から対象月（{desc}）を自動推定しました。"
            ));
            target_months = sorted_fb;
        }
    }

    // 6. 既存生成シフトシートの探索 (例: "2024年09月シフト", "24年09月シフト")
    let mut existing_shifts = HashMap::new();
    let re_shift_sheet = Regex::new(r"(\d{2,4})年(\d{1,2})月(?:シフト)?").unwrap();
    for s_name in &sheet_names {
        if let Some(caps) = re_shift_sheet.captures(s_name) {
            let mut y = caps[1].parse::<i32>().unwrap_or(default_year);
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
        prev_month_main_offs_history,
        prev_sheet_year_month,
        prev_month_tail_shifts,
        existing_shifts,
        parse_warnings,
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
            let s = cell.to_string().trim().to_string();
            let s_lower = s.to_lowercase();
            if s.contains("作成") || s.contains("対象") || s.contains("出力") || s.contains("生成") {
                col_target = Some(c_idx);
            } else if s.contains("種別") || s.contains("内容") || s.contains("手術") || s_lower.contains("ope") || s.contains("オペ") {
                col_type = Some(c_idx);
            } else if s.contains("日付") || s_lower.contains("date") {
                col_date = Some(c_idx);
            } else if col_date.is_none() && s.contains("日") && !s.contains("曜日") {
                col_date = Some(c_idx);
            }
        }
        if col_date.is_some() {
            header_row_idx = r_idx;
            break;
        }
    }

    let c_date = col_date.unwrap_or(0);
    let c_type = col_type.unwrap_or(1);

    // 初期基準年の推定（シート内に明示年またはシリアル値があれば先頭の年、なければ現在年）
    let mut running_year = chrono::Local::now().year();
    for row in rows.iter().skip(header_row_idx + 1) {
        if let Some(cell) = row.get(c_date) {
            let s = cell.to_string();
            if matches!(cell, Data::Int(_) | Data::Float(_) | Data::DateTime(_) | Data::DateTimeIso(_))
                || has_explicit_year(&s)
            {
                let probe = parse_date_list(cell, running_year, None);
                if let Some(d) = probe.iter().min() {
                    running_year = d.year();
                    break;
                }
            }
        }
    }
    let mut prev_month: Option<u32> = None;

    for row in rows.iter().skip(header_row_idx + 1) {
        if row.len() <= c_date {
            continue;
        }
        let date_cell = &row[c_date];
        let type_cell = if row.len() > c_type { &row[c_type] } else { &Data::Empty };

        let raw_s = date_cell.to_string();
        let is_yearless_str = matches!(date_cell, Data::String(_))
            && !has_explicit_year(&raw_s)
            && !raw_s.trim().chars().all(|c| c.is_ascii_digit() || c == '.');

        let mut parsed_vec: Vec<NaiveDate> = parse_date_list(date_cell, running_year, None)
            .into_iter()
            .collect();
        parsed_vec.sort_unstable();

        // 年省略表記で12月 -> 1月のように月が巻き戻った場合は翌年へロールオーバー
        if is_yearless_str {
            if let (Some(pm), Some(first_d)) = (prev_month, parsed_vec.first()) {
                if first_d.month() < pm && pm >= 10 && first_d.month() <= 3 {
                    running_year += 1;
                    parsed_vec = parse_date_list(date_cell, running_year, None)
                        .into_iter()
                        .collect();
                    parsed_vec.sort_unstable();
                }
            }
        }

        let type_str = type_cell.to_string().trim().to_string();

        let is_marked = if let Some(ct) = col_target {
            if row.len() > ct {
                let val_str = row[ct].to_string().trim().to_string();
                val_str == "○" || val_str == "◯" || val_str == "1" || val_str.eq_ignore_ascii_case("true")
            } else {
                false
            }
        } else {
            false
        };

        for d in parsed_vec {
            running_year = d.year();
            prev_month = Some(d.month());
            target_months.insert((d.year(), d.month()));
            if is_marked {
                marked_months.insert((d.year(), d.month()));
            }
            if !type_str.is_empty() {
                ope_dict
                    .entry(d)
                    .and_modify(|existing| {
                        if !existing.contains(&type_str) {
                            existing.push(',');
                            existing.push_str(&type_str);
                        }
                    })
                    .or_insert_with(|| type_str.clone());
            }
        }
    }
}

/// スタッフマスタシートのパース
fn parse_staff_sheet(
    range: &Range<Data>,
    default_year: i32,
    target_months: &[(i32, u32)],
    holidays: &HashMap<NaiveDate, String>,
    config: &ShiftConfig,
    parse_warnings: &mut Vec<String>,
    all_known_staff_names: &mut HashSet<String>,
) -> Result<Vec<Staff>, String> {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return Err("スタッフシートが空です。".to_string());
    }

    // ヘッダー行探索 (氏名, 所属, 職種/勤務形態, 雇用形態, 基本公休, 有休, 特別有休, 希望休, 指定公休, 希望出勤曜日, 除外)
    let mut col_name = None;
    let mut col_section = None;
    let mut col_role = None;
    let mut col_work_form = None;
    let mut col_employment = None;
    let mut col_pattern = None;
    let mut col_basic_off = None;
    let mut col_preferred_work = None;
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
            } else if s.contains("勤務形態") {
                col_work_form = Some(c_idx);
            } else if s.contains("役職") || s.contains("役割") || s.contains("資格") || s.contains("職種") {
                col_role = Some(c_idx);
            } else if s.contains("雇用形態") || s.contains("形態") {
                col_employment = Some(c_idx);
            } else if s.contains("勤務パターン") || s.contains("パターン") || (s.contains("週") && !s.contains("休")) {
                col_pattern = Some(c_idx);
            } else if s.contains("希望公休曜日") || s.contains("基本公休") || s.contains("公休曜日") {
                col_basic_off = Some(c_idx);
            } else if s.contains("希望出勤") || s.contains("出勤曜日") {
                col_preferred_work = Some(c_idx);
            } else if s.contains("特別") {
                col_special_vacation = Some(c_idx);
            } else if s.contains("有休") {
                col_vacation = Some(c_idx);
            } else if s.contains("希望休") {
                col_preferred_off = Some(c_idx);
            } else if s.contains("指定公休")
                || s.contains("公休指定")
                || (s.contains("公休") && !s.contains("曜日") && (s.contains("指定") || s.contains("日")))
            {
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
    let mut seen_active_names: HashSet<String> = HashSet::new();

    for (r_idx, row) in rows.iter().enumerate().skip(header_row_idx + 1) {
        if row.len() <= c_name {
            continue;
        }

        let name = normalize_staff_name(&row[c_name].to_string());
        if name.is_empty() || name.contains("合計") || name.contains("人数") {
            continue;
        }

        // 除外判定前でもスタッフマスタ登録者として記録（除外スタッフが制約条件シートに残っている場合の誤警告防止）
        all_known_staff_names.insert(name.clone());

        // 除外列判定
        if let Some(c_ex) = col_exclude {
            if row.len() > c_ex {
                let ex_val = row[c_ex].to_string().trim().to_string();
                if ex_val == "1"
                    || ex_val.eq_ignore_ascii_case("true")
                    || ex_val == "○"
                    || ex_val == "◯"
                    || ex_val.contains("除外")
                {
                    continue;
                }
            }
        }

        // 同名スタッフ重複チェック
        if !seen_active_names.insert(name.clone()) {
            parse_warnings.push(format!(
                "⚠️ [重複スタッフ警告] スタッフマスタに同名スタッフ『{name}』（{}行目）が重複登録されています。後続の重複行をスキップしました。",
                r_idx + 1
            ));
            continue;
        }

        // 各休暇・公休セルに存在しない日付（例: 9/31, 平年2/29）が含まれていないか検証
        let leave_col_labels = [
            (col_vacation, "有休"),
            (col_special_vacation, "特別有休"),
            (col_preferred_off, "希望休"),
            (col_specified_off, "指定公休"),
        ];
        for (col_opt, col_label) in leave_col_labels {
            if let Some(cell) = col_opt.and_then(|c| row.get(c)) {
                let invalid_tokens = detect_invalid_date_strings(cell, default_year, target_months);
                for bad_tok in invalid_tokens {
                    parse_warnings.push(format!(
                        "⚠️ [日付入力警告] スタッフ『{name}』の{col_label}欄に存在しない日付『{bad_tok}』が含まれています（無視されました）。"
                    ));
                }
            }
        }

        let section = col_section.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let raw_role = col_role.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let raw_work_form = col_work_form.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();
        let raw_employment = col_employment.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        // 「勤務形態」列および「職種」列の両方から職種（看護師(Ns) / 検査(スタッフ)）を判定
        let wf_norm: String = raw_work_form.nfkc().collect();
        let wf_lower = wf_norm.to_lowercase();
        let wf_has_role = wf_norm.contains("看護師")
            || wf_lower.contains("ns")
            || wf_norm.contains("検査")
            || wf_norm.contains("スタッフ")
            || wf_lower.contains("ort");

        let role = if wf_has_role {
            raw_work_form.clone()
        } else if !raw_role.is_empty() {
            raw_role
        } else {
            raw_work_form.clone()
        };

        // 雇用形態（「雇用形態」列が空で「勤務形態」列に正社員・パート区分が入っている場合も補完）
        let employment = if !raw_employment.is_empty() {
            raw_employment
        } else if wf_norm.contains("パート社員") {
            "パート社員".to_string()
        } else if wf_norm.contains("正社員") {
            "正社員".to_string()
        } else if wf_norm.contains("パート") {
            "パート".to_string()
        } else {
            String::new()
        };

        let pattern = col_pattern.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        let basic_off_str = col_basic_off.and_then(|c| row.get(c)).map(|d| d.to_string().trim().to_string()).unwrap_or_default();

        // 曜日抽出: 月火水木金土日 -> 0..=6
        let mut basic_off_days = extract_weekdays(&basic_off_str);

        let is_core = employment == "正社員"
            || employment == "パート社員"
            || (employment.is_empty() && (pattern.contains("週5") || pattern.contains("週4")));
        let is_part_1f = section == "1階" && employment == "パート";
        let is_special = pattern.contains("必要に応じて週5") || pattern.contains("必要に応じて");

        // basic_off_type (もしくは / あるいは / または / or に対応)
        let basic_off_lower = basic_off_str.to_lowercase();
        let mut basic_off_type = if basic_off_str.contains("もしくは")
            || basic_off_str.contains("あるいは")
            || basic_off_str.contains("または")
            || basic_off_lower.contains("or")
        {
            OffType::Or
        } else if !basic_off_days.is_empty() {
            OffType::And
        } else {
            OffType::Or
        };

        // 非コアスタッフ（パート等）で「希望公休曜日」が空欄かつ「希望出勤曜日」のみ記入されている場合の自動補完
        if !is_core && basic_off_days.is_empty() {
            let pref_work_str = col_preferred_work
                .and_then(|c| row.get(c))
                .map(|d| d.to_string().trim().to_string())
                .unwrap_or_default();
            if !pref_work_str.is_empty() && !pref_work_str.contains("必須") {
                let work_days = extract_weekdays(&pref_work_str);
                if !work_days.is_empty() {
                    basic_off_days = (0..=6).filter(|w| !work_days.contains(w)).collect();
                    basic_off_type = OffType::And;
                }
            }
        }

        // 有休（日・祝除外、年省略時の target_months 補完対応）
        let vacation_dates = col_vacation
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list_for_months(d, default_year, target_months, Some(holidays)))
            .unwrap_or_default();

        // 特別有休（日・祝除外）
        let special_vacation_dates = col_special_vacation
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list_for_months(d, default_year, target_months, Some(holidays)))
            .unwrap_or_default();

        // 希望休（日・祝除外しない）
        let preferred_off_dates = col_preferred_off
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list_for_months(d, default_year, target_months, None))
            .unwrap_or_default();

        // 指定公休（日・祝除外）
        let specified_off_dates = col_specified_off
            .and_then(|c| row.get(c))
            .map(|d| parse_date_list_for_months(d, default_year, target_months, Some(holidays)))
            .unwrap_or_default();

        // 午後（PM）有休・半休・早退の指定日を収集（土曜日2人勤務許容判定用）
        let mut pm_leave_dates = HashSet::new();
        for col_opt in [col_vacation, col_special_vacation, col_preferred_off, col_specified_off] {
            if let Some(cell) = col_opt.and_then(|c| row.get(c)) {
                pm_leave_dates.extend(parse_pm_dates_for_months(
                    cell,
                    default_year,
                    target_months,
                    Some(holidays),
                ));
            }
        }

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
            pm_leave_dates,
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

    if staffs.is_empty() {
        return Err("有効なスタッフが1名も登録されていません（全員除外またはシートが空です）。".to_string());
    }

    Ok(staffs)
}

/// 文字列から曜日番号 (0=月..6=日) を出現順・重複なしで抽出
fn extract_weekdays(s: &str) -> Vec<u32> {
    let mut days = Vec::new();
    for ch in s.chars() {
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
            if !days.contains(&w) {
                days.push(w);
            }
        }
    }
    days
}

/// 任意のセル値から整数 (`i32`) を堅牢に抽出（Int / Float / 全角数字文字列すべて対応）
fn cell_to_i32(cell: &Data) -> Option<i32> {
    match cell {
        Data::Int(i) => Some(*i as i32),
        Data::Float(f) => Some(f.round() as i32),
        Data::String(s) => {
            let norm: String = s.nfkc().collect();
            let trimmed = norm.trim();
            if let Ok(i) = trimmed.parse::<i32>() {
                Some(i)
            } else if let Ok(f) = trimmed.parse::<f64>() {
                Some(f.round() as i32)
            } else {
                None
            }
        }
        _ => None,
    }
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
            let s_lower = s.to_lowercase();
            if s.contains("レベル") || s.contains("重要度") || s.contains("優先度") {
                col_level = Some(c_idx);
            } else if s.contains("対象") || s.contains("スタッフ") {
                col_staffs = Some(c_idx);
            } else if s.contains("最低") || s.contains("最小") || s_lower.contains("min") {
                col_min = Some(c_idx);
            } else if s.contains("推奨") || s_lower.contains("rec") {
                col_rec = Some(c_idx);
            } else if s.contains("最大") || s_lower.contains("max") {
                col_max = Some(c_idx);
            } else if s.contains("種別") || s.contains("タイプ") || s.contains("区分") || s.contains("制限") {
                col_type = Some(c_idx);
            } else if s.contains("適用") || s.contains("日付") || s.contains("曜日") || s == "条件" {
                col_dates = Some(c_idx);
            } else if s.contains("ルール") || s.contains("名称") || s.contains("所属") || s.contains("部署") || s.contains("セクション") {
                col_name = Some(c_idx);
            } else if s.contains("条件") {
                if col_dates.is_none() {
                    col_dates = Some(c_idx);
                } else if col_name.is_none() {
                    col_name = Some(c_idx);
                }
            }
        }
        if (col_name.is_some() || col_dates.is_some()) && (col_min.is_some() || col_level.is_some()) {
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
            .map(normalize_staff_name)
            .filter(|s| !s.is_empty())
            .collect();

        let val_min = col_min.and_then(|c| row.get(c)).and_then(cell_to_i32).unwrap_or(0);
        let val_rec = col_rec.and_then(|c| row.get(c)).and_then(cell_to_i32).unwrap_or(0);
        let val_max = col_max.and_then(|c| row.get(c)).and_then(cell_to_i32).unwrap_or(99);
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

/// 前月実績シートのパース（縦型フォーマット・横型フォーマット両対応、タイトル行の年月抽出対応）
fn parse_prev_month_sheet(
    range: &Range<Data>,
    main_offs: &mut HashMap<String, u32>,
    tail_shifts: &mut HashMap<String, Vec<u8>>,
    sheet_year_month: &mut Option<(i32, u32)>,
) {
    let rows: Vec<&[Data]> = range.rows().collect();
    if rows.is_empty() {
        return;
    }

    // 先頭3行からタイトル年月 (例: "2026年12月 勤務シフト表") を検出
    let re_ym = Regex::new(r"(\d{2,4})\s*年\s*(\d{1,2})\s*月").unwrap();
    for row in rows.iter().take(3) {
        for cell in row.iter().take(5) {
            let norm: String = cell.to_string().nfkc().collect();
            if let Some(caps) = re_ym.captures(&norm) {
                if let (Ok(mut y), Ok(m)) = (caps[1].parse::<i32>(), caps[2].parse::<u32>()) {
                    if y < 100 {
                        y += 2000;
                    }
                    if (1..=12).contains(&m) {
                        *sheet_year_month = Some((y, m));
                        break;
                    }
                }
            }
        }
        if sheet_year_month.is_some() {
            break;
        }
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
            let s = normalize_staff_name(&cell.to_string());
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
            let name = normalize_staff_name(&row[0].to_string());
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
        let s = normalize_staff_name(&cell.to_string());
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
    fn test_parse_reiwa_and_invalid_date_detection() {
        let cell = Data::String("令和8年9月10日, R8/9/15, 9/31".to_string());
        let dates = parse_date_list(&cell, 2026, None);
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()));
        assert!(dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 15).unwrap()));
        assert_eq!(dates.len(), 2);

        let invalid = detect_invalid_date_strings(&cell, 2026, &[(2026, 9)]);
        assert_eq!(invalid, vec!["9/31".to_string()]);
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

    #[test]
    fn test_ope_vs_injection_detection() {
        assert!(is_ope_text("Vit,cat"));
        assert!(!is_injection_text("Vit,cat"));

        assert!(!is_ope_text("注射"));
        assert!(is_injection_text("注射"));

        assert!(!is_ope_text("硝子体注射"));
        assert!(is_injection_text("硝子体注射"));

        assert!(is_ope_text("cat, 注射"));
        assert!(is_injection_text("cat, 注射"));

        assert!(!is_ope_text(""));
        assert!(!is_injection_text(""));
    }

    #[test]
    fn test_pm_leave_parsing_and_role_detection() {
        let cell = Data::String("9/12(PM), 9/19午後, 9/26".to_string());
        let all_dates = parse_date_list(&cell, 2026, None);
        assert_eq!(all_dates.len(), 3);
        assert!(all_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()));
        assert!(all_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()));
        assert!(all_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()));

        let pm_dates = parse_pm_dates_for_months(&cell, 2026, &[], None);
        assert_eq!(pm_dates.len(), 2);
        assert!(pm_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 12).unwrap()));
        assert!(pm_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 19).unwrap()));
        assert!(!pm_dates.contains(&NaiveDate::from_ymd_opt(2026, 9, 26).unwrap()));

        let s_ns = Staff {
            name: "テストA".to_string(),
            section: "2階裏".to_string(),
            role: "看護師(Ns)".to_string(),
            employment: "正社員".to_string(),
            pattern: "週4日".to_string(),
            basic_off_days: vec![],
            basic_off_type: OffType::Or,
            is_auto_rotated_off: false,
            is_special: false,
            vacation_dates: HashSet::new(),
            special_vacation_dates: HashSet::new(),
            preferred_off_dates: HashSet::new(),
            specified_off_dates: HashSet::new(),
            pm_leave_dates: HashSet::new(),
            is_core: true,
            is_part_1f: false,
            order: 1,
        };
        assert!(s_ns.is_nurse());
        assert!(!s_ns.is_exam());

        let s_exam = Staff {
            role: "検査(スタッフ)".to_string(),
            ..s_ns
        };
        assert!(!s_exam.is_nurse());
        assert!(s_exam.is_exam());
    }
}
