//! 勤務条件表 ひな形ファイル生成ツール (`examples/generate_template.rs`)
//!
//! Version: v1.3.0
//! Updated: 2026-11-08 - v1.3.0 2階裏職種判定（看護師/検査）・注射日対応・石橋さん週4固定＆Ope除外・土曜PM有休表記ガイド追加
//! - v1.2.1 (2026-11-05): 標準開発・配布同梱用「勤務条件表_ひな形.xlsx」生成ツール（システム参照の有効・補助・未参照の視覚的区分、入力規則プルダウン、セルコメント、記入ガイドシート完備）
//!
//! 実行方法:
//! `cargo run --example generate_template`

use rust_xlsxwriter::{
    Color, DataValidation, DataValidationErrorStyle, Format, FormatAlign, FormatBorder, Note,
    Workbook, Worksheet, XlsxError,
};

const TEMPLATE_VERSION: &str = "v1.3.0";

fn main() -> Result<(), XlsxError> {
    let out_path = "勤務条件表_ひな形.xlsx";
    let mut workbook = Workbook::new();

    // 1. スタッフマスタ
    {
        let ws = workbook.add_worksheet();
        ws.set_name("スタッフマスタ")?;
        build_staff_sheet(ws)?;
    }

    // 2. シフト制約条件
    {
        let ws = workbook.add_worksheet();
        ws.set_name("シフト制約条件")?;
        build_rules_sheet(ws)?;
    }

    // 3. 手術スケジュール
    {
        let ws = workbook.add_worksheet();
        ws.set_name("手術スケジュール")?;
        build_ope_sheet(ws)?;
    }

    // 4. 前月シフト実績
    {
        let ws = workbook.add_worksheet();
        ws.set_name("前月シフト実績")?;
        build_prev_shift_sheet(ws)?;
    }

    // 5. 設定
    {
        let ws = workbook.add_worksheet();
        ws.set_name("設定")?;
        build_config_sheet(ws)?;
    }

    // 6. 記入ガイド（パーサー非干渉シート名）
    {
        let ws = workbook.add_worksheet();
        ws.set_name("記入ガイド")?;
        build_guide_sheet(ws)?;
    }

    workbook.save(out_path)?;
    println!("ひな形ファイルを生成しました: {} (Version: {})", out_path, TEMPLATE_VERSION);
    Ok(())
}

// ============================================================================
// 共通フォーマット生成ヘルパー
// ============================================================================

/// 【有効】システム直接参照ヘッダー（濃紺 #1F4E78）
fn fmt_header_active() -> Format {
    Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.5)
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x1F4E78))
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter)
        .set_text_wrap()
        .set_border(FormatBorder::Thin)
}

/// 【条件付・補助参照】ヘッダー（青緑 #2B6CB0）
fn fmt_header_cond() -> Format {
    Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.5)
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x2B6CB0))
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter)
        .set_text_wrap()
        .set_border(FormatBorder::Thin)
}

/// 【未参照・現場メモ専用】ヘッダー（グレー #7F7F7F）
fn fmt_header_ignored() -> Format {
    Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.0)
        .set_font_color(Color::White)
        .set_background_color(Color::RGB(0x7F7F7F))
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter)
        .set_text_wrap()
        .set_border(FormatBorder::Thin)
}

/// 有効セル（標準・左寄せ / 中央寄せ）
fn fmt_cell_active(center: bool) -> Format {
    let f = Format::new()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.0)
        .set_align(FormatAlign::VerticalCenter)
        .set_border(FormatBorder::Thin);
    if center {
        f.set_align(FormatAlign::Center)
    } else {
        f
    }
}

/// 除外スタッフ行の有効セル（薄赤グレーで除外中を視覚化）
fn fmt_cell_excluded(center: bool) -> Format {
    let f = Format::new()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.0)
        .set_font_color(Color::RGB(0x808080))
        .set_background_color(Color::RGB(0xFCE4D6))
        .set_align(FormatAlign::VerticalCenter)
        .set_border(FormatBorder::Thin);
    if center {
        f.set_align(FormatAlign::Center)
    } else {
        f
    }
}

/// 条件付・補助参照セル（薄水色 #F0F6FC）
fn fmt_cell_cond(center: bool) -> Format {
    let f = Format::new()
        .set_font_name("Yu Gothic UI")
        .set_font_size(10.0)
        .set_background_color(Color::RGB(0xF0F6FC))
        .set_align(FormatAlign::VerticalCenter)
        .set_border(FormatBorder::Thin);
    if center {
        f.set_align(FormatAlign::Center)
    } else {
        f
    }
}

/// 未参照・メモ専用セル（薄グレー #F2F2F2、文字 #595959）
fn fmt_cell_ignored(center: bool) -> Format {
    let f = Format::new()
        .set_font_name("Yu Gothic UI")
        .set_font_size(9.5)
        .set_font_color(Color::RGB(0x595959))
        .set_background_color(Color::RGB(0xF2F2F2))
        .set_align(FormatAlign::VerticalCenter)
        .set_border(FormatBorder::Thin);
    if center {
        f.set_align(FormatAlign::Center)
    } else {
        f
    }
}

fn make_note(text: &str) -> Note {
    Note::new(text)
        .set_author("シフト自動生成システム")
        .set_width(320)
        .set_height(140)
}

