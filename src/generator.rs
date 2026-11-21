//! 勤務シフト自動生成システム 複数月連続生成サービス (`generator.rs`)
//!
//! Version: v2.0.0
//! Updated: 2026-11-21 - v2.0.0 セッション型対話生成・月別承認緩和引き継ぎ・祝日年範囲動的化
//! - v1.3.1 (2026-11-10): 対象月重複排除・事前診断ログ通知・Excelファイルロック時の自動別名保存フォールバック対応
//! - v1.3.0 (2026-11-08): 2階裏配置制約・石橋さん/長谷川さん個別ルール改修リリース同期
//! - v1.2.0 (2026-11-01): 前月シフト実績シートの年月整合ガード・過去最大3ヶ月主公休履歴の引き継ぎ実装
//! - v1.0.0 (2026-10-31): Phase 3 実装: 複数月連続生成・月跨ぎ実績引き継ぎ・Excel一括レンダリング

use crate::config::RelaxationKind;
use crate::holidays::get_holidays;
use crate::parser::{parse_shift_excel, ParsedExcelData};
use crate::renderer::render_shifts_to_file;
use crate::solver::{solve_shift_with_relaxations, MonthContext, SolveResult};
use chrono::{Datelike, NaiveDate};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 初期読み込み用の広い祝日カレンダー範囲を取得する（現在年 - 2 〜 現在年 + 5）
pub fn initial_holidays() -> HashMap<NaiveDate, String> {
    let cur_y = chrono::Local::now().year();
    get_holidays(cur_y - 2, cur_y + 5)
}

/// 対象年月リストに合わせて必要な祝日カレンダーを動的取得する
pub fn holidays_for_months(months: &[(i32, u32)]) -> HashMap<NaiveDate, String> {
    if months.is_empty() {
        return initial_holidays();
    }
    let min_y = months.iter().map(|&(y, _)| y).min().unwrap();
    let max_y = months.iter().map(|&(y, _)| y).max().unwrap();
    get_holidays(min_y - 1, max_y + 1)
}

/// 対象年月に応じた動的祝日カレンダーで勤務条件表 Excel を読み込む
pub fn parse_excel_with_dynamic_holidays<P: AsRef<Path>>(
    excel_path: P,
) -> Result<(ParsedExcelData, HashMap<NaiveDate, String>), String> {
    let path = excel_path.as_ref();
    let cur_y = chrono::Local::now().year();
    let mut holidays = get_holidays(cur_y - 2, cur_y + 5);
    let mut parsed = parse_shift_excel(path, &holidays)?;

    let out_of_range = parsed
        .target_months
        .iter()
        .any(|&(y, _)| y < cur_y - 2 || y > cur_y + 5);
    if out_of_range {
        holidays = holidays_for_months(&parsed.target_months);
        parsed = parse_shift_excel(path, &holidays)?;
    } else if !parsed.target_months.is_empty() {
        holidays = holidays_for_months(&parsed.target_months);
    }

    Ok((parsed, holidays))
}

/// シフト生成オプション
#[derive(Debug, Clone, Default)]
pub struct GenerateOptions {
    /// 生成対象月リスト（None の場合は parsed.target_months を使用）
    pub target_months: Option<Vec<(i32, u32)>>,
    /// 出力先 Excel パス（None の場合はデフォルト命名規則を使用）
    pub output_path: Option<PathBuf>,
    /// 月別の事前承認済み緩和条件マップ
    pub approved_relaxations_by_month: HashMap<(i32, u32), HashSet<RelaxationKind>>,
}

/// 1ヶ月ごとの実行ステップ結果
#[derive(Debug, Clone)]
pub enum MonthStepOutcome {
    /// 当該月のシフトが確定し、次月へ進んだ（または全月完了）
    Solved {
        year: i32,
        month: u32,
        result: SolveResult,
        has_next: bool,
    },
    /// 厳格条件では適切解がなく、緩和選択またはスキップが必要
    NeedsRelaxation {
        year: i32,
        month: u32,
        result: SolveResult,
    },
    /// すでに全月の処理が完了している
    AllDone,
}

