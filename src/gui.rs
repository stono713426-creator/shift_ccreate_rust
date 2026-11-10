//! 勤務シフト自動生成システム デスクトップ GUI (`gui.rs`)
//!
//! Version: v1.3.1
//! Updated: 2026-11-10 - v1.3.1 入力条件表の事前検証警告（日付不正・スタッフ名不一致・重複行等）のプレビュー表示対応
//! - v1.3.0 (2026-11-08): 2階裏配置制約・石橋さん/長谷川さん個別ルール改修リリース同期
//! - v1.2.1 (2026-11-05): 標準条件表（勤務条件表_ひな形.xlsx）の自動検出優先度整備
//! - v1.2.0 (2026-11-01): バージョン同期・全設定完全適用対応
//! - v1.0.0 (2026-10-31): Phase 4 実装: eframe / egui によるモダンデスクトップ GUI の完全実装
//!
//! - Windows 標準日本語フォント（メイリオ / 游ゴシック）自動ロード
//! - 条件表 Excel 選択（rfd ダイアログ）＋カレント条件表自動検出ボタン
//! - ファイル選択時の軽量解析・プレビュー表示（スタッフ数、制約数、前月実績有無、対象月チェックボックス）
//! - バックグラウンドスレッドによる非同期ソルブ、プログレスバー、リアルタイムログ表示
//! - シフト生成完了後の「出力Excelを開く」「保存フォルダを開く」ワンクリックボタン

use crate::generator::{GenerateOptions, ShiftGenerator};
use crate::holidays::get_holidays;
use crate::parser::{parse_shift_excel, ParsedExcelData};
use crate::solver::SolveResult;
use crate::VERSION;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

/// ワーカースレッドから GUI メインループへ送信するイベント
#[derive(Debug)]
pub enum WorkerEvent {
    Log(String),
    Progress {
        current_idx: usize,
        total: usize,
        year: i32,
        month: u32,
        message: String,
    },
    Finished {
        output_path: PathBuf,
        results: Vec<(i32, u32, SolveResult)>,
        elapsed_secs: f64,
    },
    Failed(String),
}

/// 対象月の選択状態
#[derive(Debug, Clone)]
struct MonthSelection {
    year: i32,
    month: u32,
    selected: bool,
}

/// GUI アプリケーションの状態
pub struct ShiftApp {
    /// 入力 Excel ファイルのパス
    input_path: String,
    /// 出力 Excel ファイルのパス（未指定時は自動命名）
    output_path: String,
    /// パース済みデータ
    parsed_data: Option<ParsedExcelData>,
    /// パースエラーメッセージ
    parse_error: Option<String>,
    /// 対象月チェックリスト
    month_selections: Vec<MonthSelection>,
    /// ワーカースレッド実行中フラグ
    is_running: bool,
    /// 進捗率 (0.0 〜 1.0)
    progress_ratio: f32,
    /// 現在の進捗メッセージ
    status_message: String,
    /// リアルタイムログ一覧
    logs: Vec<String>,
    /// 完了時の出力先パス
    completed_output_path: Option<PathBuf>,
    /// 完了時の月別結果
    completed_results: Option<Vec<(i32, u32, SolveResult)>>,
    /// 完了時の総所要時間
    completed_elapsed_secs: Option<f64>,
    /// 失敗時のエラーメッセージ
    failure_error: Option<String>,
    /// ワーカースレッドからの受信チャンネル
    rx: Option<Receiver<WorkerEvent>>,
}

impl Default for ShiftApp {
    fn default() -> Self {
        Self {
            input_path: String::new(),
            output_path: String::new(),
            parsed_data: None,
            parse_error: None,
            month_selections: Vec::new(),
            is_running: false,
            progress_ratio: 0.0,
            status_message: "待機中: 勤務条件表 Excel ファイルを選択してください。".to_string(),
            logs: Vec::new(),
            completed_output_path: None,
            completed_results: None,
            completed_elapsed_secs: None,
            failure_error: None,
            rx: None,
        }
    }
}