// ============================================================================
// 1. スタッフマスタ シート構築
// ============================================================================
fn build_staff_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_freeze_panes(1, 1)?;
    ws.set_row_height(0, 32.0)?;

    let h_act = fmt_header_active();
    let h_cond = fmt_header_cond();
    let h_ign = fmt_header_ignored();

    // 列構成: 左側(A〜K)にシステム有効列を集約、Lに条件付補完列、右側(M〜P)に未参照メモ列を配置
    let headers: &[(&str, &Format, f64, &str)] = &[
        (
            "氏名",
            &h_act,
            13.0,
            "【有効・必須】\nシフト表の列名および「シフト制約条件」「前月シフト実績」との名寄せキーです。\n※括弧の半角/全角（例: 鈴木(孝)）も他シートと完全一致させてください。",
        ),
        (
            "所属",
            &h_act,
            10.0,
            "【有効・必須】\n選択肢: 2階表 / 2階裏 / 1階\nセクション別の人数平準化・2階裏最低人数・出力表の列グループ分けに使用されます。",
        ),
        (
            "職種",
            &h_act,
            10.0,
            "【有効（「勤務形態」列名でも認識）】\n・「看護師」（または Ns）＝看護師カウント対象\n・「スタッフ」（または 検査 / ORT）＝検査スタッフカウント対象\n2階裏のOpe日・火曜・木曜の職種別最低/推奨人数判定に使用されます。",
        ),
        (
            "雇用形態",
            &h_act,
            12.0,
            "【有効・必須】\n・「正社員」「パート社員」＝コアスタッフ（月間所定日数・公休ローテーション対象）\n・「パート」等＝非コア（希望公休曜日以外の診療日に固定出勤）",
        ),
        (
            "基本勤務パターン",
            &h_act,
            26.0,
            "【有効・必須】\n・週5日 / 週4日 / 週4日（必要に応じて週5日可） / 週3日 / 除外\n※数字の「4」「5」は必ず半角で入力してください（全角「週４日」は不可）。",
        ),
        (
            "希望公休曜日",
            &h_act,
            16.0,
            "【有効】\n・空欄(コア): 平日(月〜金)から主公休を月替わり自動ローテーション\n・単一/複数固定(AND): 「火」「月,金,土,日」\n・選択式(OR): 必ず「もしくは / または / or」を含める（例: 水,もしくは木）\n※「水・木」「水/木」だけだと両方休み(AND)になります。",
        ),
        (
            "特別有休（指定日）",
            &h_act,
            28.0,
            "【有効】\n強制休み（出力表記: 特有）＋所定勤務日数から控除。\n※コアスタッフ全員に共通する日付（年末年始等）はクリニック休診日として扱われます。\n※期間指定は終了日にも月を含めてください（⭕ 11/17-11/21 / ❌ 11/17-21）。",
        ),
        (
            "有休（指定日）",
            &h_act,
            18.0,
            "【有効】\n強制休み（出力表記: 有休）＋所定勤務日数から控除。\nカンマ「,」区切りや範囲「2026/11/17-2026/11/21」で複数指定可能。\n※土曜午後の半休は「9/12(PM)」「9/12午後」と記入すると土曜2階裏の実質2人勤務例外が適用されます。",
        ),
        (
            "希望休（指定日）",
            &h_act,
            18.0,
            "【有効】\n有休を消費せず、通常の公休「公」を特定日に割り当てたい場合に入力します（所定勤務日数は減りません）。",
        ),
        (
            "指定公休日",
            &h_act,
            16.0,
            "【有効】\n強制公休（出力表記: 公）として固定し、さらに同じISO週の別の主公休取得義務を免除します（所定勤務日数の上限計算にも考慮）。",
        ),
        (
            "除外フラグ",
            &h_act,
            11.0,
            "【有効】\nTrue（または 1 / ○）にするとシフト作成対象から完全除外し、グループ制約の必要人数も自動で減算します。",
        ),
        (
            "希望出勤曜日(パート補完)",
            &h_cond,
            20.0,
            "【条件付き参照（パートのみ）】\n雇用形態が「パート」等で「希望公休曜日」が空欄の時のみ、出勤曜日の逆算に使われます。\n⚠️ 正社員に「月曜出勤必須」等と書いても無視されます（必ず「シフト制約条件」シートで設定してください）。",
        ),
        (
            "勤務開始時刻(未参照)",
            &h_ign,
            15.0,
            "【システム未参照（現場メモ専用）】\n日単位シフト最適化のため、この列の時刻は計算結果に影響しません。現場管理メモとしてご活用ください。",
        ),
        (
            "勤務終了時刻(未参照)",
            &h_ign,
            15.0,
            "【システム未参照（現場メモ専用）】\n日単位シフト最適化のため、この列の時刻は計算結果に影響しません。",
        ),
        (
            "勤務タイプ(未参照)",
            &h_ign,
            14.0,
            "【システム未参照（現場メモ専用）】\n「通常」「時短」等の区分は計算結果に影響しません。",
        ),
        (
            "備考(未参照)",
            &h_ign,
            28.0,
            "【システム未参照（現場メモ専用）】\n制約シートとの連動メモや注意事項などを自由に記入できます（シフト計算には影響しません）。",
        ),
    ];

    for (col, (title, fmt, width, note_txt)) in headers.iter().enumerate() {
        let c = col as u16;
        ws.set_column_width(c, *width)?;
        ws.write_string_with_format(0, c, *title, fmt)?;
        ws.insert_note(0, c, &make_note(note_txt))?;
    }

    // (氏名, 所属, 職種, 雇用形態, パターン, 希望公休, 特別有休, 有休, 希望休, 指定公休, 除外, 希望出勤(パート補完), 開始, 終了, タイプ, 備考)
    #[allow(clippy::type_complexity)]
    let rows: &[(&str, &str, &str, &str, &str, &str, &str, &str, &str, &str, bool, &str, &str, &str, &str, &str)] = &[
        ("榊原", "2階表", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("中澤", "2階表", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("田中島", "2階表", "ORT", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "月曜出勤必須は[シフト制約条件]シートで設定"),
        ("梶原", "2階表", "ORT", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "月曜出勤必須は[シフト制約条件]シートで設定"),
        ("鈴木(孝)", "2階表", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "氏名の半角括弧を制約シートと統一"),
        ("小田切", "2階表", "スタッフ", "正社員", "週5日", "月", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("川端", "2階表", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("宮澤", "2階表", "スタッフ", "正社員", "週4日", "火", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "火曜固定＋第2公休自動ローテーション"),
        ("高橋", "2階表", "スタッフ", "正社員", "週5日", "土", "2026/12/29-2027/01/04", "2026-11-10", "", "", true, "", "", "17:30", "通常", "除外フラグ=Trueにより計算・制約から自動除外"),
        ("板垣", "2階表", "スタッフ", "パート社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "9:00", "17:00", "通常", ""),
        ("並木", "2階表", "スタッフ", "パート社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "9:00", "17:00", "通常", ""),
        ("森川", "2階表", "スタッフ", "正社員", "除外", "", "2026/12/29-2027/01/04", "", "", "", true, "", "", "", "通常", "除外中"),
        ("長谷川", "2階裏", "スタッフ", "正社員", "週5日", "火", "2026/11/9,2026/11/10,2026/12/29-2027/01/04", "", "", "", false, "", "", "", "時短", "16:15早上がり・石橋さんと外来重複時はフルタイム+1増員"),
        ("小島", "2階裏", "スタッフ", "正社員", "週5日", "", "2026/11/17-2026/11/21,2026/12/29-2027/01/04", "", "", "2026/11/16", false, "", "", "", "通常", ""),
        ("堀", "2階裏", "看護師", "正社員", "週4日（必要に応じて週5日可）", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "看護師カウント対象・必要時週5可"),
        ("江原", "2階裏", "看護師", "正社員", "週4日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "看護師カウント対象"),
        ("石橋", "2階裏", "看護師", "パート社員", "週4日", "土", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", "週4固定(祝日非連動)・Ope/注射必須・非Ope週水曜公休・Ope日外来除外"),
        ("鈴木", "1階", "スタッフ", "パート", "週3日", "月,金,土,日", "2026/12/29-2027/01/04", "", "", "", false, "火・水・木", "", "", "通常", "パート固定曜日出勤"),
        ("後藤", "1階", "スタッフ", "パート", "週3日", "火,水,木,日", "2026/12/29-2027/01/04", "", "", "", false, "月・金・土", "", "", "通常", "パート固定曜日出勤"),
        ("岡本", "1階", "スタッフ", "パート", "週3日", "火,水,土,日", "2026/12/29-2027/01/04", "", "", "", false, "月・木・金", "", "", "通常", "パート固定曜日出勤"),
        ("石渡", "1階", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("今川", "1階", "スタッフ", "正社員", "週5日", "土", "2026/12/29-2027/01/04", "", "", "", false, "", "8:30", "18:00", "通常", ""),
        ("野田", "1階", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("岸", "1階", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "時短", ""),
        ("鹿島", "1階", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("分部", "1階", "スタッフ", "正社員", "週5日", "", "2026/12/29-2027/01/04", "", "", "", false, "", "", "", "通常", ""),
        ("太田", "1階", "スタッフ", "正社員", "週5日", "水,もしくは木", "2026/12/29-2027/01/04", "", "", "", false, "", "", "17:30", "通常", "OR選択公休（水または木のいずれか1日）"),
    ];

    let f_act_l = fmt_cell_active(false);
    let f_act_c = fmt_cell_active(true);
    let f_ex_l = fmt_cell_excluded(false);
    let f_ex_c = fmt_cell_excluded(true);
    let f_cond_c = fmt_cell_cond(true);
    let f_ign_c = fmt_cell_ignored(true);
    let f_ign_l = fmt_cell_ignored(false);

    for (idx, r) in rows.iter().enumerate() {
        let row_num = (idx + 1) as u32;
        ws.set_row_height(row_num, 22.0)?;
        let is_ex = r.10 || r.4 == "除外";
        let fl = if is_ex { &f_ex_l } else { &f_act_l };
        let fc = if is_ex { &f_ex_c } else { &f_act_c };

        ws.write_string_with_format(row_num, 0, r.0, fl)?;
        ws.write_string_with_format(row_num, 1, r.1, fc)?;
        ws.write_string_with_format(row_num, 2, r.2, fc)?;
        ws.write_string_with_format(row_num, 3, r.3, fc)?;
        ws.write_string_with_format(row_num, 4, r.4, fl)?;
        ws.write_string_with_format(row_num, 5, r.5, fc)?;
        ws.write_string_with_format(row_num, 6, r.6, fl)?;
        ws.write_string_with_format(row_num, 7, r.7, fl)?;
        ws.write_string_with_format(row_num, 8, r.8, fl)?;
        ws.write_string_with_format(row_num, 9, r.9, fl)?;
        ws.write_boolean_with_format(row_num, 10, r.10, fc)?;
        ws.write_string_with_format(row_num, 11, r.11, &f_cond_c)?;
        ws.write_string_with_format(row_num, 12, r.12, &f_ign_c)?;
        ws.write_string_with_format(row_num, 13, r.13, &f_ign_c)?;
        ws.write_string_with_format(row_num, 14, r.14, &f_ign_c)?;
        ws.write_string_with_format(row_num, 15, r.15, &f_ign_l)?;
    }

    // 入力規則（プルダウン: 50行目まで設定し新規スタッフ追加にも対応）
    let dv_sec = DataValidation::new()
        .allow_list_strings(&["2階表", "2階裏", "1階"])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(1, 1, 50, 1, &dv_sec)?;

    let dv_role = DataValidation::new()
        .allow_list_strings(&["スタッフ", "看護師", "ORT"])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 2, 50, 2, &dv_role)?;

    let dv_emp = DataValidation::new()
        .allow_list_strings(&["正社員", "パート社員", "パート"])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(1, 3, 50, 3, &dv_emp)?;

    let dv_pat = DataValidation::new()
        .allow_list_strings(&[
            "週5日",
            "週4日",
            "週4日（必要に応じて週5日可）",
            "週3日",
            "除外",
        ])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 4, 50, 4, &dv_pat)?;

    let dv_bool = DataValidation::new()
        .allow_list_strings(&["FALSE", "TRUE"])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 10, 50, 10, &dv_bool)?;

    Ok(())
}

// ============================================================================
// 2. シフト制約条件 シート構築
// ============================================================================
fn build_rules_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_freeze_panes(1, 0)?;
    ws.set_row_height(0, 32.0)?;

    let h_act = fmt_header_active();
    let h_cond = fmt_header_cond();
    let h_ign = fmt_header_ignored();

    let headers: &[(&str, &Format, f64, &str)] = &[
        (
            "所属",
            &h_cond,
            12.0,
            "【補助参照（ラベル・除外判定）】\nルールの識別名として使用されます（※ここに「1階」と書いても1階全員が対象になるわけではなく、対象者はD列「対象スタッフ」で決まります）。\n※「小西」を含む行は外部イベントとして自動除外されます。",
        ),
        (
            "適用条件",
            &h_act,
            26.0,
            "【有効・必須（定型キーワード判定）】\n⭕ 使える表現:\n・全日\n・月曜 / 火曜 / 水曜 / 木曜 / 金曜 / 土曜（※「曜」必須）\n・手術のある日 / 日曜日以外の手術のある日\n・火曜(OPE日) / 水曜(OPE日)\n・「Vit」手術日 / 「下垂」手術日 / 「cat」手術日 / 「gla」手術日（※新術式は必ず鉤括弧「 」で囲む）\n\n❌ 注意:\n・1行に複数曜日（火曜・水曜）は不可→行を分ける\n・未知の平文を書くと「全日」扱いになるため注意！",
        ),
        (
            "制限タイプ",
            &h_act,
            18.0,
            "【有効】\n・「同時休み不可」: 「同時休み」を含む場合、max(最小出勤数, 人数 - 最小出勤数) を最低出勤人数として強制します。\n・「グループ出勤必須」「出勤必須」「出勤人数指定」: 最小・推奨・最大出勤数の数値通りに動作します。",
        ),
        (
            "対象スタッフ",
            &h_act,
            26.0,
            "【有効・必須】\nスタッフマスタA列の氏名と完全一致させてください（カンマ「,」区切り）。\n※除外フラグ=Trueのスタッフが含まれていても、自動で必要人数から減算されるため修正不要です。",
        ),
        (
            "最小出勤数",
            &h_act,
            12.0,
            "【有効】\n対象スタッフのうち最低限出勤すべき人数。\n制約レベルが HARD 系の場合は Stage 1 で絶対厳守されます（全員有休等の物理不能日のみ自動クランプ）。",
        ),
        (
            "推奨出勤数",
            &h_act,
            12.0,
            "【有効】\n可能な限り確保したい理想の出勤人数（空欄なら適用なし）。\n例: 金曜 HIGH1 で最小2・推奨3にすると、主公休をスライドしてでも3名全員出勤を最優先します。",
        ),
        (
            "最大出勤数",
            &h_act,
            12.0,
            "【有効（任意）】\n同時出勤の上限人数を制限したい場合に入力します（空欄なら上限なし）。",
        ),
        (
            "制約レベル",
            &h_act,
            13.0,
            "【有効・必須】\n優先度順（高→低）:\nHARD1 > HARD(HARD2) > HARD3 > HIGH1 > HIGH2(HIGH) > HIGH3 > SOFT1 > SOFT(SOFT2) > SOFT3 > LOW",
        ),
        (
            "備考(未参照)",
            &h_ign,
            34.0,
            "【システム未参照（現場メモ専用）】\nルールの意図や背景メモを自由に記載できます（シフト計算には影響しません）。",
        ),
    ];

    for (col, (title, fmt, width, note_txt)) in headers.iter().enumerate() {
        let c = col as u16;
        ws.set_column_width(c, *width)?;
        ws.write_string_with_format(0, c, *title, fmt)?;
        ws.insert_note(0, c, &make_note(note_txt))?;
    }

    #[allow(clippy::type_complexity)]
    let rules: &[(&str, &str, &str, &str, i32, Option<i32>, Option<i32>, &str, &str)] = &[
        ("1階", "「Vit」手術日", "同時休み不可", "石渡,岸", 1, None, None, "HARD1", "硝子体(Vit)手術日は石渡・岸の最低1名出勤必須（最優先）"),
        ("1階", "「下垂」手術日", "同時休み不可", "石渡,岸,今川", 2, None, None, "HARD", "眼瞼下垂手術日は3名中2名以上出勤必須"),
        ("2階表", "全日", "グループ出勤必須", "小田切,鈴木(孝),榊原", 1, None, None, "HARD", "毎日最低1名は出勤"),
        ("2階表", "全日", "グループ出勤必須", "田中島,梶原,榊原", 1, None, None, "HARD", "毎日最低1名は出勤"),
        ("2階表", "全日", "グループ出勤必須", "中澤,川端", 1, None, None, "HARD", "毎日最低1名は出勤"),
        ("2階表", "全日", "グループ出勤必須", "宮澤,高橋", 1, None, None, "HARD", "※高橋除外時は宮澤の勤務日のみ適用（休日は自動クランプ）"),
        ("2階表", "月曜", "出勤必須", "田中島,梶原", 2, None, None, "HARD", "ORT2名は毎週月曜そろって出勤必須"),
        ("2階表", "火曜(OPE日)", "出勤人数指定", "小田切,鈴木(孝),榊原", 2, Some(3), None, "HIGH2", "手術のある火曜は最低2名・推奨3名（金曜HIGH1の次に優先）"),
        ("2階表", "水曜(OPE日)", "出勤人数指定", "小田切,鈴木(孝),榊原", 2, Some(3), None, "HIGH2", "手術のある水曜は最低2名・推奨3名（金曜HIGH1の次に優先）"),
        ("2階表", "金曜", "出勤人数指定", "小田切,鈴木(孝),榊原", 2, Some(3), None, "HIGH1", "毎週金曜は最低2名・できる限り3名全員出勤（火水OPEより優先）"),
        ("2階裏", "日曜日以外の手術のある日", "出勤人数指定", "江原,堀", 1, Some(2), None, "SOFT", "手術日は看護師(江原・堀)最低1名・推奨2名"),
        ("2階裏", "日曜日以外の手術のある日", "出勤必須", "石橋", 1, None, None, "HARD", "土曜以外の手術日は石橋出勤必須（土曜固定休の日は自動免除）"),
    ];

    let f_cond_c = fmt_cell_cond(true);
    let f_act_l = fmt_cell_active(false);
    let f_act_c = fmt_cell_active(true);
    let f_ign_l = fmt_cell_ignored(false);

    for (idx, r) in rules.iter().enumerate() {
        let row_num = (idx + 1) as u32;
        ws.set_row_height(row_num, 22.0)?;
        ws.write_string_with_format(row_num, 0, r.0, &f_cond_c)?;
        ws.write_string_with_format(row_num, 1, r.1, &f_act_l)?;
        ws.write_string_with_format(row_num, 2, r.2, &f_act_c)?;
        ws.write_string_with_format(row_num, 3, r.3, &f_act_l)?;
        ws.write_number_with_format(row_num, 4, r.4 as f64, &f_act_c)?;
        if let Some(rec) = r.5 {
            ws.write_number_with_format(row_num, 5, rec as f64, &f_act_c)?;
        } else {
            ws.write_blank(row_num, 5, &f_act_c)?;
        }
        if let Some(max_v) = r.6 {
            ws.write_number_with_format(row_num, 6, max_v as f64, &f_act_c)?;
        } else {
            ws.write_blank(row_num, 6, &f_act_c)?;
        }
        ws.write_string_with_format(row_num, 7, r.7, &f_act_c)?;
        ws.write_string_with_format(row_num, 8, r.8, &f_ign_l)?;
    }

    // 入力規則（代表例のプルダウン、Information モードなので自由な「○○」手術日も手入力可能）
    let dv_cond = DataValidation::new()
        .allow_list_strings(&[
            "全日",
            "月曜",
            "火曜",
            "水曜",
            "木曜",
            "金曜",
            "土曜",
            "日曜日以外の手術のある日",
            "火曜(OPE日)",
            "水曜(OPE日)",
            "「Vit」手術日",
            "「下垂」手術日",
            "「cat」手術日",
            "「gla」手術日",
        ])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 1, 40, 1, &dv_cond)?;

    let dv_type = DataValidation::new()
        .allow_list_strings(&[
            "同時休み不可",
            "グループ出勤必須",
            "出勤必須",
            "出勤人数指定",
        ])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 2, 40, 2, &dv_type)?;

    let dv_level = DataValidation::new()
        .allow_list_strings(&[
            "HARD1", "HARD", "HARD3", "HIGH1", "HIGH2", "HIGH3", "SOFT1", "SOFT", "SOFT3", "LOW",
        ])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(1, 7, 40, 7, &dv_level)?;

    Ok(())
}

// ============================================================================
// 3. 手術スケジュール シート構築
// ============================================================================
fn build_ope_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_freeze_panes(1, 0)?;
    ws.set_row_height(0, 32.0)?;

    let h_act = fmt_header_active();
    let h_cond = fmt_header_cond();
    let h_ign = fmt_header_ignored();

    let headers: &[(&str, &Format, f64, &str)] = &[
        (
            "日付",
            &h_act,
            15.0,
            "【有効・必須（生成対象月の決定キー）】\n手術日を指定します。さらに、ここに記載された日付の「年月」がシフト自動生成の対象月一覧として自動認識されます。\n※手術が0件の月もシフトを作りたい場合は、その月の日付を1行（手術種別は空欄可）入力してください。",
        ),
        (
            "手術種別",
            &h_act,
            18.0,
            "【有効】\nVit / cat / gla / 下垂 / 注射 などを記入（カンマ区切り複数可。同一日の複数行も自動結合）。\n・「注射」以外のオペがある日＝「Ope日」（石橋さん必須＆2階裏外来カウント除外、2階裏外来3名以上必須）\n・「注射」がある日＝「注射日」（石橋さん必須出勤）",
        ),
        (
            "作成対象",
            &h_cond,
            12.0,
            "【補助参照（GUI初期選択用）】\n・全行空欄の場合: 登場するすべての年月を生成対象として初期選択します。\n・特定の月の行に「○」を付けた場合: GUI起動時に「○」が付いた月だけチェックONの状態にします。",
        ),
        (
            "担当医(未参照)",
            &h_ign,
            15.0,
            "【システム未参照（現場メモ専用）】\n担当医名はシフト計算および出力表には反映されません。スケジュール管理メモとしてご利用ください。",
        ),
        (
            "備考(未参照)",
            &h_ign,
            24.0,
            "【システム未参照（現場メモ専用）】\n自由記入欄です（計算結果には影響しません）。",
        ),
    ];

    for (col, (title, fmt, width, note_txt)) in headers.iter().enumerate() {
        let c = col as u16;
        ws.set_column_width(c, *width)?;
        ws.write_string_with_format(0, c, *title, fmt)?;
        ws.insert_note(0, c, &make_note(note_txt))?;
    }

    let opes: &[(&str, &str, &str)] = &[
        ("2026-09-02", "Vit,cat", "重城"),
        ("2026-09-04", "cat", "恩田"),
        ("2026-09-08", "cat", "恩田"),
        ("2026-09-11", "cat", "恩田"),
        ("2026-09-13", "gla", "相原"),
        ("2026-09-16", "下垂", "宮田"),
        ("2026-09-25", "Vit,cat", "真下"),
        ("2026-10-04", "gla", "相原"),
        ("2026-10-09", "cat", "恩田"),
        ("2026-10-14", "Vit,cat", "重城"),
        ("2026-10-16", "cat", "恩田"),
        ("2026-10-23", "Vit,cat", "真下"),
        ("2026-10-27", "cat", "恩田"),
        ("2026-11-04", "Vit,cat", "重城"),
        ("2026-11-06", "cat", "恩田"),
        ("2026-11-13", "cat", "恩田"),
        ("2026-11-18", "cat", "恩田"),
        ("2026-11-20", "cat", "恩田"),
        ("2026-12-02", "Vit,cat", "重城"),
        ("2026-12-04", "cat", "恩田"),
        ("2026-12-11", "cat", "恩田"),
        ("2026-12-15", "Vit,cat", "真下"),
        ("2026-12-18", "cat", "恩田"),
        ("2027-01-06", "Vit,cat", "重城"),
        ("2027-01-08", "cat", "恩田"),
        ("2027-01-13", "下垂", "宮田"),
        ("2027-01-15", "cat", "恩田"),
        ("2027-01-26", "cat", "恩田"),
        ("2027-01-29", "Vit,cat", "真下"),
        ("2027-01-31", "gla", "相原"),
        ("2027-02-03", "Vit,cat", "重城"),
        ("2027-02-05", "cat", "恩田"),
        ("2027-02-12", "cat", "恩田"),
        ("2027-02-19", "cat", "恩田"),
        ("2027-02-21", "gla", "相原"),
        ("2027-02-26", "Vit,cat", "真下"),
        ("2027-03-05", "cat", "恩田"),
        ("2027-03-07", "gla", "相原"),
        ("2027-03-09", "cat", "恩田"),
        ("2027-03-12", "Vit,cat", "真下"),
        ("2027-03-26", "cat", "恩田"),
    ];

    let f_act_c = fmt_cell_active(true);
    let f_cond_c = fmt_cell_cond(true);
    let f_ign_c = fmt_cell_ignored(true);
    let f_ign_l = fmt_cell_ignored(false);

    for (idx, &(dt, kind, doc)) in opes.iter().enumerate() {
        let row_num = (idx + 1) as u32;
        ws.set_row_height(row_num, 20.0)?;
        ws.write_string_with_format(row_num, 0, dt, &f_act_c)?;
        ws.write_string_with_format(row_num, 1, kind, &f_act_c)?;
        ws.write_blank(row_num, 2, &f_cond_c)?;
        ws.write_string_with_format(row_num, 3, doc, &f_ign_c)?;
        ws.write_blank(row_num, 4, &f_ign_l)?;
    }

    let dv_mark = DataValidation::new()
        .allow_list_strings(&["○"])?
        .set_error_style(DataValidationErrorStyle::Information);
    ws.add_data_validation(1, 2, 100, 2, &dv_mark)?;

    Ok(())
}

// ============================================================================
// 4. 前月シフト実績 シート構築（出力シフト表のそのままコピペに対応しつつ参照箇所を明示）
// ============================================================================
fn build_prev_shift_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_freeze_panes(2, 4)?;
    ws.set_row_height(0, 28.0)?;
    ws.set_row_height(1, 26.0)?;

    let fmt_title = Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(12.0)
        .set_font_color(Color::RGB(0x1F4E78))
        .set_align(FormatAlign::VerticalCenter);

    ws.write_string_with_format(0, 0, "2026年12月 勤務シフト表", &fmt_title)?;
    ws.insert_note(
        0,
        0,
        &make_note(
            "【有効・年月判定キー】\n本アプリで出力した前月シフトシートをそのまま丸ごとコピー＆ペーストして使えます。\n\n■ システムが実際に読み込む箇所（濃色表示）:\n1. A1セルの年月（例: 2026年12月 → 翌月2027年01月を単発/先頭生成する時のみ有効化）\n2. 2行目のスタッフ名\n3. 月末7日分（25日〜31日）の「○」有無（月跨ぎ連勤上限チェック用）\n4. 最下段「主公休曜日」行（主公休ローテーション用）\n\n■ 未参照箇所（グレー表示）:\n・1日〜24日の行、および合計出勤日数・所定勤務日数・公休日数・人数集計列は計算に使いません。",
        ),
    )?;

    let csv_lines: &[&str] = &[
        "日付,曜日,備考,警告,榊原,中澤,田中島,梶原,鈴木(孝),小田切,川端,宮澤,高橋,板垣,人数,,長谷川,小島,堀,江原,石橋,人数,,鈴木,後藤,岡本,石渡,今川,野田,岸,鹿島,分部,太田,人数,総出勤人数",
        "1,火,,,○,○,○,公,○,○,○,公,○,○,8,,公,○,○,○,○,4,,○,公,公,○,○,○,公,○,○,○,7,19",
        "2,水,Vit/cat,,公,公,○,○,○,○,○,公,○,○,7,,○,公,公,○,○,3,,○,公,公,○,○,○,○,○,○,公,7,17",
        "3,木,,,○,○,○,○,公,○,公,○,○,公,7,,○,○,○,公,○,4,,○,公,○,公,○,○,○,○,公,○,7,18",
        "4,金,cat,,○,○,公,○,○,○,○,○,○,○,9,,○,○,○,○,公,4,,公,○,○,○,○,公,○,公,○,○,7,20",
        "5,土,,,○,○,○,○,○,○,○,○,公,○,9,,○,○,○,○,公,4,,公,○,公,○,公,○,○,○,○,○,7,20",
        "6,日,,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,0",
        "7,月,,,○,○,○,○,○,公,○,○,○,○,9,,○,○,○,公,○,4,,公,○,○,○,○,○,○,○,○,○,9,22",
        "8,火,,,○,○,○,公,○,○,○,公,○,○,8,,公,○,○,○,○,4,,○,公,公,○,○,○,公,○,○,○,7,19",
        "9,水,,,公,公,○,○,○,○,○,公,○,○,7,,○,公,公,○,○,3,,○,公,公,○,○,○,○,○,○,公,7,17",
        "10,木,,,○,○,○,○,公,○,公,○,○,公,7,,○,○,○,公,○,4,,○,公,○,公,○,○,○,○,公,○,7,18",
        "11,金,cat,,○,○,公,○,○,○,○,○,○,○,9,,○,○,○,○,公,4,,公,○,○,○,○,公,○,公,○,○,7,20",
        "12,土,,,○,○,○,○,○,○,○,○,公,○,9,,○,○,○,○,公,4,,公,○,公,○,公,○,○,○,○,○,7,20",
        "13,日,,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,0",
        "14,月,,,○,○,○,○,○,公,○,○,○,○,9,,○,○,○,公,○,4,,公,○,○,○,○,○,○,○,○,○,9,22",
        "15,火,Vit/cat,,○,○,○,公,○,○,○,公,○,○,8,,公,○,○,○,○,4,,○,公,公,○,○,○,公,○,○,○,7,19",
        "16,水,,,公,公,○,○,○,○,○,公,○,○,7,,○,公,公,○,○,3,,○,公,公,○,○,○,○,○,○,公,7,17",
        "17,木,,,○,○,○,○,公,○,公,○,○,公,7,,○,○,○,公,○,4,,○,公,○,公,○,○,○,○,公,○,7,18",
        "18,金,cat,,○,○,公,○,○,○,○,○,○,○,9,,○,○,○,○,公,4,,公,○,○,○,○,公,○,公,○,○,7,20",
        "19,土,,,○,○,○,○,○,○,○,○,公,○,9,,○,○,○,○,公,4,,公,○,公,○,公,○,○,○,○,○,7,20",
        "20,日,,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,0",
        "21,月,,,○,○,○,○,○,公,○,○,○,○,9,,○,○,○,公,○,4,,公,○,○,○,○,○,○,○,○,○,9,22",
        "22,火,,,○,○,○,公,○,○,○,公,○,○,8,,公,○,○,○,○,4,,○,公,公,○,○,○,公,○,○,○,7,19",
        "23,水,,,公,公,○,○,○,○,○,公,○,○,7,,○,公,公,○,○,3,,○,公,公,○,○,○,○,○,○,公,7,17",
        "24,木,,,○,○,○,○,公,○,公,○,○,公,7,,○,○,○,公,○,4,,○,公,○,公,○,○,○,○,公,○,7,18",
        "25,金,,,○,○,公,○,○,○,○,○,○,○,9,,○,○,○,○,公,4,,公,○,○,○,○,公,○,公,○,○,7,20",
        "26,土,,,○,○,○,○,○,○,○,○,公,○,9,,○,○,○,○,公,4,,公,○,公,○,公,○,○,○,○,○,7,20",
        "27,日,,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,0,,休業,休業,休業,休業,休業,休業,休業,休業,休業,休業,0,0",
        "28,月,,,○,○,○,○,○,公,○,○,○,○,9,,○,○,○,公,○,4,,公,○,○,○,○,○,○,○,○,○,9,22",
        "29,火,,⚠️,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,0",
        "30,水,,⚠️,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,0",
        "31,木,,⚠️,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,0,,特有,特有,特有,特有,特有,特有,特有,特有,特有,特有,0,0",
        ",,合計出勤日数,,20,20,20,20,20,20,20,16,20,20,196,,20,20,20,16,16,92,,12,12,12,20,20,20,20,20,20,20,176,464",
        ",,所定勤務日数,,20,20,20,20,20,20,20,16,20,20,-,,20,20,16,16,16,-,,12,12,12,20,20,20,20,20,20,20,-,-",
        ",,公休日数,,4,4,4,4,4,4,4,8,4,4,-,,4,4,4,8,8,-,,12,12,12,4,4,4,4,4,4,4,-,-",
        ",,主公休曜日,,水,水,金,火,木,火,木,火・水,月,木,-,,月,水,水,木,土・金,-,,月・金・土・日,火・水・木・日,火・水・土・日,木,月,金,火,金,木,水,-,-",
    ];

    let h_act = fmt_header_active();
    let h_ign = fmt_header_ignored();
    let f_act_c = fmt_cell_active(true);
    let f_ign_c = fmt_cell_ignored(true);

    let header_cols: Vec<&str> = csv_lines[0].split(',').collect();
    let mut ignored_col_indices = Vec::new();
    for (c_idx, &col_title) in header_cols.iter().enumerate() {
        let col = c_idx as u16;
        let is_ign_col = col_title.is_empty()
            || col_title == "警告"
            || col_title.contains("人数");
        if is_ign_col {
            ignored_col_indices.push(c_idx);
        }
        let width = if col_title.is_empty() {
            3.5
        } else if col_title == "備考" {
            12.0
        } else if c_idx <= 3 {
            6.5
        } else {
            9.5
        };
        ws.set_column_width(col, width)?;
        if !col_title.is_empty() {
            let fmt = if is_ign_col { &h_ign } else { &h_act };
            ws.write_string_with_format(1, col, col_title, fmt)?;
        }
    }

    for (line_idx, &line) in csv_lines.iter().enumerate().skip(1) {
        let excel_row = (line_idx + 1) as u32;
        ws.set_row_height(excel_row, 19.0)?;
        let cells: Vec<&str> = line.split(',').collect();

        // 参照対象行か判定: 日付 25〜31 または 主公休曜日 行のみシステムが参照
        let day_num = cells.first().and_then(|s| s.parse::<u32>().ok());
        let is_main_off_row = cells.get(2).copied() == Some("主公休曜日");
        let is_active_row = matches!(day_num, Some(25..=31)) || is_main_off_row;

        for (c_idx, &val) in cells.iter().enumerate() {
            if header_cols.get(c_idx).copied() == Some("") && val.is_empty() {
                continue;
            }
            let col = c_idx as u16;
            let use_active_fmt = is_active_row && !ignored_col_indices.contains(&c_idx) && c_idx != 1 && c_idx != 3;
            let fmt = if use_active_fmt { &f_act_c } else { &f_ign_c };
            if let Ok(num) = val.parse::<i32>() {
                ws.write_number_with_format(excel_row, col, num as f64, fmt)?;
            } else {
                ws.write_string_with_format(excel_row, col, val, fmt)?;
            }
        }
    }

    Ok(())
}

// ============================================================================
// 5. 設定 シート構築
// ============================================================================
fn build_config_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_freeze_panes(1, 0)?;
    ws.set_row_height(0, 32.0)?;

    let h_act = fmt_header_active();
    let h_ign = fmt_header_ignored();

    let headers: &[(&str, &Format, f64, &str)] = &[
        (
            "設定項目",
            &h_act,
            26.0,
            "【有効・必須】\n設定キー名です（変更しないでください）。全15項目すべてが v1.2.0 ソルバーおよび出力レンダラーに連動しています。",
        ),
        (
            "設定値",
            &h_act,
            14.0,
            "【有効・必須】\n実際にシステムへ適用される設定値です。数値またはプルダウン（有効/無効、強/中/弱）から変更できます。",
        ),
        (
            "初期値(未参照)",
            &h_ign,
            14.0,
            "【システム未参照（参考表示）】\n標準の初期値控えです（変更してもシステムには影響しません）。",
        ),
        (
            "単位/選択肢(未参照)",
            &h_ign,
            20.0,
            "【システム未参照（参考表示）】\nB列「設定値」に入力可能な単位や選択肢の目安です。",
        ),
        (
            "説明(未参照)",
            &h_ign,
            56.0,
            "【システム未参照（参考表示）】\n各設定項目がシフト生成結果にどう効くかの解説です。",
        ),
    ];

    for (col, (title, fmt, width, note_txt)) in headers.iter().enumerate() {
        let c = col as u16;
        ws.set_column_width(c, *width)?;
        ws.write_string_with_format(0, c, *title, fmt)?;
        ws.insert_note(0, c, &make_note(note_txt))?;
    }

    let items: &[(&str, &str, &str, &str, &str)] = &[
        ("2階裏_基本人数", "3", "3", "名", "2階裏の日次最低出勤人数（有休・指定公休者がいる日は自動減算）"),
        ("2階裏_看護師最低人数", "1", "1", "名", "2階裏で絶対に欠かせない看護師の最低人数 (Stage 1 HARD制約)"),
        ("2階裏_看護師推奨人数", "2", "2", "名", "2階裏で理想とされる看護師の人数 (不足時ソフトペナルティ)"),
        ("連勤上限日数", "5", "5", "日", "連続勤務の上限日数（5なら6連勤を禁止。前月末尾7日とも連動）"),
        ("固定公休_削減許容日数", "2", "2", "日", "曜日固定公休スタッフが所定日数に届かない場合の許容幅 (目標-N日)"),
        ("3連休抑制レベル", "中", "中", "強 / 中 / 弱 / なし", "有休・特別有休を除く通常の公休3連休発生を抑える強さ"),
        ("セクション人数ブレ幅", "1", "1", "名", "1階・2階表の平均出勤人数からの許容ブレ範囲 (平均±N名)"),
        ("週休配分目標", "2", "2", "日", "週単位・7日間ローリング窓での週休平準化基準日数（週5は2日、週4は3日）"),
        ("既存シフト維持レベル", "中", "中", "強 / 中 / 弱", "同ブック内に「YYYY年MM月シフト」シートがある場合の維持強度"),
        ("2階裏_警告表示人数", "2", "2", "名以下", "出力Excelの警告列に「⚠️」を表示する2階裏の出勤人数閾値"),
        ("祝日週_公休設定", "有効", "有効", "有効 / 無効", "祝日を含み診療日が5日となる週にコアスタッフへ最低1日の公休を確保"),
        ("週単位_公休平準化", "有効", "有効", "有効 / 無効", "ISO週および7日ローリング窓で各週の公休日数を平準化"),
        ("2階裏_週4名最低日数", "2", "2", "日", "2階裏の通常週(6稼働日)における4名以上出勤の最低確保日数"),
        ("空欄公休ローテーション", "有効", "有効", "有効 / 無効", "希望公休が空欄のコアスタッフに平日主公休を設定し過去3ヶ月履歴でローテーション"),
        ("指定公休日_有効", "有効", "有効", "有効 / 無効", "スタッフマスタの「指定公休日」列をHARD制約として適用"),
    ];

    let f_act_l = fmt_cell_active(false);
    let f_act_c = fmt_cell_active(true);
    let f_ign_c = fmt_cell_ignored(true);
    let f_ign_l = fmt_cell_ignored(false);

    for (idx, &(k, v, def_v, unit, desc)) in items.iter().enumerate() {
        let row_num = (idx + 1) as u32;
        ws.set_row_height(row_num, 22.0)?;
        ws.write_string_with_format(row_num, 0, k, &f_act_l)?;
        if let Ok(n) = v.parse::<i32>() {
            ws.write_number_with_format(row_num, 1, n as f64, &f_act_c)?;
        } else {
            ws.write_string_with_format(row_num, 1, v, &f_act_c)?;
        }
        if let Ok(n) = def_v.parse::<i32>() {
            ws.write_number_with_format(row_num, 2, n as f64, &f_ign_c)?;
        } else {
            ws.write_string_with_format(row_num, 2, def_v, &f_ign_c)?;
        }
        ws.write_string_with_format(row_num, 3, unit, &f_ign_c)?;
        ws.write_string_with_format(row_num, 4, desc, &f_ign_l)?;
    }

    // プルダウン設定
    let dv_consec = DataValidation::new()
        .allow_list_strings(&["強", "中", "弱", "なし"])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(6, 1, 6, 1, &dv_consec)?;

    let dv_keep = DataValidation::new()
        .allow_list_strings(&["強", "中", "弱"])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(9, 1, 9, 1, &dv_keep)?;

    let dv_enable = DataValidation::new()
        .allow_list_strings(&["有効", "無効"])?
        .set_error_style(DataValidationErrorStyle::Warning);
    ws.add_data_validation(11, 1, 15, 1, &dv_enable)?;

    Ok(())
}

// ============================================================================
// 6. 記入ガイド シート構築（システムには読み込まれない取扱説明書シート）
// ============================================================================
fn build_guide_sheet(ws: &mut Worksheet) -> Result<(), XlsxError> {
    ws.set_column_width(0, 26.0)?;
    ws.set_column_width(1, 38.0)?;
    ws.set_column_width(2, 46.0)?;
    ws.set_column_width(3, 42.0)?;

    let fmt_title = Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(14.0)
        .set_font_color(Color::RGB(0x1F4E78));

    let fmt_sec = Format::new()
        .set_bold()
        .set_font_name("Yu Gothic UI")
        .set_font_size(11.0)
        .set_font_color(Color::RGB(0x1F4E78));

    let h_act = fmt_header_active();
    let h_cond = fmt_header_cond();
    let h_ign = fmt_header_ignored();
    let f_cell = fmt_cell_active(false).set_text_wrap();

    ws.set_row_height(0, 28.0)?;
    ws.write_string_with_format(
        0,
        0,
        format!("勤務条件表 ひな形 記入ガイド（ひな形 Version: {} / 対応アプリ: v1.3.0以降）", TEMPLATE_VERSION),
        &fmt_title,
    )?;

    ws.set_row_height(2, 24.0)?;
    ws.write_string_with_format(2, 0, "■ 1. 列ヘッダーの色分け凡例（各シート共通）", &fmt_sec)?;

    ws.set_row_height(3, 26.0)?;
    ws.write_string_with_format(3, 0, "濃紺ヘッダー（白背景）", &h_act)?;
    ws.write_string_with_format(3, 1, "【システム参照：有効】", &f_cell)?;
    ws.write_string_with_format(3, 2, "シフト自動生成の計算・出力に直接反映される項目です。", &f_cell)?;
    ws.write_string_with_format(3, 3, "各ヘッダーにマウスを乗せると詳細ルールが表示されます。", &f_cell)?;

    ws.set_row_height(4, 26.0)?;
    ws.write_string_with_format(4, 0, "青緑ヘッダー（薄水色背景）", &h_cond)?;
    ws.write_string_with_format(4, 1, "【条件付き・補助参照】", &f_cell)?;
    ws.write_string_with_format(4, 2, "特定条件（パートの公休空欄時、GUI初期選択など）でのみ参照される項目です。", &f_cell)?;
    ws.write_string_with_format(4, 3, "正社員の「希望出勤曜日」などはここに入れても無視されます。", &f_cell)?;

    ws.set_row_height(5, 26.0)?;
    ws.write_string_with_format(5, 0, "グレーヘッダー（薄灰背景）", &h_ign)?;
    ws.write_string_with_format(5, 1, "【システム未参照（現場メモ専用）】", &f_cell)?;
    ws.write_string_with_format(5, 2, "勤務開始/終了時刻・勤務タイプ・担当医・備考など、計算に影響しないメモ欄です。", &f_cell)?;
    ws.write_string_with_format(5, 3, "現場の備忘録として自由にご記入いただけます。", &f_cell)?;

    ws.set_row_height(7, 24.0)?;
    ws.write_string_with_format(7, 0, "■ 2. 日本語平文・日付・曜日の「⭕ 認識される書き方」と「❌ NG・注意点」早見表", &fmt_sec)?;

    let table_headers = ["対象箇所", "やりたい設定", "⭕ 認識される書き方（対応OK）", "❌ 認識されない・誤動作する書き方（要注意）"];
    ws.set_row_height(8, 26.0)?;
    for (c, t) in table_headers.iter().enumerate() {
        ws.write_string_with_format(8, c as u16, *t, &h_act)?;
    }

    let guide_rows: &[(&str, &str, &str, &str)] = &[
        (
            "シフト制約条件\n「適用条件」列",
            "毎日適用したい",
            "全日",
            "未知の単語（例: 月末、第1金曜）を書くとエラーにならず「全日（毎日）」扱いになるため注意！",
        ),
        (
            "シフト制約条件\n「適用条件」列",
            "特定の曜日に適用したい",
            "月曜 / 火曜 / 水曜 / 木曜 / 金曜 / 土曜（または ○曜日）",
            "❌「金」「月」など1文字のみ（「曜」が必須）\n❌「火曜・水曜」の1行併記（先頭の1曜日しか効かないため行を分ける）",
        ),
        (
            "シフト制約条件\n「適用条件」列",
            "特定曜日の手術日に適用",
            "火曜(OPE日) / 水曜(OPE日) / 金曜の手術日",
            "❌「火(OPE日)」（「曜」を抜かすと全曜日のOPE日になってしまう）",
        ),
        (
            "シフト制約条件\n「適用条件」列",
            "特定術式の手術日に適用",
            "「Vit」手術日 / 「下垂」手術日 / 「cat」手術日 / 「gla」手術日\n※新しい術式も鉤括弧「○○」手術日 と書けば自動対応！",
            "❌ 丸括弧 (Vit)手術日 や鉤括弧なしの新術式（例: 涙道手術日 → 全手術日扱いになるため「涙道」手術日と書く）",
        ),
        (
            "スタッフマスタ\n「希望公休曜日」列",
            "複数曜日のうち1日休み(OR)\nvs 両方とも固定休み(AND)",
            "・OR選択: 水,もしくは木 / 水または木 / 水or木\n・AND固定: 火 / 月,金,土,日",
            "❌ ORのつもりで「水・木」「水/木」「水,木」と書く → 両方とも固定休み(AND)と判定されます！",
        ),
        (
            "スタッフマスタ\n「基本勤務パターン」列",
            "週4勤務・必要時週5応援",
            "週5日 / 週4日 / 週4日（必要に応じて週5日可） / 除外",
            "❌「週４日」（全角数字の４は認識されず週5扱いになるため半角4で記入）",
        ),
        (
            "スタッフマスタ\n「有休・特別有休・指定公休」",
            "単発日・連続期間の指定",
            "2026/11/17-2026/11/21 / 11/17〜11/21 / 2026/12/29-2027/01/04\nカンマ「,」・読点「、」・改行区切りすべてOK",
            "❌「2026/11/17-21」「11/17〜21」のように終了日の「月」を省略する書き方（必ず 11/17-11/21 と月まで書く）",
        ),
        (
            "スタッフマスタ ＆\nシフト制約条件",
            "スタッフ氏名の紐付け",
            "両シートで完全一致（例: 鈴木(孝) と 鈴木(孝)）",
            "❌ 片方が半角「鈴木(孝)」でもう片方が全角「鈴木（孝）」だと別人と判定されます",
        ),
        (
            "手術スケジュール\n「日付」列",
            "シフトを作成する対象月の決定",
            "作りたい年月の手術日を入力（手術0件の月も日付だけ1行入れれば対象月になる）",
            "❌ 手術スケジュールに1行も登場しない年月は、自動検出の対象月リストに表示されません",
        ),
        (
            "前月シフト実績",
            "月跨ぎ連勤・主公休引き継ぎ",
            "前月の出力シフトシートを丸ごとコピペ（A1に「YYYY年MM月」を残す）",
            "※A1の年月の「翌月」を生成する時だけ自動適用されます（例: 12月実績が入っていても9月生成時には悪影響を与えません）",
        ),
    ];

    for (idx, &(place, goal, ok_ex, ng_ex)) in guide_rows.iter().enumerate() {
        let r = (idx + 9) as u32;
        ws.set_row_height(r, 42.0)?;
        ws.write_string_with_format(r, 0, place, &f_cell)?;
        ws.write_string_with_format(r, 1, goal, &f_cell)?;
        ws.write_string_with_format(r, 2, ok_ex, &f_cell)?;
        ws.write_string_with_format(r, 3, ng_ex, &f_cell)?;
    }

    Ok(())
}