/// 複数月生成を月単位で一時停止・緩和再試行できる対話対応セッション
#[derive(Debug, Clone)]
pub struct ShiftGenerationSession {
    original_parsed: ParsedExcelData,
    working_parsed: ParsedExcelData,
    holidays: HashMap<NaiveDate, String>,
    months: Vec<(i32, u32)>,
    output_path: PathBuf,
    current_idx: usize,
    results: Vec<(i32, u32, SolveResult)>,
    sheet_tail: HashMap<String, Vec<u8>>,
    sheet_offs: HashMap<String, u32>,
    sheet_hist: HashMap<String, Vec<u32>>,
    expected_from_sheet: Option<(i32, u32)>,
}

impl ShiftGenerationSession {
    /// 生成セッションを初期化する
    pub fn new(
        parsed: &ParsedExcelData,
        holidays: &HashMap<NaiveDate, String>,
        options: &GenerateOptions,
    ) -> Result<Self, String> {
        let raw_months = match &options.target_months {
            Some(m) if !m.is_empty() => m.clone(),
            _ => {
                if !parsed.target_months.is_empty() {
                    parsed.target_months.clone()
                } else {
                    return Err("作成対象月が指定されていません。".to_string());
                }
            }
        };

        // 同一対象月の重複排除（出現順序維持）
        let mut months = Vec::with_capacity(raw_months.len());
        for ym in raw_months {
            if !months.contains(&ym) {
                months.push(ym);
            }
        }

        let output_path = match &options.output_path {
            Some(p) => p.clone(),
            None => PathBuf::from(ShiftGenerator::default_output_filename(&months)),
        };

        let mut working_parsed = parsed.clone();
        for (s_name, &w) in &working_parsed.prev_month_main_offs {
            let entry = working_parsed
                .prev_month_main_offs_history
                .entry(s_name.clone())
                .or_default();
            if entry.is_empty() {
                entry.push(w);
            }
        }

        let sheet_tail = working_parsed.prev_month_tail_shifts.clone();
        let sheet_offs = working_parsed.prev_month_main_offs.clone();
        let sheet_hist = working_parsed.prev_month_main_offs_history.clone();
        let expected_from_sheet = parsed.prev_sheet_year_month.map(|(py, pm)| {
            if pm == 12 {
                (py + 1, 1)
            } else {
                (py, pm + 1)
            }
        });

        Ok(Self {
            original_parsed: parsed.clone(),
            working_parsed,
            holidays: holidays.clone(),
            months,
            output_path,
            current_idx: 0,
            results: Vec::new(),
            sheet_tail,
            sheet_offs,
            sheet_hist,
            expected_from_sheet,
        })
    }

    pub fn total_months(&self) -> usize {
        self.months.len()
    }

    pub fn current_idx(&self) -> usize {
        self.current_idx
    }

    pub fn current_month(&self) -> Option<(i32, u32)> {
        self.months.get(self.current_idx).copied()
    }

    pub fn results(&self) -> &[(i32, u32, SolveResult)] {
        &self.results
    }

    pub fn output_path(&self) -> &Path {
        &self.output_path
    }

    fn prepare_working_parsed_for_current(&mut self) {
        let idx = self.current_idx;
        let (year, month) = self.months[idx];

        let is_contiguous_from_prev_solved = if idx > 0 {
            let (prev_y, prev_m) = self.months[idx - 1];
            let next_of_prev = if prev_m == 12 {
                (prev_y + 1, 1)
            } else {
                (prev_y, prev_m + 1)
            };
            next_of_prev == (year, month)
                && self
                    .results
                    .get(idx - 1)
                    .map(|r| r.2.success)
                    .unwrap_or(false)
        } else {
            false
        };

        if !is_contiguous_from_prev_solved {
            match self.expected_from_sheet {
                Some(exp_ym) if exp_ym != (year, month) => {
                    self.working_parsed.prev_sheet_year_month = None;
                    self.working_parsed.prev_month_tail_shifts.clear();
                    self.working_parsed.prev_month_main_offs.clear();
                    self.working_parsed.prev_month_main_offs_history.clear();
                }
                Some(exp_ym) if exp_ym == (year, month) => {
                    self.working_parsed.prev_sheet_year_month =
                        self.original_parsed.prev_sheet_year_month;
                    self.working_parsed.prev_month_tail_shifts = self.sheet_tail.clone();
                    self.working_parsed.prev_month_main_offs = self.sheet_offs.clone();
                    self.working_parsed.prev_month_main_offs_history = self.sheet_hist.clone();
                }
                _ => {
                    if idx > 0 {
                        self.working_parsed.prev_sheet_year_month = None;
                        self.working_parsed.prev_month_tail_shifts.clear();
                    }
                }
            }
        }
    }

