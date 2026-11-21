//! 勤務シフト自動生成システム デスクトップ GUI (`gui.rs`)
//!
//! Version: v2.0.0
//! Updated: 2026-11-21 - v2.0.0 厳格探索＋適切解なし時の月別一時停止・原因診断・緩和チェックリスト選択UI・完了品質バッジ対応
//! - v1.3.3 (2026-11-17): 手術スケジュール「作成対象」列による初期選択判定を廃止（全対象月を初期選択）
//! - v1.3.2 (2026-11-10): 外部Excel起動時の一瞬コンソール表示抑止 (CREATE_NO_WINDOW)
//! - v1.3.1 (2026-11-10): 入力条件表の事前検証警告のプレビュー表示対応
//! - v1.0.0 (2026-10-31): Phase 4 実装: eframe / egui によるモダンデスクトップ GUI の完全実装

use crate::config::RelaxationKind;
use crate::generator::{
    holidays_for_months, parse_excel_with_dynamic_holidays, GenerateOptions, MonthStepOutcome,
    ShiftGenerationSession, ShiftGenerator,
};
use crate::parser::ParsedExcelData;
use crate::solver::SolveResult;
use crate::VERSION;
use chrono::NaiveDate;
use eframe::egui::{self, Color32, RichText, Vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

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
    PausedForRelaxation {
        session: Box<ShiftGenerationSession>,
        year: i32,
        month: u32,
        result: SolveResult,
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
    /// 動的取得した祝日マップ
    holidays: HashMap<NaiveDate, String>,
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
    /// 一時停止中のセッション（適切解なし時）
    paused_session: Option<ShiftGenerationSession>,
    /// 一時停止中の年月
    paused_month: Option<(i32, u32)>,
    /// 一時停止中のソルブ＆診断結果
    paused_result: Option<SolveResult>,
    /// ユーザーがチェック選択した緩和条件
    selected_relaxations: HashSet<RelaxationKind>,
    /// 生成開始時刻
    generation_start_time: Option<Instant>,
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
            holidays: HashMap::new(),
            parse_error: None,
            month_selections: Vec::new(),
            is_running: false,
            progress_ratio: 0.0,
            status_message: "待機中: 勤務条件表 Excel ファイルを選択してください。".to_string(),
            logs: Vec::new(),
            paused_session: None,
            paused_month: None,
            paused_result: None,
            selected_relaxations: HashSet::new(),
            generation_start_time: None,
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
        if let Some(local_file) = find_local_condition_excel() {
            app.input_path = local_file.to_string_lossy().to_string();
            app.reload_file();
        }
        app
    }

    /// ファイルの再読み込みとプレビュー更新
    fn reload_file(&mut self) {
        self.parsed_data = None;
        self.holidays.clear();
        self.parse_error = None;
        self.month_selections.clear();
        self.paused_session = None;
        self.paused_month = None;
        self.paused_result = None;
        self.selected_relaxations.clear();
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

        match parse_excel_with_dynamic_holidays(&path) {
            Ok((parsed, holidays)) => {
                let mut selections = Vec::new();
                for &(y, m) in &parsed.target_months {
                    selections.push(MonthSelection {
                        year: y,
                        month: m,
                        selected: true,
                    });
                }
                self.month_selections = selections;
                self.update_default_output_filename(&parsed.target_months);
                let warn_cnt = parsed.parse_warnings.len();
                let has_parse_error = parsed.parse_warnings.iter().any(|w| w.starts_with('❌'));
                self.holidays = holidays;
                self.parsed_data = Some(parsed);
                if has_parse_error {
                    self.status_message = format!(
                        "❌ 勤務条件表に修正が必要な入力エラーがあります（確認事項 {warn_cnt} 件）。プレビュー欄をご確認ください。"
                    );
                } else if warn_cnt > 0 {
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

        let holidays = holidays_for_months(&selected_months);
        let options = GenerateOptions {
            target_months: Some(selected_months),
            output_path: Some(out_path),
            approved_relaxations_by_month: HashMap::new(),
        };

        let session = match ShiftGenerationSession::new(&parsed, &holidays, &options) {
            Ok(s) => s,
            Err(e) => {
                self.failure_error = Some(e.clone());
                self.status_message = format!("❌ エラー: {e}");
                return;
            }
        };

        self.is_running = true;
        self.progress_ratio = 0.0;
        self.status_message = "厳格条件でのシフト最適化を開始しています...".to_string();
        self.logs.clear();
        self.paused_session = None;
        self.paused_month = None;
        self.paused_result = None;
        self.selected_relaxations.clear();
        self.completed_output_path = None;
        self.completed_results = None;
        self.failure_error = None;

        let start_time = Instant::now();
        self.generation_start_time = Some(start_time);
        self.launch_worker(session, HashSet::new(), start_time);
    }

    /// 選択された緩和条件を適用して一時停止中の月を再試行する
    fn retry_paused_with_relaxations(&mut self) {
        let Some(session) = self.paused_session.take() else {
            return;
        };
        let approved = self.selected_relaxations.clone();
        self.paused_result = None;
        self.is_running = true;
        self.status_message = format!(
            "承認緩和 {} 件を適用して再試行しています...",
            approved.len()
        );
        let start_time = self.generation_start_time.unwrap_or_else(Instant::now);
        self.launch_worker(session, approved, start_time);
    }

    /// 一時停止中の月をスキップ（未生成）して残りの月を続行する
    fn skip_paused_month(&mut self) {
        let Some(mut session) = self.paused_session.take() else {
            return;
        };
        let Some(failed_res) = self.paused_result.take() else {
            return;
        };
        self.paused_month = None;
        self.selected_relaxations.clear();
        session.skip_current_month(failed_res);

        self.is_running = true;
        self.status_message = "該当月を未生成としてスキップし、残りの月を続行中...".to_string();
        let start_time = self.generation_start_time.unwrap_or_else(Instant::now);
        self.launch_worker(session, HashSet::new(), start_time);
    }

    /// 一時停止中の生成を中止する
    fn abort_paused_generation(&mut self) {
        self.paused_session = None;
        self.paused_month = None;
        self.paused_result = None;
        self.selected_relaxations.clear();
        self.is_running = false;
        self.status_message = "シフト生成を中止しました。".to_string();
    }

    /// ワーカースレッドを起動してセッションを進める
    fn launch_worker(
        &mut self,
        mut session: ShiftGenerationSession,
        initial_approved: HashSet<RelaxationKind>,
        start_time: Instant,
    ) {
        let (tx, rx): (Sender<WorkerEvent>, Receiver<WorkerEvent>) = channel();
        self.rx = Some(rx);

        thread::spawn(move || {
            let mut next_approved = initial_approved;

            loop {
                if session.current_month().is_none() {
                    let tx_fin = tx.clone();
                    let progress_cb = move |idx, total, y, m, msg: &str| {
                        let ratio = if total > 0 { idx as f32 / total as f32 } else { 0.0 };
                        let _ = tx_fin.send(WorkerEvent::Progress {
                            current_idx: idx,
                            total,
                            year: y,
                            month: m,
                            message: msg.to_string(),
                        });
                        let _ = tx_fin.send(WorkerEvent::Log(format!("[{:.1}%] {}", ratio * 100.0, msg)));
                    };
                    match session.finalize_excel(progress_cb) {
                        Ok((out_file, results)) => {
                            let _ = tx.send(WorkerEvent::Finished {
                                output_path: out_file,
                                results,
                                elapsed_secs: start_time.elapsed().as_secs_f64(),
                            });
                        }
                        Err(e) => {
                            let _ = tx.send(WorkerEvent::Failed(e));
                        }
                    }
                    break;
                }

                let approved = std::mem::take(&mut next_approved);
                let tx_step = tx.clone();
                let progress_cb = move |idx, total, y, m, msg: &str| {
                    let ratio = if total > 0 { idx as f32 / total as f32 } else { 0.0 };
                    let _ = tx_step.send(WorkerEvent::Progress {
                        current_idx: idx,
                        total,
                        year: y,
                        month: m,
                        message: msg.to_string(),
                    });
                    let _ = tx_step.send(WorkerEvent::Log(format!("[{:.1}%] {}", ratio * 100.0, msg)));
                };

                match session.solve_current_month(&approved, progress_cb) {
                    MonthStepOutcome::Solved { has_next, .. } => {
                        if !has_next {
                            let tx_fin = tx.clone();
                            let fin_cb = move |idx, total, y, m, msg: &str| {
                                let ratio = if total > 0 { idx as f32 / total as f32 } else { 0.0 };
                                let _ = tx_fin.send(WorkerEvent::Progress {
                                    current_idx: idx,
                                    total,
                                    year: y,
                                    month: m,
                                    message: msg.to_string(),
                                });
                                let _ = tx_fin.send(WorkerEvent::Log(format!("[{:.1}%] {}", ratio * 100.0, msg)));
                            };
                            match session.finalize_excel(fin_cb) {
                                Ok((out_file, results)) => {
                                    let _ = tx.send(WorkerEvent::Finished {
                                        output_path: out_file,
                                        results,
                                        elapsed_secs: start_time.elapsed().as_secs_f64(),
                                    });
                                }
                                Err(e) => {
                                    let _ = tx.send(WorkerEvent::Failed(e));
                                }
                            }
                            break;
                        }
                    }
                    MonthStepOutcome::NeedsRelaxation { year, month, result } => {
                        let _ = tx.send(WorkerEvent::PausedForRelaxation {
                            session: Box::new(session),
                            year,
                            month,
                            result,
                        });
                        break;
                    }
                    MonthStepOutcome::AllDone => {
                        break;
                    }
                }
            }
        });
    }

    /// ワーカースレッドからのイベント受信処理
    fn check_worker_events(&mut self) {
        let mut detach_rx = false;
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
                    WorkerEvent::PausedForRelaxation {
                        session,
                        year,
                        month,
                        result,
                    } => {
                        self.is_running = false;
                        let total = session.total_months().max(1);
                        self.progress_ratio = session.current_idx() as f32 / total as f32;
                        if self.paused_month != Some((year, month)) {
                            self.selected_relaxations.clear();
                        }
                        self.paused_month = Some((year, month));
                        self.paused_session = Some(*session);
                        self.paused_result = Some(result);
                        self.status_message = format!(
                            "⚠️ {}年{:02}月：現在の条件では適切解がありません。原因を確認し緩和条件を選択してください。",
                            year, month
                        );
                        detach_rx = true;
                    }
                    WorkerEvent::Finished {
                        output_path,
                        results,
                        elapsed_secs,
                    } => {
                        self.progress_ratio = 1.0;
                        self.is_running = false;
                        self.paused_session = None;
                        self.paused_month = None;
                        self.paused_result = None;
                        self.selected_relaxations.clear();

                        let failed_cnt = results.iter().filter(|(_, _, r)| !r.success).count();
                        let relaxed_cnt = results.iter().filter(|(_, _, r)| r.success && r.stage == 2).count();
                        self.status_message = if failed_cnt > 0 {
                            format!(
                                "⚠️ 処理完了（{}ヶ月が適切解なし・未生成 / 所要時間: {:.2}秒）",
                                failed_cnt, elapsed_secs
                            )
                        } else if relaxed_cnt > 0 {
                            format!(
                                "⚠️ 承認緩和を適用して全月のシフト生成が完了しました！（所要時間: {:.2}秒）",
                                elapsed_secs
                            )
                        } else {
                            format!(
                                "🎉 全月のシフト生成が厳格条件で正常に完了しました！（所要時間: {:.2}秒）",
                                elapsed_secs
                            )
                        };

                        self.completed_output_path = Some(output_path);
                        self.completed_results = Some(results);
                        self.completed_elapsed_secs = Some(elapsed_secs);
                        detach_rx = true;
                    }
                    WorkerEvent::Failed(err) => {
                        self.is_running = false;
                        self.paused_session = None;
                        self.paused_month = None;
                        self.paused_result = None;
                        self.failure_error = Some(err.clone());
                        self.status_message = format!("❌ エラー: {}", err);
                        detach_rx = true;
                    }
                }
            }
        }
        if detach_rx {
            self.rx = None;
        }
    }
}