impl ShiftApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut app = Self::default();
        // 起動時にカレントディレクトリの条件表Excelを自動検出
        if let Some(local_file) = find_local_condition_excel() {
            app.input_path = local_file.to_string_lossy().to_string();
            app.reload_file();
        }
        app
    }

    /// ファイルの再読み込みとプレビュー更新
    fn reload_file(&mut self) {
        self.parsed_data = None;
        self.parse_error = None;
        self.month_selections.clear();
        self.completed_output_path = None;
        self.completed_results = None;
        self.failure_error = None;

        let path = PathBuf::from(&self.input_path);
        if !path.exists() {
            if !self.input_path.trim().is_empty() {
                self.parse_error = Some("指定されたファイルが存在しません。".to_string());
            }
            return;
        }

        let holidays = get_holidays(2024, 2030);
        match parse_shift_excel(&path, &holidays) {
            Ok(parsed) => {
                let mut selections = Vec::new();
                for &(y, m) in &parsed.target_months {
                    let is_marked = parsed.marked_months.contains(&(y, m));
                    // marked_months が空なら全て選択、存在すれば marked のみ選択
                    let selected = if parsed.marked_months.is_empty() {
                        true
                    } else {
                        is_marked
                    };
                    selections.push(MonthSelection {
                        year: y,
                        month: m,
                        selected,
                    });
                }
                self.month_selections = selections;
                self.update_default_output_filename(&parsed.target_months);
                let warn_cnt = parsed.parse_warnings.len();
                self.parsed_data = Some(parsed);
                if warn_cnt > 0 {
                    self.status_message = format!(
                        "勤務条件表の解析が完了しました（⚠️ 入力確認事項が {warn_cnt} 件あります。プレビュー欄をご確認ください）。"
                    );
                } else {
                    self.status_message = "勤務条件表の解析が完了しました。作成対象月を確認して生成を開始してください。".to_string();
                }
            }
            Err(e) => {
                self.parse_error = Some(format!("Excelファイルの解析に失敗しました: {}", e));
            }
        }
    }

    /// デフォルト出力ファイル名の更新
    fn update_default_output_filename(&mut self, months: &[(i32, u32)]) {
        if self.output_path.is_empty() {
            self.output_path = ShiftGenerator::default_output_filename(months);
        }
    }

    /// シフト生成の非同期開始
    fn start_generation(&mut self) {
        let parsed = match &self.parsed_data {
            Some(p) => p.clone(),
            None => return,
        };

        let selected_months: Vec<(i32, u32)> = self
            .month_selections
            .iter()
            .filter(|m| m.selected)
            .map(|m| (m.year, m.month))
            .collect();

        if selected_months.is_empty() {
            self.status_message = "エラー: 作成対象年月が1つも選択されていません。".to_string();
            return;
        }

        let out_path = if self.output_path.trim().is_empty() {
            PathBuf::from(ShiftGenerator::default_output_filename(&selected_months))
        } else {
            PathBuf::from(&self.output_path)
        };

        self.is_running = true;
        self.progress_ratio = 0.0;
        self.status_message = "祝日カレンダーの計算および最適化の準備中...".to_string();
        self.logs.clear();
        self.completed_output_path = None;
        self.completed_results = None;
        self.failure_error = None;

        let (tx, rx): (Sender<WorkerEvent>, Receiver<WorkerEvent>) = channel();
        self.rx = Some(rx);

        let options = GenerateOptions {
            target_months: Some(selected_months),
            output_path: Some(out_path),
        };

        // バックグラウンドスレッドの起動
        thread::spawn(move || {
            let start_time = std::time::Instant::now();
            let holidays = get_holidays(2024, 2030);

            let tx_clone = tx.clone();
            let progress_cb = move |idx, total, y, m, msg: &str| {
                let ratio = if total > 0 {
                    idx as f32 / total as f32
                } else {
                    0.0
                };
                let _ = tx_clone.send(WorkerEvent::Progress {
                    current_idx: idx,
                    total,
                    year: y,
                    month: m,
                    message: msg.to_string(),
                });
                let _ = tx_clone.send(WorkerEvent::Log(format!(
                    "[{:.1}%] {}",
                    ratio * 100.0,
                    msg
                )));
            };

            match ShiftGenerator::generate(&parsed, &holidays, &options, progress_cb) {
                Ok((out_file, results)) => {
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let _ = tx.send(WorkerEvent::Finished {
                        output_path: out_file,
                        results,
                        elapsed_secs: elapsed,
                    });
                }
                Err(e) => {
                    let _ = tx.send(WorkerEvent::Failed(e));
                }
            }
        });
    }

    /// ワーカースレッドからのイベント受信処理
    fn check_worker_events(&mut self) {
        let mut finished = false;
        if let Some(ref rx) = self.rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    WorkerEvent::Log(msg) => {
                        self.logs.push(msg);
                    }
                    WorkerEvent::Progress {
                        current_idx,
                        total,
                        message,
                        ..
                    } => {
                        if total > 0 {
                            self.progress_ratio = current_idx as f32 / total as f32;
                        }
                        self.status_message = message;
                    }
                    WorkerEvent::Finished {
                        output_path,
                        results,
                        elapsed_secs,
                    } => {
                        self.progress_ratio = 1.0;
                        self.is_running = false;
                        self.completed_output_path = Some(output_path);
                        self.completed_results = Some(results);
                        self.completed_elapsed_secs = Some(elapsed_secs);
                        self.status_message = format!(
                            "🎉 シフト生成が正常に完了しました！（総所要時間: {:.2}秒）",
                            elapsed_secs
                        );
                        finished = true;
                    }
                    WorkerEvent::Failed(err) => {
                        self.is_running = false;
                        self.failure_error = Some(err.clone());
                        self.status_message = format!("❌ エラー: {}", err);
                        finished = true;
                    }
                }
            }
        }
        if finished {
            self.rx = None;
        }
    }
}

