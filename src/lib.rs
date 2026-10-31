//! 勤務シフト自動生成システム コアライブラリ (Rust Edition)
//!
//! Version: v1.0.1
//! Updated: 2026-10-31 - Patch 修正: スペースなしハイフン日付範囲パース対応 (小島さんの特別有休反映等)
//!
//! 医療機関向けの複雑な勤務シフト制約を解決する純Rust製シフト最適化ライブラリ。

pub mod config;
pub mod generator;
pub mod gui;
pub mod holidays;
pub mod parser;
pub mod renderer;
pub mod solver;

/// システムバージョン定数 (`Cargo.toml` の version と自動同期)
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