    /// 現在の対象月を1ヶ月分だけソルブする
    pub fn solve_current_month<F>(
        &mut self,
        approved_relaxations: &HashSet<RelaxationKind>,
        mut progress_cb: F,
    ) -> MonthStepOutcome
    where
        F: FnMut(usize, usize, i32, u32, &str),
    {
        if self.current_idx >= self.months.len() {
            return MonthStepOutcome::AllDone;
        }

        let idx = self.current_idx;
        let total = self.months.len();
        let (year, month) = self.months[idx];
        self.prepare_working_parsed_for_current();

        let month_label = format!("{}年{:02}月", year, month);
        progress_cb(
            idx,
            total,
            year,
            month,
            &format!("{} のシフトを厳格最適化中...", month_label),
        );

        let solve_res = solve_shift_with_relaxations(
            &self.working_parsed,
            year,
            month,
            &self.holidays,
            approved_relaxations,
        );

        for diag_log in &solve_res.logs {
            if diag_log.starts_with("⚠️") || diag_log.starts_with("❌") {
                progress_cb(idx, total, year, month, diag_log);
            }
        }

        if solve_res.success {
            let mode_str = if solve_res.stage == 1 {
                "厳格達成".to_string()
            } else {
                format!("承認緩和 {} 件適用", approved_relaxations.len())
            };
            progress_cb(
                idx,
                total,
                year,
                month,
                &format!(
                    "{} の最適化が完了しました ({}, 所要時間: {:.2}s)",
                    month_label, mode_str, solve_res.execution_time_secs
                ),
            );

            // 次月への実績引き継ぎ
            self.working_parsed.prev_month_main_offs = solve_res.main_offs.clone();
            for (s_name, &w) in &solve_res.main_offs {
                let hist = self
                    .working_parsed
                    .prev_month_main_offs_history
                    .entry(s_name.clone())
                    .or_default();
                hist.insert(0, w);
                hist.truncate(3);
            }

            let month_ctx = MonthContext::new(year, month, &self.holidays);
            let days_in_month = month_ctx.days_in_month;
            let tail_start = if days_in_month > 7 {
                days_in_month - 6
            } else {
                1
            };

            let mut new_tail_shifts = HashMap::new();
            for staff in &self.working_parsed.staffs {
                if let Some(staff_shifts) = solve_res.shifts.get(&staff.name) {
                    let mut tail = Vec::new();
                    for d in tail_start..=days_in_month {
                        let val = staff_shifts.get(&d).copied().unwrap_or(0);
                        tail.push(val);
                    }
                    new_tail_shifts.insert(staff.name.clone(), tail);
                }
            }
            self.working_parsed.prev_sheet_year_month = Some((year, month));
            self.working_parsed.prev_month_tail_shifts = new_tail_shifts;

            self.results.push((year, month, solve_res.clone()));
            self.current_idx += 1;

            MonthStepOutcome::Solved {
                year,
                month,
                result: solve_res,
                has_next: self.current_idx < self.months.len(),
            }
        } else {
            progress_cb(
                idx,
                total,
                year,
                month,
                &format!(
                    "{} は現在の条件では適切解がありません（原因診断・緩和選択待機）",
                    month_label
                ),
            );
            MonthStepOutcome::NeedsRelaxation {
                year,
                month,
                result: solve_res,
            }
        }
    }

    /// 現在の月を「未生成（適切解なし）」としてスキップし、次月へ進む
    pub fn skip_current_month(&mut self, failed_result: SolveResult) {
        if let Some(&(year, month)) = self.months.get(self.current_idx) {
            self.results.push((year, month, failed_result));
            self.current_idx += 1;
        }
    }