impl eframe::App for ShiftApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // バックグラウンドタスクのイベント処理
        self.check_worker_events();

        if self.is_running {
            ctx.request_repaint();
        }

        // トップパネル（ヘッダー）
        egui::TopBottomPanel::top("header_panel").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new(format!("勤務シフト自動生成システム (Rust Edition {})", VERSION)).size(20.0).strong());
            });
            ui.label(RichText::new("二段階数理最適化 (MILP / HiGHS) による超高速シフト自動作成").color(Color32::from_rgb(120, 120, 120)));
            ui.add_space(6.0);
        });

        // セントラルパネル（メインコンテンツ）
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // 1. 入力ファイル選択セクション
                ui.group(|ui| {
                    ui.label(RichText::new("1. 勤務条件表 Excel ファイルの選択").strong());
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        let text_edit = ui.add_sized(
                            Vec2::new(ui.available_width() - 210.0, 26.0),
                            egui::TextEdit::singleline(&mut self.input_path).hint_text("勤務条件表_*.xlsx のパスを入力または選択"),
                        );
                        if text_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            self.reload_file();
                        }

                        if ui.button("📁 選択...").clicked() {
                            if let Some(file) = rfd::FileDialog::new()
                                .add_filter("Excel ファイル", &["xlsx", "xlsm"])
                                .pick_file()
                            {
                                self.input_path = file.to_string_lossy().to_string();
                                self.reload_file();
                            }
                        }

                        if ui.button("🔍 自動検出").clicked() {
                            if let Some(local_file) = find_local_condition_excel() {
                                self.input_path = local_file.to_string_lossy().to_string();
                                self.reload_file();
                            }
                        }
                    });

                    if let Some(ref err) = self.parse_error {
                        ui.add_space(4.0);
                        ui.label(RichText::new(err).color(Color32::from_rgb(220, 50, 50)));
                    }
                });

                ui.add_space(8.0);

                // 2. プレビューおよび作成対象月選択セクション
                if let Some(ref parsed) = self.parsed_data {
                    ui.group(|ui| {
                        ui.label(RichText::new("2. 勤務条件表の解析プレビュー").strong());
                        ui.add_space(4.0);

                        ui.horizontal_wrapped(|ui| {
                            let sec_2f_f = parsed.staffs.iter().filter(|s| s.section == "2階表").count();
                            let sec_2f_b = parsed.staffs.iter().filter(|s| s.section == "2階裏").count();
                            let sec_1f = parsed.staffs.iter().filter(|s| s.section == "1階").count();

                            ui.label(format!("👥 スタッフ数: {}名 (2階表: {}名, 2階裏: {}名, 1階: {}名)", parsed.staffs.len(), sec_2f_f, sec_2f_b, sec_1f));
                            ui.separator();
                            ui.label(format!("📋 制約条件: {}件", parsed.rules.len()));
                            ui.separator();
                            ui.label(format!("🏥 手術日程: {}日", parsed.ope_dict.len()));
                            ui.separator();
                            let prev_str = if !parsed.prev_month_tail_shifts.is_empty() { "あり" } else { "なし" };
                            ui.label(format!("⏮ 前月実績: {}", prev_str));
                        });

                        if !parsed.parse_warnings.is_empty() {
                            ui.add_space(6.0);
                            for warn_msg in &parsed.parse_warnings {
                                ui.label(
                                    RichText::new(warn_msg)
                                        .color(Color32::from_rgb(200, 100, 0))
                                        .strong(),
                                );
                            }
                        }

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.label(RichText::new("作成対象年月の選択:").strong());
                            if ui.button("全選択").clicked() {
                                for m in &mut self.month_selections {
                                    m.selected = true;
                                }
                            }
                            if ui.button("全解除").clicked() {
                                for m in &mut self.month_selections {
                                    m.selected = false;
                                }
                            }
                        });

                        ui.horizontal_wrapped(|ui| {
                            for m in &mut self.month_selections {
                                let label = format!("{}年{:02}月", m.year, m.month);
                                ui.checkbox(&mut m.selected, label);
                            }
                        });
                    });

                    ui.add_space(8.0);

                    // 3. 出力ファイル設定セクション
                    ui.group(|ui| {
                        ui.label(RichText::new("3. 出力先 Excel ファイル").strong());
                        ui.add_space(4.0);

                        ui.horizontal(|ui| {
                            ui.add_sized(
                                Vec2::new(ui.available_width() - 110.0, 26.0),
                                egui::TextEdit::singleline(&mut self.output_path).hint_text("出力ファイル名 (.xlsx)"),
                            );

                            if ui.button("💾 変更...").clicked() {
                                if let Some(file) = rfd::FileDialog::new()
                                    .add_filter("Excel ファイル", &["xlsx"])
                                    .set_file_name(&self.output_path)
                                    .save_file()
                                {
                                    self.output_path = file.to_string_lossy().to_string();
                                }
                            }
                        });
                    });

                    ui.add_space(12.0);

                    // 4. 実行ボタン
                    ui.vertical_centered(|ui| {
                        let btn_text = if self.is_running {
                            "⏳ シフト最適化を実行中..."
                        } else {
                            "🚀 シフト自動生成を開始"
                        };

                        let btn = egui::Button::new(RichText::new(btn_text).size(16.0).strong())
                            .min_size(Vec2::new(260.0, 42.0));

                        if ui.add_enabled(!self.is_running, btn).clicked() {
                            self.start_generation();
                        }
                    });
                }

                ui.add_space(10.0);

                // 5. 進捗状況 ＆ リアルタイムログ
                ui.group(|ui| {
                    ui.label(RichText::new("4. 実行ログ・進捗").strong());
                    ui.add_space(4.0);

                    // プログレスバー
                    ui.add(
                        egui::ProgressBar::new(self.progress_ratio)
                            .show_percentage()
                            .animate(self.is_running),
                    );

                    ui.add_space(4.0);
                    ui.label(&self.status_message);

                    // 完了時の成功バナー・アクションボタン
                    if let Some(ref out_path) = self.completed_output_path {
                        ui.add_space(8.0);
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("🎉 シフト生成が正常に完了しました！").color(Color32::from_rgb(0, 150, 0)).strong());
                                if let Some(secs) = self.completed_elapsed_secs {
                                    ui.label(format!("(総所要時間: {:.2}秒)", secs));
                                }
                            });

                            if let Some(ref results) = self.completed_results {
                                ui.horizontal_wrapped(|ui| {
                                    for (y, m, res) in results {
                                        let tag = if res.success {
                                            format!("{}年{:02}月: 成功 ({:.2}s)", y, m, res.execution_time_secs)
                                        } else {
                                            format!("{}年{:02}月: 失敗", y, m)
                                        };
                                        ui.label(RichText::new(tag).size(11.0));
                                    }
                                });
                            }

                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if ui.button(RichText::new("📊 出力 Excel を開く").strong()).clicked() {
                                    open_file_in_os(out_path);
                                }
                                if ui.button("📂 保存先フォルダを開く").clicked() {
                                    open_folder_in_os(out_path);
                                }
                            });
                        });
                    }

                    // 失敗時バナー
                    if let Some(ref err) = self.failure_error {
                        ui.add_space(8.0);
                        ui.label(RichText::new(format!("❌ エラー: {}", err)).color(Color32::from_rgb(220, 50, 50)).strong());
                    }

                    ui.add_space(6.0);
                    // ログ表示スクロールエリア
                    ui.separator();
                    ui.label(RichText::new("詳細ログ:").size(12.0));
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for log in &self.logs {
                                ui.label(RichText::new(log).monospace().size(11.0));
                            }
                        });
                });
            });
        });
    }
}

