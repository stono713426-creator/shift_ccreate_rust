# 勤務シフト自動生成システム (Rust Edition)

> Version: v1.0.0  
> Last Updated: 2026-10-31  

医療機関向けの複雑な勤務シフト制約を数理最適化（MILP: HiGHS）によって高速解決し、現場フォーマットの Excel 帳票として出力する **100% 純Rust製ネイティブ・デスクトップアプリケーション** です（Python版 `sift_create` v1.9.1 の全仕様完全移植プロジェクト）。

---

## 開発ロードマップ

詳細な移植仕様・数式・書式定義はすべて [SPECIFICATION.md](./SPECIFICATION.md) に記載されています。

- [x] **初期セットアップ**: プロジェクト構成、Git 初期化、Windows `.exe` バージョン情報自動埋め込み (`build.rs`)、完全移植仕様書 (`SPECIFICATION.md`) 策定
- [x] **Phase 1**: 設定 (`config.rs`)・祝日計算 (`holidays.rs`)・Excel 全シート＆NFKC日付パーサー (`parser.rs`) の実装と実データ読込テスト
- [x] **Phase 2**: 二段階数理最適化ソルバー (`solver.rs` / `good_lp` + `highs`) の実装と全7項目の回帰テスト合格
- [ ] **Phase 3**: 現場フォーマット Excel 描画 (`renderer.rs` / `rust_xlsxwriter`)・複数月連続生成サービス (`generator.rs`)・CLI 対応
- [ ] **Phase 4**: モダンデスクトップ GUI (`gui.rs` / `eframe`)・単体 `.exe` リリースビルド・配布 ZIP パッケージング

---

## ビルドと実行

```powershell
# 開発ビルド・動作確認
cargo run

# テスト一括実行
cargo test

# 配布用最適化リリースビルド (バージョン情報リソース自動埋め込み)
cargo build --release
```
