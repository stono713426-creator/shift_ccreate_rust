//! 勤務シフト自動生成システム エントリーポイント (`main.rs`)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 4 実装: GUI 起動（引数なし / --gui）および CLI バッチ実行のハイブリッド対応
//!
//! 使い方:
//!   - デスクトップ GUI 起動: `shift_create_rust.exe` (引数なし起動)
//!   - CLI バッチ実行      : `shift_create_rust.exe <入力Excelパス> [出力Excelパス]`
//!   - ヘルプ表示          : `shift_create_rust.exe --help`

use shift_core::generator::{GenerateOptions, ShiftGenerator};
use shift_core::gui::run_gui;
use shift_core::holidays::get_holidays;
use shift_core::parser::parse_shift_excel;
use shift_core::VERSION;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let args: Vec<String> = env::args().collect();

    // 引数なし、または --gui 指定時: デスクトップ GUI を起動
    if args.len() < 2 || (args.len() == 2 && args[1] == "--gui") {
        if let Err(e) = run_gui() {
            eprintln!("GUI の起動に失敗しました: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // ヘルプオプション判定
    if args[1] == "--help" || args[1] == "-h" {
        print_usage(&args[0]);
        return;
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = if args.len() >= 3 {
        Some(PathBuf::from(&args[2]))
    } else {
        None
    };

    run_cli_batch(&input_path, output_path);
}

/// CLI の使用方法を表示する
fn print_usage(prog_name: &str) {
    println!("============================================================");
    println!("  勤務シフト自動生成システム (Rust Edition {})", VERSION);
    println!("============================================================");
    println!("使い方:");
    println!("  {} [オプション | 入力Excelパス] [出力Excelパス]", prog_name);
    println!();
    println!("起動モード:");
    println!("  引数なし起動    : デスクトップ GUI (eframe / egui) を起動");
    println!("  CLI バッチ実行  : <入力Excelパス> を指定してバックグラウンド生成");
    println!();
    println!("引数・オプション:");
    println!("  --gui           : 明示的にデスクトップ GUI を起動");
    println!("  --help, -h      : このヘルプメッセージを表示");
    println!("  <入力Excelパス> : スタッフマスタ・制約条件・手術等を含む勤務条件表Excel");
    println!("  [出力Excelパス] : （任意）現場フォーマットで出力するシフト表Excelパス");
    println!("                    未指定時は '{{開始年下2桁}}年{{開始月}}-{{終了月}}月シフト.xlsx'");
    println!("============================================================");
}

/// CLI バッチ実行
fn run_cli_batch(input_path: &Path, output_path: Option<PathBuf>) {
    println!("============================================================");
    println!("  勤務シフト自動生成システム (Rust Edition {})", VERSION);
    println!("============================================================");

    if !input_path.exists() {
        eprintln!("エラー: 入力ファイルが存在しません: {}", input_path.display());
        std::process::exit(1);
    }

    let total_start = Instant::now();
    println!("1. 祝日カレンダーの取得・計算中...");
    let holidays = get_holidays(2024, 2030);
    println!("   祝日登録件数: {}件", holidays.len());

    println!("2. 勤務条件表の解析中: {}", input_path.display());
    let parse_start = Instant::now();
    let parsed = match parse_shift_excel(input_path, &holidays) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("エラー: 入力Excelの解析に失敗しました: {}", e);
            std::process::exit(1);
        }
    };
    println!(
        "   解析完了 ({:.2}秒): スタッフ数={}名, 制約数={}件, 手術日程={}日, 作成対象月={:?}",
        parse_start.elapsed().as_secs_f64(),
        parsed.staffs.len(),
        parsed.rules.len(),
        parsed.ope_dict.len(),
        parsed.target_months
    );

    if parsed.target_months.is_empty() {
        eprintln!("エラー: 手術スケジュールシート等から作成対象月が検出されませんでした。");
        std::process::exit(1);
    }

    println!("3. 複数月シフトの二段階数理最適化 (MILP / HiGHS) を開始...");
    let options = GenerateOptions {
        target_months: Some(parsed.target_months.clone()),
        output_path,
    };

    let result = ShiftGenerator::generate(&parsed, &holidays, &options, |idx, total, y, m, msg| {
        if y > 0 && m > 0 {
            println!("   [{}/{}] ({}年{:02}月) {}", idx + 1, total, y, m, msg);
        } else {
            println!("   {}", msg);
        }
    });

    match result {
        Ok((out_file, month_results)) => {
            println!("============================================================");
            println!("🎉 シフト生成が正常に完了しました！");
            println!("   出力ファイル: {}", out_file.display());
            println!("   総所要時間  : {:.2}秒", total_start.elapsed().as_secs_f64());
            println!("   生成月内訳  :");
            for (y, m, res) in month_results {
                let status_str = if res.success {
                    format!("成功 (Stage {}, {:.2}s)", res.stage, res.execution_time_secs)
                } else {
                    "失敗 (INFEASIBLE)".to_string()
                };
                println!("     - {}年{:02}月: {}", y, m, status_str);
            }
            println!("============================================================");
        }
        Err(e) => {
            eprintln!("============================================================");
            eprintln!("❌ エラー: シフト生成中に問題が発生しました: {}", e);
            eprintln!("============================================================");
            std::process::exit(1);
        }
    }
}
