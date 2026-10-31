//! 勤務シフト自動生成システム 現場フォーマット Excel レンダラー (`renderer.rs`)
//!
//! Version: v1.2.0
//! Updated: 2026-11-01 - v1.2.0 リリースバージョン同期
//! - v1.0.0 (2026-10-31): Phase 3 実装: SPECIFICATION.md セクション6 完全準拠の現場フォーマット Excel 出力
//!
//! - シート名: "{year}年{month:02}月シフト"
//! - ウィンドウ枠固定: E3 (row 2, col 4)
//! - 印刷設定: A4 横向き (Landscape), 1x1 ページ適合, 水平中央寄せ, グリッド線表示 ON
//! - 行高: 行1=30.0, 行2=28.0, データ行=22.0, 下部集計行=24.0
//! - 列幅・色分け:
//!   - 日付 (6.25, #305496), 曜日 (5.0, #305496), 備考 (11.625, #305496), 警告 (5.0, #C00000)
//!   - 2階表 (#1F4E78, 人数 9.875, 空列 4.0)
//!   - 2階裏 (#2F5597, 人数 9.875, 空列 4.0)
//!   - 1階 (#203764, 人数 8.25, 空列なし)
//!   - 総出勤人数 (9.875, #1B365D)
//!   - スタッフ列幅: 一律 13.0
//! - セル値・書式:
//!   - 日曜・祝日: 背景 #FCE4D6, 文字 #C00000, スタッフセル "休業"
//!   - 通常診療日: 有休 "有休", 特有 "特有", 出勤 "○" (手術日は文字色 #006100, 通常 #000000), 公休 "公" (文字色 #595959)
//!   - 警告列: 非休業日で 2階裏出勤数 <= warn_2f_back_count のとき "⚠️" (背景 #FCE4D6, 文字 #C00000)
//!   - 人数式: =COUNTIF({開始列}{行}:{終了列}{行}, "○")
//!   - 下部集計4行: 合計出勤日数 (=COUNTIF), 所定勤務日数, 公休日数 (=COUNTIF), 主公休曜日

use crate::parser::{OffType, ParsedExcelData, Staff};
use crate::solver::{MonthContext, SolveResult};
use chrono::NaiveDate;
use rust_xlsxwriter::{
    Color, Format, FormatAlign, FormatBorder, Workbook, Worksheet, XlsxError,
};
use std::collections::HashMap;
use std::path::Path;

/// 0-indexed の列番号を Excel 列記号 ("A", "B", ..., "Z", "AA", ...) に変換する。
pub fn col_to_letter(mut col: u16) -> String {
    let mut result = String::new();
    col += 1;
    while col > 0 {
        let rem = (col - 1) % 26;
        result.push((b'A' + rem as u8) as char);
        col = (col - 1) / 26;
    }
    result.chars().rev().collect()
}

/// 曜日番号 (0=月..6=日) を漢字1文字に変換する。
pub fn weekday_to_kanji(w: u32) -> &'static str {
    match w {
        0 => "月",
        1 => "火",
        2 => "水",
        3 => "木",
        4 => "金",
        5 => "土",
        6 => "日",
        _ => "",
    }
}

/// 単一月のレンダリング用コンテキスト
struct MonthRenderContext<'a> {
    parsed: &'a ParsedExcelData,
    result: &'a SolveResult,
    year: i32,
    month: u32,
    holidays: &'a HashMap<NaiveDate, String>,
}

/// 書式スタイルのセット
struct FormatSet {
    // タイトル行
    title: Format,
    // 列A〜D ヘッダー
    header_date: Format,
    header_warn: Format,
    // セクション別スタッフヘッダー
    header_2f_front: Format,
    header_2f_back: Format,
    header_1f: Format,
    // 人数ヘッダー
    header_count: Format,
    // 日付データ行：通常日
    data_date: Format,
    data_weekday: Format,
    data_desc: Format,
    data_warn_empty: Format,
    data_warn_alert: Format,
    data_work_normal: Format,
    data_work_ope: Format,
    data_off_public: Format,
    data_vacation: Format,
    data_sp_vacation: Format,
    data_count: Format,
    data_empty_col: Format,
    // 日付データ行：日曜・祝日行
    holiday_date: Format,
    holiday_weekday: Format,
    holiday_desc: Format,
    holiday_warn: Format,
    holiday_staff: Format,
    holiday_count: Format,
    holiday_empty_col: Format,
    // 下部集計行
    summary_label_top: Format,
    summary_label: Format,
    summary_val_top: Format,
    summary_val: Format,
    summary_text: Format,
    summary_empty_col_top: Format,
    summary_empty_col: Format,
}