/// カレントディレクトリから勤務条件表Excelを自動検出する
///
/// 優先順位:
/// 1. ユーザーが作成したカスタム勤務条件表 (`勤務条件表_ひな形.xlsx` および旧検証用 `勤務条件表_9*.xlsx` 以外)
/// 2. 標準ひな形 (`勤務条件表_ひな形.xlsx`)
/// 3. その他の `勤務条件*.xlsx`
pub fn find_local_condition_excel() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(".") {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if (name.contains("勤務条件表") || name.contains("勤務条件"))
                    && name.ends_with(".xlsx")
                    && !name.starts_with("~$")
                {
                    candidates.push(path);
                }
            }
        }
    }
    candidates.sort();

    // 1. ユーザー独自の勤務条件表があれば最優先
    if let Some(custom) = candidates.iter().find(|p| {
        let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        n != "勤務条件表_ひな形.xlsx" && n != "勤務条件表_9.xlsx" && n != "勤務条件表_9月.xlsx"
    }) {
        return Some(custom.clone());
    }

    // 2. 標準条件表 (勤務条件表_ひな形.xlsx)
    if let Some(tpl) = candidates
        .iter()
        .find(|p| p.file_name().and_then(|s| s.to_str()) == Some("勤務条件表_ひな形.xlsx"))
    {
        return Some(tpl.clone());
    }

    // 3. フォールバック
    candidates.into_iter().next()
}

