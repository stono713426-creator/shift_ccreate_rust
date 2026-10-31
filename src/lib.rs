//! 勤務シフト自動生成システム コアライブラリ (Rust Edition)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - Phase 1 実装: config, holidays, parser モジュールの追加
//!
//! 医療機関向けの複雑な勤務シフト制約を解決する純Rust製シフト最適化ライブラリ。

pub mod config;
pub mod holidays;
pub mod parser;

/// システムバージョン定数 (`Cargo.toml` の version と自動同期)
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