impl FormatSet {
    fn new() -> Self {
        let meiryo = "メイリオ";
        let c_border = Color::RGB(0xBFBFBF);

        // 基本ボーダー付きフォーマット作成ヘルパー
        let make_base = || {
            Format::new()
                .set_font_name(meiryo)
                .set_font_size(10)
                .set_align(FormatAlign::Center)
                .set_align(FormatAlign::VerticalCenter)
                .set_border(FormatBorder::Thin)
                .set_border_color(c_border)
        };

        // タイトル
        let title = Format::new()
            .set_font_name(meiryo)
            .set_font_size(14)
            .set_bold()
            .set_align(FormatAlign::Left)
            .set_align(FormatAlign::VerticalCenter);

        // ヘッダー共通設定ヘルパー
        let make_header = |bg: Color| {
            Format::new()
                .set_font_name(meiryo)
                .set_font_size(10)
                .set_bold()
                .set_font_color(Color::RGB(0xFFFFFF))
                .set_background_color(bg)
                .set_align(FormatAlign::Center)
                .set_align(FormatAlign::VerticalCenter)
                .set_border(FormatBorder::Thin)
                .set_border_color(c_border)
        };

        let header_date = make_header(Color::RGB(0x305496));
        let header_warn = make_header(Color::RGB(0xC00000));
        let header_2f_front = make_header(Color::RGB(0x1F4E78));
        let header_2f_back = make_header(Color::RGB(0x2F5597));
        let header_1f = make_header(Color::RGB(0x203764));
        let header_count = make_header(Color::RGB(0x1B365D));

        // 通常データ行
        let data_date = make_base();
        let data_weekday = make_base();
        let data_desc = make_base().set_align(FormatAlign::Left);
        let data_warn_empty = make_base();

        let data_warn_alert = make_base()
            .set_font_color(Color::RGB(0xC00000))
            .set_background_color(Color::RGB(0xFCE4D6))
            .set_bold();

        let data_work_normal = make_base().set_font_color(Color::RGB(0x000000));
        let data_work_ope = make_base()
            .set_font_color(Color::RGB(0x006100))
            .set_bold();
        let data_off_public = make_base().set_font_color(Color::RGB(0x595959));

        let data_vacation = make_base()
            .set_font_color(Color::RGB(0x000000))
            .set_background_color(Color::RGB(0xE2EFDA));

        let data_sp_vacation = make_base()
            .set_font_color(Color::RGB(0x000000))
            .set_background_color(Color::RGB(0xDDEBF7));

        let data_count = make_base().set_bold();

        let data_empty_col = Format::new()
            .set_border(FormatBorder::Thin)
            .set_border_color(c_border);

        // 日曜・祝日行
        let bg_holiday = Color::RGB(0xFCE4D6);
        let fg_holiday = Color::RGB(0xC00000);

        let make_holiday = || {
            make_base()
                .set_background_color(bg_holiday)
                .set_font_color(fg_holiday)
        };

        let holiday_date = make_holiday();
        let holiday_weekday = make_holiday();
        let holiday_desc = make_holiday().set_align(FormatAlign::Left);
        let holiday_warn = make_holiday();
        let holiday_staff = make_holiday();
        let holiday_count = make_holiday();

        let holiday_empty_col = Format::new()
            .set_background_color(bg_holiday)
            .set_border(FormatBorder::Thin)
            .set_border_color(c_border);

        // 下部集計行
        let summary_label_top = Format::new()
            .set_font_name(meiryo)
            .set_font_size(10)
            .set_bold()
            .set_align(FormatAlign::Right)
            .set_align(FormatAlign::VerticalCenter)
            .set_border(FormatBorder::Thin)
            .set_border_top(FormatBorder::Double)
            .set_border_color(c_border)
            .set_border_top_color(Color::RGB(0x000000));

        let summary_label = Format::new()
            .set_font_name(meiryo)
            .set_font_size(10)
            .set_bold()
            .set_align(FormatAlign::Right)
            .set_align(FormatAlign::VerticalCenter)
            .set_border(FormatBorder::Thin)
            .set_border_color(c_border);

        let summary_val_top = Format::new()
            .set_font_name(meiryo)
            .set_font_size(10)
            .set_bold()
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter)
            .set_border(FormatBorder::Thin)
            .set_border_top(FormatBorder::Double)
            .set_border_color(c_border)
            .set_border_top_color(Color::RGB(0x000000));

