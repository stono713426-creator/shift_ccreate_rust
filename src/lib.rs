//! 勤務シフト自動生成システム コアライブラリ (Rust Edition)
//!
//! Version: v1.0.0
//! Updated: 2026-10-31 - 初期プロジェクト立ち上げ（Python版 sift_create v1.9.1 からの完全仕様移植準備）

/// システムバージョン定数 (`Cargo.toml` の version と自動同期)
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
