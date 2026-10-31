//! 勤務シフト自動生成システム 複数月連続生成サービス (`generator.rs`)
//!
//! Version: v1.2.0
//! Updated: 2026-11-01 - v1.2.0 前月シフト実績シートの年月整合ガード・過去最大3ヶ月主公休履歴の引き継ぎ実装
//! - v1.0.0 (2026-10-31): Phase 3 実装: 複数月連続生成・月跨ぎ実績引き継ぎ・Excel一括レンダリング
//!
//! - 入力Excelデータおよび対象月リスト（target_months）に基づく連続ソルブ
//! - 前月実績（末尾7日間の出勤フラグ＆過去最大3ヶ月の主公休曜日履歴）を次月に自動引き継ぎ
//! - SPECIFICATION.md セクション6 準拠のデフォルト出力ファイル名決定
//! - 進捗通知コールバック（CLI / GUI 対応）

use crate::parser::ParsedExcelData;
use crate::renderer::render_shifts_to_file;
use crate::solver::{solve_shift, MonthContext, SolveResult};
use chrono::NaiveDate;
use std::collections::HashMap;
use std::path::PathBuf;

/// シフト生成オプション
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// 生成対象月リスト（None の場合は parsed.target_months を使用）
    pub target_months: Option<Vec<(i32, u32)>>,
    /// 出力先 Excel パス（None の場合はデフォルト命名規則を使用）
    pub output_path: Option<PathBuf>,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            target_months: None,
            output_path: None,
        }
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

    /// 複数月のシフトを連続生成し、結果を単一の現場フォーマット Excel に出力する
    pub fn generate<F>(
        parsed: &ParsedExcelData,
        holidays: &HashMap<NaiveDate, String>,
        options: &GenerateOptions,
        mut progress_cb: F,
    ) -> Result<(PathBuf, Vec<(i32, u32, SolveResult)>), String>
    where
        F: FnMut(usize, usize, i32, u32, &str),
    {
        let months = match &options.target_months {
            Some(m) if !m.is_empty() => m.clone(),
            _ => {
                if !parsed.target_months.is_empty() {
                    parsed.target_months.clone()
                } else {
                    return Err("作成対象月が指定されていません。".to_string());
                }
            }
        };

        let output_path = match &options.output_path {
            Some(p) => p.clone(),
            None => PathBuf::from(Self::default_output_filename(&months)),
        };

        let total_months = months.len();
        let mut results = Vec::with_capacity(total_months);

        // 月跨ぎ実績を引き継ぎながら連続生成
        let mut working_parsed = parsed.clone();

        // 初期状態において prev_month_main_offs のみ設定されている場合は history にも同期
        for (s_name, &w) in &working_parsed.prev_month_main_offs {
            let entry = working_parsed
                .prev_month_main_offs_history
                .entry(s_name.clone())
                .or_default();
            if entry.is_empty() {
                entry.push(w);
            }
        }

        // 「前月シフト実績」シート由来の初期データを退避（シート記載年月の翌月のみ適用するガード用）
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

        for (idx, &(year, month)) in months.iter().enumerate() {
            // 年月連続性および「前月シフト実績」シートの対象年月整合チェック
            let is_contiguous_from_prev_solved = if idx > 0 {
                let (prev_y, prev_m) = months[idx - 1];
                let next_of_prev = if prev_m == 12 {
                    (prev_y + 1, 1)
                } else {
                    (prev_y, prev_m + 1)
                };
                next_of_prev == (year, month) && results.get(idx - 1).map(|r: &(i32, u32, SolveResult)| r.2.success).unwrap_or(false)
            } else {
                false
            };

            if !is_contiguous_from_prev_solved {
                match expected_from_sheet {
                    Some(exp_ym) if exp_ym != (year, month) => {
                        // 例: 前月実績シートが「2026年12月」なのに「2026年9月」から生成する場合はクリア
                        working_parsed.prev_month_tail_shifts.clear();
                        working_parsed.prev_month_main_offs.clear();
                        working_parsed.prev_month_main_offs_history.clear();
                    }
                    Some(exp_ym) if exp_ym == (year, month) => {
                        // 前月を解かずにシート記載年月の翌月（例: 2027年1月）を解く場合はシート実績を復元
                        working_parsed.prev_month_tail_shifts = sheet_tail.clone();
                        working_parsed.prev_month_main_offs = sheet_offs.clone();
                        working_parsed.prev_month_main_offs_history = sheet_hist.clone();
                    }
                    _ => {
                        if idx > 0 {
                            // シート年月不明かつ飛び月生成の場合は連勤引き継ぎのみクリア
                            working_parsed.prev_month_tail_shifts.clear();
                        }
                    }
                }
            }

            let month_label = format!("{}年{:02}月", year, month);
            progress_cb(
                idx,
                total_months,
                year,
                month,
                &format!("{} のシフトを最適化中...", month_label),
            );

            // ソルバー実行
            let solve_res = solve_shift(&working_parsed, year, month, holidays);

            if solve_res.success {
                progress_cb(
                    idx,
                    total_months,
                    year,
                    month,
                    &format!(
                        "{} の最適化が完了しました (所要時間: {:.2}s, Stage {})",
                        month_label, solve_res.execution_time_secs, solve_res.stage
                    ),
                );

                // 次月への実績引き継ぎ
                // 1. 当月主公休曜日 (main_offs) および過去最大3ヶ月分の履歴引き継ぎ
                working_parsed.prev_month_main_offs = solve_res.main_offs.clone();
                for (s_name, &w) in &solve_res.main_offs {
                    let hist = working_parsed
                        .prev_month_main_offs_history
                        .entry(s_name.clone())
                        .or_default();
                    hist.insert(0, w);
                    hist.truncate(3);
                }

                // 2. 当月末尾7日間の出勤実績 (tail_shifts) の引き継ぎ
                let month_ctx = MonthContext::new(year, month, holidays);
                let days_in_month = month_ctx.days_in_month;
                let tail_start = if days_in_month > 7 {
                    days_in_month - 6
                } else {
                    1
                };

                let mut new_tail_shifts = HashMap::new();
                for staff in &working_parsed.staffs {
                    if let Some(staff_shifts) = solve_res.shifts.get(&staff.name) {
                        let mut tail = Vec::new();
                        for d in tail_start..=days_in_month {
                            let val = staff_shifts.get(&d).copied().unwrap_or(0);
                            tail.push(val);
                        }
                        new_tail_shifts.insert(staff.name.clone(), tail);
                    }
                }
                working_parsed.prev_month_tail_shifts = new_tail_shifts;
            } else {
                progress_cb(
                    idx,
                    total_months,
                    year,
                    month,
                    &format!("{} のシフト生成に失敗しました（解なし）", month_label),
                );
            }

            results.push((year, month, solve_res));
        }

        // Excel レンダリング
        progress_cb(
            total_months,
            total_months,
            0,
            0,
            &format!("Excel ファイルを出力中: {}", output_path.display()),
        );

        render_shifts_to_file(&output_path, parsed, &results, holidays)
            .map_err(|e| format!("Excel ファイルの保存に失敗しました: {}", e))?;

        progress_cb(
            total_months,
            total_months,
            0,
            0,
            &format!("全工程完了: {}", output_path.display()),
        );

        Ok((output_path, results))
    }
}