        let summary_val = Format::new()
            .set_font_name(meiryo)
            .set_font_size(10)
            .set_bold()
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter)
            .set_border(FormatBorder::Thin)
            .set_border_color(c_border);

        let summary_text = summary_val.clone();

        let summary_empty_col_top = Format::new()
            .set_border(FormatBorder::Thin)
            .set_border_top(FormatBorder::Double)
            .set_border_color(c_border)
            .set_border_top_color(Color::RGB(0x000000));

        let summary_empty_col = Format::new()
            .set_border(FormatBorder::Thin)
            .set_border_color(c_border);

        Self {
            title,
            header_date,
            header_warn,
            header_2f_front,
            header_2f_back,
            header_1f,
            header_count,
            data_date,
            data_weekday,
            data_desc,
            data_warn_empty,
            data_warn_alert,
            data_work_normal,
            data_work_ope,
            data_off_public,
            data_vacation,
            data_sp_vacation,
            data_count,
            data_empty_col,
            holiday_date,
            holiday_weekday,
            holiday_desc,
            holiday_warn,
            holiday_staff,
            holiday_count,
            holiday_empty_col,
            summary_label_top,
            summary_label,
            summary_val_top,
            summary_val,
            summary_text,
            summary_empty_col_top,
            summary_empty_col,
        }
    }
}