    /// 蓄積された全月結果を Excel ファイルへ出力する
    pub fn finalize_excel<F>(
        &self,
        mut progress_cb: F,
    ) -> Result<(PathBuf, Vec<(i32, u32, SolveResult)>), String>
    where
        F: FnMut(usize, usize, i32, u32, &str),
    {
        let total_months = self.months.len();
        if let Some(parent) = self.output_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                let _ = std::fs::create_dir_all(parent);
            }
        }

        progress_cb(
            total_months,
            total_months,
            0,
            0,
            &format!("Excel ファイルを出力中: {}", self.output_path.display()),
        );

        let mut actual_saved_path = self.output_path.clone();
        if let Err(primary_err) = render_shifts_to_file(
            &self.output_path,
            &self.original_parsed,
            &self.results,
            &self.holidays,
        ) {
            let parent_dir = self
                .output_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            let stem = self
                .output_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("シフト出力");
            let ext = self
                .output_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("xlsx");

            let mut saved_alt = None;
            for alt_idx in 1..=10 {
                let alt_name = format!("{stem}_別名保存_{alt_idx}.{ext}");
                let alt_path = if parent_dir.as_os_str().is_empty() {
                    PathBuf::from(alt_name)
                } else {
                    parent_dir.join(alt_name)
                };
                if render_shifts_to_file(
                    &alt_path,
                    &self.original_parsed,
                    &self.results,
                    &self.holidays,
                )
                .is_ok()
                {
                    saved_alt = Some(alt_path);
                    break;
                }
            }

            if let Some(alt_path) = saved_alt {
                progress_cb(
                    total_months,
                    total_months,
                    0,
                    0,
                    &format!(
                        "⚠️ 出力先ファイルが他のプログラム（Excel等）で開かれていたため、別名ファイルへ自動保存しました: {}",
                        alt_path.display()
                    ),
                );
                actual_saved_path = alt_path;
            } else {
                return Err(format!(
                    "Excel ファイルの保存に失敗しました（出力先がExcel等で開かれていないか確認してください）: {}",
                    primary_err
                ));
            }
        }

        progress_cb(
            total_months,
            total_months,
            0,
            0,
            &format!("全工程完了: {}", actual_saved_path.display()),
        );

        Ok((actual_saved_path, self.results.clone()))
    }
}

/// 複数月連続生成サービス
pub struct ShiftGenerator;

impl ShiftGenerator {
    /// デフォルトの出力ファイル名を算出する
    /// 例: 2026年9月〜2026年9月 -> "26年09-09月シフト.xlsx"
    /// 例: 2026年9月〜2027年3月 -> "26年09-03月シフト.xlsx"
    pub fn default_output_filename(months: &[(i32, u32)]) -> String {
        if months.is_empty() {
            return "シフト出力.xlsx".to_string();
        }
        let (first_y, first_m) = months.first().unwrap();
        let (_last_y, last_m) = months.last().unwrap();
        let short_y = first_y % 100;
        format!("{:02}年{:02}-{:02}月シフト.xlsx", short_y, first_m, last_m)
    }

    /// 複数月のシフトを連続生成し、結果を単一の現場フォーマット Excel に出力する（CLI / 一括実行用：無承認での自動緩和は行わない）
    pub fn generate<F>(
        parsed: &ParsedExcelData,
        holidays: &HashMap<NaiveDate, String>,
        options: &GenerateOptions,
        mut progress_cb: F,
    ) -> Result<(PathBuf, Vec<(i32, u32, SolveResult)>), String>
    where
        F: FnMut(usize, usize, i32, u32, &str),
    {
        let mut session = ShiftGenerationSession::new(parsed, holidays, options)?;

        while let Some((year, month)) = session.current_month() {
            let approved = options
                .approved_relaxations_by_month
                .get(&(year, month))
                .cloned()
                .unwrap_or_default();

            match session.solve_current_month(&approved, &mut progress_cb) {
                MonthStepOutcome::Solved { .. } => {}
                MonthStepOutcome::NeedsRelaxation { result, .. } => {
                    session.skip_current_month(result);
                }
                MonthStepOutcome::AllDone => break,
            }
        }

        session.finalize_excel(&mut progress_cb)
    }
}