impl eframe::App for ShiftApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.check_worker_events();

        if self.is_running {
            ctx.request_repaint();
        }

        // トップパネル（ヘッダー）
        egui::TopBottomPanel::top("header_panel").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new(format!("勤務シフト自動生成システム (Rust Edition {})", VERSION))
                        .size(20.0)
                        .strong(),
                );
            });
            ui.label(
                RichText::new("厳格数理最適化 (MILP / HiGHS) ＆ 原因診断・対話的緩和選択対応")
                    .color(Color32::from_rgb(120, 120, 120)),
            );
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
                            egui::TextEdit::singleline(&mut self.input_path)
                                .hint_text("勤務条件表_*.xlsx のパスを入力または選択"),
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

                            ui.label(format!(
                                "👥 スタッフ数: {}名 (2階表: {}名, 2階裏: {}名, 1階: {}名)",
                                parsed.staffs.len(),
                                sec_2f_f,
                                sec_2f_b,
                                sec_1f
                            ));
                            ui.separator();
                            ui.label(format!("📋 制約条件: {}件", parsed.rules.len()));
                            ui.separator();
                            ui.label(format!("🏥 手術日程: {}日", parsed.ope_dict.len()));
                            ui.separator();
                            let prev_str = if !parsed.prev_month_tail_shifts.is_empty() {
                                "あり"
                            } else {
                                "なし"
                            };
                            ui.label(format!("⏮ 前月実績: {}", prev_str));
                        });

                        if !parsed.parse_warnings.is_empty() {
                            ui.add_space(6.0);
                            for warn_msg in &parsed.parse_warnings {
                                let color = if warn_msg.starts_with('❌') {
                                    Color32::from_rgb(200, 30, 30)
                                } else if warn_msg.starts_with("ℹ️") {
                                    Color32::from_rgb(40, 100, 170)
                                } else {
                                    Color32::from_rgb(200, 100, 0)
                                };
                                ui.label(RichText::new(warn_msg).color(color).strong());
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
                                egui::TextEdit::singleline(&mut self.output_path)
                                    .hint_text("出力ファイル名 (.xlsx)"),
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
                    let is_paused = self.paused_session.is_some();
                    ui.vertical_centered(|ui| {
                        let btn_text = if self.is_running {
                            "⏳ シフト最適化を実行中..."
                        } else if is_paused {
                            "⏸ 緩和条件の選択待機中..."
                        } else {
                            "🚀 シフト自動生成を開始（厳格探索）"
                        };

                        let btn = egui::Button::new(RichText::new(btn_text).size(16.0).strong())
                            .min_size(Vec2::new(300.0, 42.0));

                        if ui.add_enabled(!self.is_running && !is_paused, btn).clicked() {
                            self.start_generation();
                        }
                    });
                }

                // 4.5. 適切解なし時の一時停止・原因診断＆緩和選択パネル
                if self.paused_session.is_some() {
                    ui.add_space(10.0);
                    let mut action_retry = false;
                    let mut action_skip = false;
                    let mut action_abort = false;

                    if let (Some((py, pm)), Some(ref paused_res)) =
                        (self.paused_month, &self.paused_result)
                    {
                        ui.group(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "⚠️ {}年{:02}月：厳格条件では適切解がありません",
                                    py, pm
                                ))
                                .size(15.0)
                                .color(Color32::from_rgb(190, 40, 40))
                                .strong(),
                            );
                            ui.add_space(4.0);

                            if let Some(ref diag) = paused_res.diagnosis {
                                ui.label(&diag.summary);
                                ui.add_space(4.0);

                                ui.label(RichText::new("【検出された原因】").strong());
                                for c in &diag.causes {
                                    ui.label(
                                        RichText::new(format!(
                                            "・[{}] {}: {}",
                                            c.category, c.scope_label, c.detail
                                        ))
                                        .size(12.0),
                                    );
                                }

                                ui.add_space(6.0);
                                if diag.tier0_unsolvable {
                                    ui.label(
                                        RichText::new(
                                            "❌ Tier 0（絶対不変制約・入力書式等）に競合があるため緩和選択肢では解決できません。勤務条件表を見直すか、この月をスキップしてください。",
                                        )
                                        .color(Color32::from_rgb(200, 30, 30))
                                        .strong(),
                                    );
                                } else {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("【適用する緩和条件を選択】").strong());
                                        if ui.button("✨ 推奨候補を一括選択").clicked() {
                                            for p in &diag.proposals {
                                                if p.recommended {
                                                    self.selected_relaxations.insert(p.kind.clone());
                                                }
                                            }
                                        }
                                        if ui.button("全解除").clicked() {
                                            self.selected_relaxations.clear();
                                        }
                                    });

                                    egui::ScrollArea::vertical()
                                        .max_height(210.0)
                                        .id_salt("relax_proposals_scroll")
                                        .show(ui, |ui| {
                                            let mut render_group = |title: &str, items: Vec<&crate::config::RelaxationProposal>| {
                                                if items.is_empty() {
                                                    return;
                                                }
                                                ui.add_space(3.0);
                                                ui.label(RichText::new(title).size(12.0).strong());
                                                for p in items {
                                                    let mut checked =
                                                        self.selected_relaxations.contains(&p.kind);
                                                    let cb_label = format!("{} — {}", p.label, p.impact_desc);
                                                    if ui.checkbox(&mut checked, cb_label).changed() {
                                                        if checked {
                                                            self.selected_relaxations
                                                                .insert(p.kind.clone());
                                                        } else {
                                                            self.selected_relaxations.remove(&p.kind);
                                                        }
                                                    }
                                                }
                                            };

                                            let rec_items = diag
                                                .proposals
                                                .iter()
                                                .filter(|p| p.recommended && !p.is_last_resort)
                                                .collect();
                                            let alt_items = diag
                                                .proposals
                                                .iter()
                                                .filter(|p| !p.recommended && !p.is_last_resort)
                                                .collect();
                                            let last_items = diag
                                                .proposals
                                                .iter()
                                                .filter(|p| p.is_last_resort)
                                                .collect();

                                            render_group("① 推奨ピンポイント緩和候補（最小限）", rec_items);
                                            render_group("② 代替・週/月一括候補", alt_items);
                                            render_group("⚠️ ③ 最終手段（所定勤務日数±1日調整）", last_items);
                                        });
                                }
                            } else {
                                ui.label(
                                    RichText::new(format!(
                                        "ソルバー終了ステータス: {:?}（タイムアウトまたは内部エラーのため診断結果を取得できませんでした。再試行またはスキップを選択してください）",
                                        paused_res.status
                                    ))
                                    .color(Color32::from_rgb(180, 90, 0)),
                                );
                            }

                            ui.add_space(8.0);
                            ui.horizontal(|ui| {
                                let can_retry = match paused_res.diagnosis {
                                    Some(ref diag) => {
                                        !diag.tier0_unsolvable && !self.selected_relaxations.is_empty()
                                    }
                                    None => true,
                                };
                                let retry_label = if paused_res.diagnosis.is_some() {
                                    "🔄 選択した緩和条件で再試行"
                                } else {
                                    "🔄 再試行"
                                };
                                let retry_btn = egui::Button::new(RichText::new(retry_label).strong());
                                if ui.add_enabled(can_retry, retry_btn).clicked() {
                                    action_retry = true;
                                }
                                if ui.button("⏭ この月をスキップ（未生成）して続行").clicked() {
                                    action_skip = true;
                                }
                                if ui.button("🛑 中止").clicked() {
                                    action_abort = true;
                                }
                            });
                        });
                    }

                    if action_retry {
                        self.retry_paused_with_relaxations();
                    } else if action_skip {
                        self.skip_paused_month();
                    } else if action_abort {
                        self.abort_paused_generation();
                    }
                }

                ui.add_space(10.0);

                // 5. 進捗状況 ＆ リアルタイムログ
                ui.group(|ui| {
                    ui.label(RichText::new("4. 実行ログ・進捗").strong());
                    ui.add_space(4.0);

                    ui.add(
                        egui::ProgressBar::new(self.progress_ratio)
                            .show_percentage()
                            .animate(self.is_running),
                    );

                    ui.add_space(4.0);
                    ui.label(&self.status_message);

                    // 完了時のバナー・アクションボタン
                    if let Some(ref out_path) = self.completed_output_path {
                        ui.add_space(8.0);
                        ui.group(|ui| {
                            let results_opt = self.completed_results.as_ref();
                            let failed_cnt = results_opt
                                .map(|rs| rs.iter().filter(|(_, _, r)| !r.success).count())
                                .unwrap_or(0);
                            let relaxed_cnt = results_opt
                                .map(|rs| rs.iter().filter(|(_, _, r)| r.success && r.stage == 2).count())
                                .unwrap_or(0);

                            ui.horizontal(|ui| {
                                if failed_cnt > 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "⚠️ {}ヶ月が「適切解なし（未生成）」で出力完了しました（生成診断レポートをご確認ください）",
                                            failed_cnt
                                        ))
                                        .color(Color32::from_rgb(200, 80, 0))
                                        .strong(),
                                    );
                                } else if relaxed_cnt > 0 {
                                    ui.label(
                                        RichText::new("⚠️ 承認済み緩和条件を適用してシフト生成が完了しました！")
                                            .color(Color32::from_rgb(170, 110, 0))
                                            .strong(),
                                    );
                                } else {
                                    ui.label(
                                        RichText::new("🎉 全月のシフト生成が厳格条件で完了しました！")
                                            .color(Color32::from_rgb(0, 150, 0))
                                            .strong(),
                                    );
                                }
                                if let Some(secs) = self.completed_elapsed_secs {
                                    ui.label(format!("(総所要時間: {:.2}秒)", secs));
                                }
                            });

                            if let Some(results) = results_opt {
                                ui.horizontal_wrapped(|ui| {
                                    for (y, m, res) in results {
                                        let (tag, col) = if !res.success {
                                            (
                                                format!("❌ {}年{:02}月: 適切解なし", y, m),
                                                Color32::from_rgb(200, 40, 40),
                                            )
                                        } else if res.stage == 1 {
                                            (
                                                format!(
                                                    "✅ {}年{:02}月: 厳格達成 ({:.2}s)",
                                                    y, m, res.execution_time_secs
                                                ),
                                                Color32::from_rgb(0, 135, 0),
                                            )
                                        } else {
                                            (
                                                format!(
                                                    "⚠️ {}年{:02}月: 緩和{}件 ({:.2}s)",
                                                    y,
                                                    m,
                                                    res.approved_relaxations.len(),
                                                    res.execution_time_secs
                                                ),
                                                Color32::from_rgb(180, 100, 0),
                                            )
                                        };
                                        ui.label(RichText::new(tag).color(col).size(11.5));
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
                        ui.label(
                            RichText::new(format!("❌ エラー: {}", err))
                                .color(Color32::from_rgb(220, 50, 50))
                                .strong(),
                        );
                    }

                    ui.add_space(6.0);
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

    if let Some(custom) = candidates.iter().find(|p| {
        let n = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        n != "勤務条件表_ひな形.xlsx" && n != "勤務条件表_9.xlsx" && n != "勤務条件表_9月.xlsx"
    }) {
        return Some(custom.clone());
    }

    if let Some(tpl) = candidates
        .iter()
        .find(|p| p.file_name().and_then(|s| s.to_str()) == Some("勤務条件表_ひな形.xlsx"))
    {
        return Some(tpl.clone());
    }

    candidates.into_iter().next()
}

/// OS の既定アプリケーションでファイルを開く
pub fn open_file_in_os(path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/c", "start", "", &path_str]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd.spawn();
}

/// エクスプローラーでフォルダを開きファイルを選択表示する
pub fn open_folder_in_os(path: &Path) {
    if let Ok(abs_path) = std::fs::canonicalize(path) {
        let abs_str = abs_path.to_string_lossy().to_string();
        let clean_str = abs_str.strip_prefix(r"\\?\").unwrap_or(&abs_str);
        let _ = std::process::Command::new("explorer")
            .args(["/select,", clean_str])
            .spawn();
    } else {
        let _ = std::process::Command::new("explorer").arg(".").spawn();
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
            .with_inner_size([880.0, 740.0])
            .with_min_inner_size([680.0, 560.0])
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