/// 列の割り当て情報
#[derive(Debug, Clone)]
enum ColType {
    Fixed(FixedCol),
    Staff(Staff),
    SectionCount(String),
    Empty,
    TotalCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixedCol {
    Date,
    Weekday,
    Desc,
    Warn,
}

/// レイアウト計算結果
struct SheetLayout {
    columns: Vec<ColType>,
    sec1_range: Option<(u16, u16)>,
    sec2_range: Option<(u16, u16)>,
    sec3_range: Option<(u16, u16)>,
    sec1_count_col: Option<u16>,
    sec2_count_col: Option<u16>,
    sec3_count_col: Option<u16>,
}

impl SheetLayout {
    fn new(parsed: &ParsedExcelData) -> Self {
        let mut columns = Vec::new();

        // 固定列 A〜D
        columns.push(ColType::Fixed(FixedCol::Date));
        columns.push(ColType::Fixed(FixedCol::Weekday));
        columns.push(ColType::Fixed(FixedCol::Desc));
        columns.push(ColType::Fixed(FixedCol::Warn));

        let mut sec1_range = None;
        let mut sec2_range = None;
        let mut sec3_range = None;
        let mut sec1_count_col = None;
        let mut sec2_count_col = None;
        let mut sec3_count_col = None;

        let sections = [
            ("2階表", true),
            ("2階裏", true),
            ("1階", false),
        ];

        for (sec_name, has_empty) in sections {
            let staffs: Vec<Staff> = parsed
                .staffs
                .iter()
                .filter(|s| s.section == sec_name)
                .cloned()
                .collect();

            if !staffs.is_empty() {
                let start_c = columns.len() as u16;
                for s in staffs {
                    columns.push(ColType::Staff(s));
                }
                let end_c = (columns.len() - 1) as u16;
                let cnt_c = columns.len() as u16;
                columns.push(ColType::SectionCount(sec_name.to_string()));

                match sec_name {
                    "2階表" => {
                        sec1_range = Some((start_c, end_c));
                        sec1_count_col = Some(cnt_c);
                    }
                    "2階裏" => {
                        sec2_range = Some((start_c, end_c));
                        sec2_count_col = Some(cnt_c);
                    }
                    "1階" => {
                        sec3_range = Some((start_c, end_c));
                        sec3_count_col = Some(cnt_c);
                    }
                    _ => {}
                }

                if has_empty {
                    columns.push(ColType::Empty);
                }
            }
        }

        columns.push(ColType::TotalCount);

        Self {
            columns,
            sec1_range,
            sec2_range,
            sec3_range,
            sec1_count_col,
            sec2_count_col,
            sec3_count_col,
        }
    }
}

/// 単一月のワークシートを描画する
fn render_month_sheet(
    worksheet: &mut Worksheet,
    ctx: &MonthRenderContext,
    fmt: &FormatSet,
) -> Result<(), XlsxError> {
    let year = ctx.year;
    let month = ctx.month;
    let month_ctx = MonthContext::new(year, month, ctx.holidays);
    let layout = SheetLayout::new(ctx.parsed);

    // 1. ページ・表示設定
    worksheet.set_freeze_panes(2, 4)?; // E3固定 (row=2, col=4)
    worksheet.set_screen_gridlines(true);
    worksheet.set_print_gridlines(true);
    worksheet.set_paper_size(9); // A4 (9)
    worksheet.set_landscape();
    worksheet.set_print_fit_to_pages(1, 1);
    worksheet.set_print_center_horizontally(true);

    // 2. 行高・列幅の設定
    worksheet.set_row_height(0, 30.0)?; // 行1: タイトル
    worksheet.set_row_height(1, 28.0)?; // 行2: ヘッダー
    for d in 1..=month_ctx.days_in_month {
        worksheet.set_row_height(d + 1, 22.0)?; // データ行 (行3〜)
    }
    let last_day_row = month_ctx.days_in_month + 1; // 0-indexed (Excelでは days+2)
    for r in (last_day_row + 1)..=(last_day_row + 4) {
        worksheet.set_row_height(r, 24.0)?; // 下部集計4行
    }

    // 列幅の設定
    for (c_idx, col) in layout.columns.iter().enumerate() {
        let c = c_idx as u16;
        let width = match col {
            ColType::Fixed(FixedCol::Date) => 6.25,
            ColType::Fixed(FixedCol::Weekday) => 5.0,
            ColType::Fixed(FixedCol::Desc) => 11.625,
            ColType::Fixed(FixedCol::Warn) => 5.0,
            ColType::Staff(_) => 13.0,
            ColType::SectionCount(sec) => {
                if sec == "1階" {
                    8.25
                } else {
                    9.875
                }
            }
            ColType::Empty => 4.0,
            ColType::TotalCount => 9.875,
        };
        worksheet.set_column_width(c, width)?;
    }

    // 3. 行1: タイトル行
    let title_text = format!("{}年{}月 勤務シフト表", year, month);
    worksheet.write_string_with_format(0, 0, &title_text, &fmt.title)?;

    // 4. 行2: ヘッダー行
    for (c_idx, col) in layout.columns.iter().enumerate() {
        let c = c_idx as u16;
        match col {
            ColType::Fixed(FixedCol::Date) => {
                worksheet.write_string_with_format(1, c, "日付", &fmt.header_date)?;
            }
            ColType::Fixed(FixedCol::Weekday) => {
                worksheet.write_string_with_format(1, c, "曜日", &fmt.header_date)?;
            }
            ColType::Fixed(FixedCol::Desc) => {
                worksheet.write_string_with_format(1, c, "備考", &fmt.header_date)?;
            }
            ColType::Fixed(FixedCol::Warn) => {
                worksheet.write_string_with_format(1, c, "警告", &fmt.header_warn)?;
            }
            ColType::Staff(staff) => {
                let h_fmt = match staff.section.as_str() {
                    "2階表" => &fmt.header_2f_front,
                    "2階裏" => &fmt.header_2f_back,
                    _ => &fmt.header_1f,
                };
                worksheet.write_string_with_format(1, c, &staff.name, h_fmt)?;
            }
            ColType::SectionCount(_) => {
                worksheet.write_string_with_format(1, c, "人数", &fmt.header_count)?;
            }
            ColType::Empty => {
                worksheet.write_string_with_format(1, c, "", &fmt.data_empty_col)?;
            }
            ColType::TotalCount => {
                worksheet.write_string_with_format(1, c, "総出勤人数", &fmt.header_count)?;
            }
        }
    }

    // 5. 日付データ行 (row = 2 ..= days_in_month + 1)
    let warn_threshold = ctx.parsed.config.warn_2f_back_count; // デフォルト4名

    for d in 1..=month_ctx.days_in_month {
        let row_idx = d + 1; // 0-indexed
        let excel_row = row_idx + 1; // 1-indexed (数式用)
        let date = month_ctx.date_map[&d];
        let weekday_num = month_ctx.weekday_map[&d];
        let is_holiday = month_ctx.is_holiday_map[&d];
        let is_sunday = weekday_num == 6;
        let is_closed = is_sunday || is_holiday;

        // 備考テキスト
        let mut desc = String::new();
        if let Some(h_name) = ctx.holidays.get(&date) {
            desc = h_name.clone();
        } else if let Some(ope) = ctx.parsed.ope_dict.get(&date) {
            desc = ope.clone();
        }
        let is_ope_day = ctx.parsed.ope_dict.contains_key(&date);

        // 2階裏出勤数カウント（警告判定用）
        let mut count_2f_back = 0;
        if !is_closed {
            for col in &layout.columns {
                if let ColType::Staff(ref s) = col {
                    if s.section == "2階裏" {
                        if let Some(shifts) = ctx.result.shifts.get(&s.name) {
                            if shifts.get(&d) == Some(&1) {
                                count_2f_back += 1;
                            }
                        }
                    }
                }
            }
        }

        // 各列セルへの書き込み
        for (c_idx, col) in layout.columns.iter().enumerate() {
            let c = c_idx as u16;
            match col {
                ColType::Fixed(FixedCol::Date) => {
                    let f = if is_closed {
                        &fmt.holiday_date
                    } else {
                        &fmt.data_date
                    };
                    worksheet.write_number_with_format(row_idx, c, d as f64, f)?;
                }
                ColType::Fixed(FixedCol::Weekday) => {
                    let f = if is_closed {
                        &fmt.holiday_weekday
                    } else {
                        &fmt.data_weekday
                    };
                    let w_str = weekday_to_kanji(weekday_num);
                    worksheet.write_string_with_format(row_idx, c, w_str, f)?;
                }
                ColType::Fixed(FixedCol::Desc) => {
                    let f = if is_closed {
                        &fmt.holiday_desc
                    } else {
                        &fmt.data_desc
                    };
                    worksheet.write_string_with_format(row_idx, c, &desc, f)?;
                }
                ColType::Fixed(FixedCol::Warn) => {
                    if is_closed {
                        worksheet.write_string_with_format(row_idx, c, "", &fmt.holiday_warn)?;
                    } else if count_2f_back <= warn_threshold {
                        worksheet.write_string_with_format(row_idx, c, "⚠️", &fmt.data_warn_alert)?;
                    } else {
                        worksheet.write_string_with_format(row_idx, c, "", &fmt.data_warn_empty)?;
                    }
                }
                ColType::Staff(staff) => {
                    if is_closed {
                        worksheet.write_string_with_format(
                            row_idx,
                            c,
                            "休業",
                            &fmt.holiday_staff,
                        )?;
                    } else if staff.vacation_dates.contains(&date) {
                        worksheet.write_string_with_format(
                            row_idx,
                            c,
                            "有休",
                            &fmt.data_vacation,
                        )?;
                    } else if staff.special_vacation_dates.contains(&date) {
                        worksheet.write_string_with_format(
                            row_idx,
                            c,
                            "特有",
                            &fmt.data_sp_vacation,
                        )?;
                    } else {
                        let is_work = ctx
                            .result
                            .shifts
                            .get(&staff.name)
                            .and_then(|shifts| shifts.get(&d))
                            .copied()
                            .unwrap_or(0)
                            == 1;

                        if is_work {
                            let f = if is_ope_day {
                                &fmt.data_work_ope
                            } else {
                                &fmt.data_work_normal
                            };
                            worksheet.write_string_with_format(row_idx, c, "○", f)?;
                        } else {
                            worksheet.write_string_with_format(
                                row_idx,
                                c,
                                "公",
                                &fmt.data_off_public,
                            )?;
                        }
                    }
                }
                ColType::SectionCount(sec_name) => {
                    let f = if is_closed {
                        &fmt.holiday_count
                    } else {
                        &fmt.data_count
                    };

                    let range_opt = match sec_name.as_str() {
                        "2階表" => layout.sec1_range,
                        "2階裏" => layout.sec2_range,
                        "1階" => layout.sec3_range,
                        _ => None,
                    };

                    if let Some((start_c, end_c)) = range_opt {
                        let c1 = col_to_letter(start_c);
                        let c2 = col_to_letter(end_c);
                        let formula = format!("=COUNTIF({}{}:{}{}, \"○\")", c1, excel_row, c2, excel_row);
                        worksheet.write_formula_with_format(row_idx, c, formula.as_str(), f)?;
                    } else {
                        worksheet.write_number_with_format(row_idx, c, 0.0, f)?;
                    }
                }
                ColType::Empty => {
                    let f = if is_closed {
                        &fmt.holiday_empty_col
                    } else {
                        &fmt.data_empty_col
                    };
                    worksheet.write_string_with_format(row_idx, c, "", f)?;
                }
                ColType::TotalCount => {
                    let f = if is_closed {
                        &fmt.holiday_count
                    } else {
                        &fmt.data_count
                    };

                    // セクション人数列の和: =SUM(cnt1, cnt2, cnt3)
                    let mut parts = Vec::new();
                    if let Some(c1) = layout.sec1_count_col {
                        parts.push(format!("{}{}", col_to_letter(c1), excel_row));
                    }
                    if let Some(c2) = layout.sec2_count_col {
                        parts.push(format!("{}{}", col_to_letter(c2), excel_row));
                    }
                    if let Some(c3) = layout.sec3_count_col {
                        parts.push(format!("{}{}", col_to_letter(c3), excel_row));
                    }

                    if parts.is_empty() {
                        worksheet.write_number_with_format(row_idx, c, 0.0, f)?;
                    } else {
                        let formula = format!("=SUM({})", parts.join(", "));
                        worksheet.write_formula_with_format(row_idx, c, formula.as_str(), f)?;
                    }
                }
            }
        }
    }

    // 6. 下部集計4行
    let r_work_days = last_day_row + 1; // 合計出勤日数
    let r_target_days = last_day_row + 2; // 所定勤務日数
    let r_off_days = last_day_row + 3; // 公休日数
    let r_main_off = last_day_row + 4; // 主公休曜日

    let first_data_excel_row = 3; // 1日目の Excel 行
    let last_data_excel_row = last_day_row + 1; // 最終日の Excel 行

    // 列C (備考列) に集計ラベルを右揃え太字で出力
    let c_desc = 2u16;
    worksheet.write_string_with_format(r_work_days, c_desc, "合計出勤日数", &fmt.summary_label_top)?;
    worksheet.write_string_with_format(r_target_days, c_desc, "所定勤務日数", &fmt.summary_label)?;
    worksheet.write_string_with_format(r_off_days, c_desc, "公休日数", &fmt.summary_label)?;
    worksheet.write_string_with_format(r_main_off, c_desc, "主公休曜日", &fmt.summary_label)?;

    // 列A, B, D の下部集計行の空枠線
    for r in [r_work_days, r_target_days, r_off_days, r_main_off] {
        let is_top = r == r_work_days;
        let f = if is_top {
            &fmt.summary_empty_col_top
        } else {
            &fmt.summary_empty_col
        };
        for c in [0u16, 1u16, 3u16] {
            worksheet.write_string_with_format(r, c, "", f)?;
        }
    }

    // 各スタッフ列および人数列の下部集計
    for (c_idx, col) in layout.columns.iter().enumerate() {
        let c = c_idx as u16;
        let col_letter = col_to_letter(c);

        match col {
            ColType::Staff(staff) => {
                // (1) 合計出勤日数: =COUNTIF({col}3:{col}{last}, "○")
                let f_work = format!(
                    "=COUNTIF({0}{1}:{0}{2}, \"○\")",
                    col_letter, first_data_excel_row, last_data_excel_row
                );
                worksheet.write_formula_with_format(r_work_days, c, f_work.as_str(), &fmt.summary_val_top)?;

                // (2) 所定勤務日数: target_days
                let target = ctx
                    .result
                    .target_days
                    .get(&staff.name)
                    .copied()
                    .unwrap_or(0);
                worksheet.write_number_with_format(
                    r_target_days,
                    c,
                    target as f64,
                    &fmt.summary_val,
                )?;

                // (3) 公休日数: =COUNTIF({col}3:{col}{last}, "公")
                let f_off = format!(
                    "=COUNTIF({0}{1}:{0}{2}, \"公\")",
                    col_letter, first_data_excel_row, last_data_excel_row
                );
                worksheet.write_formula_with_format(r_off_days, c, f_off.as_str(), &fmt.summary_val)?;

                // (4) 主公休曜日
                let main_off_str = get_staff_main_off_display(staff, ctx.result);
                worksheet.write_string_with_format(
                    r_main_off,
                    c,
                    &main_off_str,
                    &fmt.summary_text,
                )?;
            }
            ColType::SectionCount(_) | ColType::TotalCount => {
                // 人数列・総出勤人数の合計出勤日数: =SUM({col}3:{col}{last})
                let f_sum = format!(
                    "=SUM({0}{1}:{0}{2})",
                    col_letter, first_data_excel_row, last_data_excel_row
                );
                worksheet.write_formula_with_format(r_work_days, c, f_sum.as_str(), &fmt.summary_val_top)?;

                // 2〜4行目は空欄
                worksheet.write_string_with_format(r_target_days, c, "-", &fmt.summary_text)?;
                worksheet.write_string_with_format(r_off_days, c, "-", &fmt.summary_text)?;
                worksheet.write_string_with_format(r_main_off, c, "-", &fmt.summary_text)?;
            }
            ColType::Empty => {
                worksheet.write_string_with_format(r_work_days, c, "", &fmt.summary_empty_col_top)?;
                worksheet.write_string_with_format(r_target_days, c, "", &fmt.summary_empty_col)?;
                worksheet.write_string_with_format(r_off_days, c, "", &fmt.summary_empty_col)?;
                worksheet.write_string_with_format(r_main_off, c, "", &fmt.summary_empty_col)?;
            }
            ColType::Fixed(_) => {
                // 固定列は上述で処理済み
            }
        }
    }

    Ok(())
}

/// スタッフの主公休曜日表示文字列を計算する
fn get_staff_main_off_display(staff: &Staff, result: &SolveResult) -> String {
    let has_rotating_second_off = staff.is_core
        && staff.pattern.contains("週4")
        && staff.basic_off_days.len() == 1
        && staff.basic_off_type == OffType::And;

    if has_rotating_second_off {
        let first_w = staff.basic_off_days[0];
        let second_w = result.main_offs.get(&staff.name).copied().unwrap_or(99);
        if second_w <= 6 {
            format!("{}・{}", weekday_to_kanji(first_w), weekday_to_kanji(second_w))
        } else {
            weekday_to_kanji(first_w).to_string()
        }
    } else if let Some(&w) = result.main_offs.get(&staff.name) {
        if w <= 6 {
            weekday_to_kanji(w).to_string()
        } else {
            "-".to_string()
        }
    } else if !staff.basic_off_days.is_empty() && staff.basic_off_type == OffType::And {
        let list: Vec<&str> = staff
            .basic_off_days
            .iter()
            .map(|&w| weekday_to_kanji(w))
            .collect();
        list.join("・")
    } else {
        "-".to_string()
    }
}

/// 複数月のシフト結果を単一の現場フォーマット Excel ファイルへ出力する。
///
/// `results`: `[(year, month, SolveResult)]` のリスト
pub fn render_shifts_to_file<P: AsRef<Path>>(
    output_path: P,
    parsed: &ParsedExcelData,
    results: &[(i32, u32, SolveResult)],
    holidays: &HashMap<NaiveDate, String>,
) -> Result<(), XlsxError> {
    let mut workbook = Workbook::new();
    let fmt = FormatSet::new();

    for (year, month, result) in results {
        let sheet_name = if result.success {
            format!("{}年{:02}月シフト", year, month)
        } else {
            format!("{}年{:02}月_未生成", year, month)
        };

        let worksheet = workbook.add_worksheet();
        worksheet.set_name(&sheet_name)?;

        if result.success {
            let ctx = MonthRenderContext {
                parsed,
                result,
                year: *year,
                month: *month,
                holidays,
            };
            render_month_sheet(worksheet, &ctx, &fmt)?;
        } else {
            // 失敗時はエラーサマリーを出力
            worksheet.write_string(0, 0, "シフト自動生成に失敗しました（INFEASIBLE）")?;
            worksheet.write_string(1, 0, format!("対象年月: {}年{}月", year, month))?;
            for (idx, log) in result.logs.iter().enumerate() {
                worksheet.write_string((idx + 3) as u32, 0, log)?;
            }
        }
    }

    workbook.save(output_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_col_to_letter() {
        assert_eq!(col_to_letter(0), "A");
        assert_eq!(col_to_letter(1), "B");
        assert_eq!(col_to_letter(25), "Z");
        assert_eq!(col_to_letter(26), "AA");
        assert_eq!(col_to_letter(27), "AB");
        assert_eq!(col_to_letter(51), "AZ");
        assert_eq!(col_to_letter(52), "BA");
    }

    #[test]
    fn test_weekday_to_kanji() {
        assert_eq!(weekday_to_kanji(0), "月");
        assert_eq!(weekday_to_kanji(1), "火");
        assert_eq!(weekday_to_kanji(5), "土");
        assert_eq!(weekday_to_kanji(6), "日");
    }
}