/// OS の既定アプリケーションでファイルを開く
pub fn open_file_in_os(path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", &path_str])
        .spawn();
}

/// エクスプローラーでフォルダを開きファイルを選択表示する
pub fn open_folder_in_os(path: &Path) {
    if let Ok(abs_path) = std::fs::canonicalize(path) {
        let abs_str = abs_path.to_string_lossy().to_string();
        // \\?\ プレフィックスの除去
        let clean_str = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str);
        let _ = std::process::Command::new("explorer")
            .args(["/select,", clean_str])
            .spawn();
    } else {
        let _ = std::process::Command::new("explorer")
            .arg(".")
            .spawn();
    }
}

/// 日本語フォント（Windows メイリオ / 游ゴシック）の設定
pub fn setup_custom_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let font_candidates = [
        r"C:\Windows\Fonts\meiryo.ttc",
        r"C:\Windows\Fonts\YuGothM.ttc",
        r"C:\Windows\Fonts\msgothic.ttc",
    ];

    let mut loaded = false;
    for path in font_candidates {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "jp_font".to_owned(),
                Arc::new(egui::FontData::from_owned(data)),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "jp_font".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "jp_font".to_owned());
            loaded = true;
            break;
        }
    }

    if !loaded {
        eprintln!("警告: Windows 日本語フォントが見つかりませんでした。");
    }

    ctx.set_fonts(fonts);
}

/// GUI アプリケーションの実行エントリポイント
pub fn run_gui() -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([840.0, 680.0])
            .with_min_inner_size([640.0, 520.0])
            .with_title(format!("勤務シフト自動生成システム (Rust Edition {})", VERSION)),
        ..Default::default()
    };

    eframe::run_native(
        "勤務シフト自動生成システム",
        native_options,
        Box::new(|cc| {
            setup_custom_fonts(&cc.egui_ctx);
            Ok(Box::new(ShiftApp::new(cc)))
        }),
    )
}
