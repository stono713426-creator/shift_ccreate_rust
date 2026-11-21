# 勤務シフト自動生成システム (Rust Edition) システム仕様書

- **Version**: `v2.0.0`
- **Last Updated**: `2026-11-21`
- **概要**: 純 Rust 製・厳格数理最適化（MILP / HiGHS）＋ 原因診断・対話緩和選択型勤務シフト自動生成システム

本書は、`v2.0.0` におけるシステム構成、3層制約モデル（Tier 0 / Tier 1 / Tier 2）、原因診断＆承認制緩和フロー、ペナルティ重み定数、Excel 入出力フォーマット、および品質保証テスト仕様の完全定義です。

---

## 1. アーキテクチャとモジュール構成

```text
shift_create_rust/
├── Cargo.toml                  # クレート定義 (version = "2.0.0")
├── build.rs                    # Windows EXE バージョンリソース自動埋め込み (windres)
├── SPECIFICATION.md            # 本仕様書
├── README.md                   # プロジェクト概要・利用手順
├── 勤務条件表_ひな形.xlsx       # v2.0.0 標準勤務条件表テンプレート＆回帰テスト基準
├── examples/
│   └── generate_template.rs    # 勤務条件表_ひな形.xlsx 自動生成ツール
├── src/
│   ├── lib.rs                  # ライブラリルート (shift_core, VERSION = "v2.0.0")
│   ├── main.rs                 # GUI / CLI 統合エントリーポイント (動的祝日読込・無緩和CLI)
│   ├── config.rs               # 定数・Tier 2/診断重み・RelaxationKind・ShiftConfig (15項目)
│   ├── holidays.rs             # 祝日Web API取得＋内閣府計算フォールバック
│   ├── parser.rs               # Excel全シート解析・NFKC日付パーサー・属性/制約検証
│   ├── solver.rs               # HiGHS (MILP) 厳格最適化＆原因診断エンジン・品質監査
│   ├── renderer.rs             # rust_xlsxwriter シフト帳票＋未生成シート＋生成診断レポート出力
│   ├── generator.rs            # ShiftGenerationSession 対話ステップ生成・動的祝日補完
│   └── gui.rs                  # eframe/egui GUI (一時停止診断パネル・緩和チェックリスト)
└── tests/
    └── regression_suite.rs     # 統合回帰テストスイート全18件 (lib単体17件と合わせ計35件)
```

---

## 2. 定数・ペナルティ重み・動的設定 (`config.rs`)

### 2.1 10段階制約レベル重み (`LEVEL_WEIGHT_MAP`)
| レベルキー | ペナルティ重み (`i64`) | 適用仕様（v2.0.0） |
| :--- | ---: | :--- |
| `HARD1` | `20_000_000` | Tier 1 厳格ハード制約（承認緩和時のみスラック許可、診断重み `75_000_000`） |
| `HARD2` / `HARD` | `16_000_000` | Tier 1 厳格ハード制約（承認緩和時のみスラック許可、診断重み `55_000_000`） |
| `HARD3` | `14_000_000` | Tier 1 厳格ハード制約（承認緩和時のみスラック許可、診断重み `42_000_000`） |
| `HIGH1` | `12_000_000` | Tier 2 高優先ソフト制約（常時スラック最小化） |
| `HIGH2` / `HIGH` | `9_000_000` | `HIGH` は `HIGH2` 互換 |
| `HIGH3` | `6_000_000` | |
| `SOFT1` | `400_000` | |
| `SOFT2` / `SOFT` | `100_000` | 未定義キーのデフォルト値 |
| `SOFT3` | `50_000` | |
| `LOW` / `LOW1` | `20_000` | |

### 2.2 推奨人数 (`val_rec`) 不足ペナルティ重み (`REC_WEIGHT_MAP`)
- `HARD1`: `12_000_000`, `HARD2`/`HARD`: `11_000_000`, `HARD3`: `10_500_000`
- `HIGH1`: `10_200_000`, `HIGH2`/`HIGH`: `4_000_000`, `HIGH3`: `2_000_000`
- `SOFT1`: `400_000`, `SOFT2`/`SOFT`: `200_000`（デフォルト `200_000`）, `SOFT3`: `100_000`
- `LOW`/`LOW1`: `50_000`

### 2.3 システム固定ペナルティ定数（Tier 2 ＆ 承認緩和最小化）
| 定数名 | 値 | 用途 |
| :--- | ---: | :--- |
| `WEIGHT_APPROVED_RELAX_PENALTY` | `50_000_000` | 承認済み緩和が複数日・週に跨がる場合に必要最小限の使用に留める内部ペナルティ |
| `WEIGHT_ISHIBASHI_WED_OFF_SLACK` | `14_000_000` | 承認時：条件付水曜公休（非Ope・非注射週）を他曜日へ振り替えた際の抑制重み |
| `WEIGHT_SPECIAL_EXTRA_WORK` | `10_000_000` | 承認時：「必要に応じて週5」スタッフの超過出勤1日あたりペナルティ |
| `WEIGHT_GENERAL_SEC_SLACK` | `10_000_000` | 一般セクション（1階・2階表）許容ブレ幅逸脱ペナルティ |
| `WEIGHT_SECOND_OFF_SLIDE_SLACK` | `8_500_000` | 週4日×固定1日スタッフの第2公休曜日スライド抑制ペナルティ |
| `WEIGHT_OPE_2F_BACK_REC_PENALTY` | `5_500_000` | **①** Ope日の2階裏推奨体制（Ope専従者以外に `Ns>=2` かつ `検査>=2`）不足ペナルティ |
| `WEIGHT_EARLY_OVERLAP_FULLTIME_SLACK` | `3_500_000` | **②** 非Ope日に早上がりスタッフが重複出勤した際のフルタイム増員（+1名）不足ペナルティ |
| `WEIGHT_2F_BACK_WEEKLY_4_SLACK` | `2_500_000` | **③** 2階裏 通常週（非祝・6稼働日週）の4名体制最低日数不足ペナルティ |
| `WEIGHT_2F_BACK_5_PENALTY` | `2_000_000` | 2階裏（外来）5名以上出勤抑制ペナルティ |
| `WEIGHT_MAIN_OFF_SLIDE_SLACK` | `2_000_000` | OR型スタッフの週次主公休スライド抑制ペナルティ |
| `WEIGHT_1F_UNDER_7_PENALTY` | `1_500_000` | 1階 `M_s >= 6.5` のとき 6名以下となるペナルティ |
| `WEIGHT_HOLIDAY_WEEK_OFF_PREF` | `600_000` | 平日祝日を含む5診療日週における公休優先配分ソフト目標（月間所定日数は割り込まない） |
| `WEIGHT_SEC_BALANCE_SQ` | `500_000` | 一般セクション人数平準化（2乗項） |
| `WEIGHT_SEC_BALANCE_ABS` | `500_000` | 一般セクション人数平準化（平均 `round(M_s)` からの絶対誤差） |
| `WEIGHT_EARLY_OVERLAP_PENALTY` | `400_000` | 非Ope日の早上がりスタッフ重複回避ペナルティ |
| `WEIGHT_MAIN_OFF_MISMATCH` | `300_000` | 主公休曜日以外の平日に「公」が入る不一致ペナルティ |
| `WEIGHT_WEEKLY_OFF_DIFF` | `300_000` | カレンダー週（ISO週）単位の公休日数平準化ペナルティ |
| `WEIGHT_PREV_OFF_REPEAT` | `300_000` | 過去3ヶ月主公休ローテーション基準値（通常OR `[1.2M, 600k, 300k]`、週4固定1日 `[4M, 2M, 1M]`） |
| `WEIGHT_NURSE_LT2_PENALTY` | `200_000` | 2階裏 非Ope日の看護師推奨人数未満ペナルティ |
| `WEIGHT_ROLLING_WINDOW_DIFF` | `80_000` | 7暦日ローリング窓平準化（日祝・休診・有休・特有を除外した稼働可能日ベース） |
| `WEIGHT_2F_BACK_SOFT_BALANCE` | `50_000` | 2階裏（外来）の軽い平準化重み（3〜4名枠の配分を阻害しない範囲） |
| `WEIGHT_TAIL_OFF_SUM` | `30_000` | 月末3稼働日の公休集中抑制ペナルティ |

### 2.4 診断専用 MILP スラック重み (`DIAG_WEIGHT_*`)
厳格探索が `Infeasible` となった場合、`Tier 0` を絶対固定したうえで最小限の緩和候補を抽出するため、以下の優先重みで診断 MILP を求解する：

| 定数名 | 値 | 緩和種別 |
| :--- | ---: | :--- |
| `DIAG_WEIGHT_TARGET_DAYS_ADJUST` | `200_000_000` | **最終手段**：月間所定勤務日数 `±1` 日調整 (`TargetDaysAdjust`) |
| `DIAG_WEIGHT_OPE_DEDICATED` | `90_000_000` | Ope専従スタッフの Ope/注射日必須出勤免除 (`OpeDedicatedWork`) |
| `DIAG_WEIGHT_FIXED_OFF_SWAP` | `60_000_000` | 固定公休曜日の別日振替 (`FixedOffDaySwap`) |
| `DIAG_WEIGHT_2F_BACK_MIN` | `45_000_000` | 2階裏 外来最低総人数引き下げ (`Sec2fBackMinTotal`) |
| `DIAG_WEIGHT_2F_BACK_ROLE` | `40_000_000` | 2階裏 看護師・検査最低人数引き下げ (`Sec2fBackMinNurse` / `Sec2fBackMinExam`) |
| `DIAG_WEIGHT_WEEKLY_FLEX` | `25_000_000` | 週4祝日非連動の週次出勤目標±1日柔軟化 (`WeeklyTargetFlex`) |
| `DIAG_WEIGHT_CONDITIONAL_WED_OFF` | `20_000_000` | 非Ope水曜公休の他曜日振替 (`ConditionalWedOff`) |
| `DIAG_WEIGHT_SPECIAL_EXTRA_WORK` | `15_000_000` | 特別枠（`必要に応じて週5`）の週5出勤許可 (`SpecialExtraWork`) |

（※シート `HARD` 系制約の診断重み：`HARD1=75_000_000`, `HARD2/HARD=55_000_000`, `HARD3=42_000_000`）

### 2.5 緩和スコープ・緩和種別 (`RelaxationScope` / `RelaxationKind`)
- `RelaxationScope`: `Day(u32)`（特定日）、`Week(usize)`（ISO週一括）、`Month`（当月全体）
- `RelaxationKind`: `SpecialExtraWork`, `ConditionalWedOff`, `Sec2fBackMinTotal`, `Sec2fBackMinNurse`, `Sec2fBackMinExam`, `OpeDedicatedWork`, `FixedOffDaySwap`, `WeeklyTargetFlex`, `SheetHardRule`, `TargetDaysAdjust`
- `RelaxationProposal`: `{ kind, label, impact_desc, recommended, is_last_resort }`

### 2.6 動的設定構造体 (`ShiftConfig` 全15項目)
『設定』『システム設定』『パラメータ』『config』シートから読み込む（未存在時はデフォルト値）。
1. `sec_2f_back_base_cap` (`3`)
2. `nurse_min_count` (`1`)
3. `nurse_rec_count` (`2`)
4. `max_consecutive_work_days` (`5`)
5. `fixed_off_shortage_allowance` (`2` ※旧Excel読込互換のため保持。`v2.0.0` では無承認での所定割れ許容には使用しない)
6. `consecutive_off_level` (`"中"` / 強:`300_000`, 中:`100_000`, 弱:`30_000`, なし:`0`)
7. `sec_capacity_tolerance` (`1`)
8. `weekly_off_target` (`2`)
9. `existing_shift_weight_level` (`"中"` / 強:`1_000_000`, 中:`300_000`, 弱:`50_000`)
10. `warn_2f_back_count` (`2`)
11. `holiday_week_off_rule` (`true`)
12. `weekly_off_balance_rule` (`true`)
13. `sec_2f_back_weekly_4_min` (`2`)
14. `auto_rotate_off_rule` (`true`)
15. `specified_off_rule` (`true`)

---

## 3. 祝日判定仕様 (`holidays.rs` / `generator.rs`)

1. **Web API ＋ 内閣府オフライン完全補完**: 内閣府「国民の祝日について」準拠の計算を先行生成したうえで、`https://holidays-jp.github.io/api/v1/date.json`（タイムアウト 3 秒）取得成功時のみマージ。
2. **動的対象年範囲 (`v2.0.0`)**: `parse_excel_with_dynamic_holidays` / `holidays_for_months` / `initial_holidays` により、読み込んだ条件表の対象年範囲 `[min_y - 1 ..= max_y + 1]` をカバー（2031年以降の条件表にも対応）。

---

## 4. Excel データ解析・属性・入力検証仕様 (`parser.rs`)

### 4.1 日付リスト・PM有休・NFKC 正規化
- Excel シリアル値、単体日付セル（年省略時・対象月またぎ年の自動解決）、和暦（`令和X年...` / `RX/...`）、範囲指定（`YYYY/M/D-YYYY/M/D`・時刻付き範囲）に対応し、存在しない日付（例: `9/31`, 平年 `2/29`）は `parse_warnings` へ警告記録。
- **スタッフ勤務パターン `pattern` の NFKC 正規化**: 全角数字の「週４日」「週５日」等を自動的に半角へ正規化。
- **PM 有休 (`pm_leave_dates`) の厳格抽出**: `PM` / `午後` / `午後半休` / `午後休` / `後休` / `午後有休` などの明示的午後休表記のみを対象とし、単なる `半休`・`早退`・`午前半休` での土曜2名例外誤発動を防止。

### 4.2 スタッフマスタ「個別勤務ルール」属性列と旧互換フォールバック
`スタッフマスタ` に任意列 **`個別勤務ルール`**（別名: `個別ルール` / `特記ルール` / `勤務属性`）を導入：
- `早上がり`: 非Ope日の同時出勤抑制 ＆ 重複時フルタイム +1 増員（`is_early_leave`）
- `Ope専従`: Ope日・注射日必須出勤、Ope日の 2階裏外来カウント/数式除外（`is_ope_dedicated`）
- `週4祝日非連動`: 祝日週でも週4日出勤を維持（`is_holiday_independent_week4`）
- `非Ope水曜公休`: Ope・注射がない水曜は原則公休（`has_conditional_wed_off`）
- 未記入時は旧フォーマット互換フォールバック（`石橋` → 4属性、`長谷川` → `早上がり`）、明示 `"なし"` 指定時は無効化、未知キーワード時は警告記録。

### 4.3 シフト制約条件の検証・重複整理・計算バグ修正
- **未知適用条件エラー検出 (`validate_rule_condition_desc`)**: `"月末"`, `"第1金曜"`, `"月燿"` 等の認識不能文字列が「全日」へ誤フォールバックするバグを修正。`is_invalid_condition = true` を付与し、`solve_shift_with_relaxations` 開始時に Tier 0 エラーとして即停止・修正案内を行う。
- **複数曜日指定対応**: `"火曜・水曜"` 等の複数曜日条件をすべて適用対象日として抽出。
- **重複ルールの自動整理**: 2階裏Ope標準制約と重複していた旧ひな形ルール（石橋 / 堀・江原の「手術のある日」）を読み込み時に自動一本化し通知（新ひな形は 9 件に整理）。また除外スタッフにより有効対象が 1名となった `全日 >= 1` ルールも警告付きで自動無効化。
- **有効人数・同時休みバグ修正**:
  - 除外スタッフ発生時：`val_min - excluded_count` による不自然な下限崩壊を廃止し、`val_min.min(valid_count)` へ修正。
  - `同時休み不可` ルール：`max(val_min, N - val_min)` の誤式を廃止し、`val_min` をそのまま最低出勤人数（`sum_x >= eff_min`）として適用。

---

## 5. 3層・厳格数理最適化モデル＆原因診断仕様 (`solver.rs`)

### 5.1 3層モデルポリシー (`Tier 0 / Tier 1 / Tier 2`)
1. **Tier 0（絶対不変・無断緩和も選択肢提案も不可）**:
   - 除外スタッフ（全日 `x = 0`）、日曜・祝日・全員共通特別有休（`common_sp_vac`）・個人確定休暇（有休・特別有休・希望休・指定公休）は $x_{s,d} = 0$。
   - **月跨ぎ含む連勤上限 (`max_consecutive_work_days`)**: 前月末尾実績（年月タグが一致する前月シートのみ参照）を忠実に連結し、連続 $k+1$ 日の和 $\le k$ を絶対厳守。旧 `sanitize_prev_tail_for_staff` による前月連勤の無断0化や非コアスタッフの無断連勤スキップは完全廃止。
   - 非コアスタッフ希望出勤曜日の出勤固定（$x = 1$）。
2. **Tier 1（厳格ハード・不可時は「適切解なし」明示停止）**:
   - **コア月間所定勤務日数**: 希望休超過時でも `target_days` を無断クランプせず、厳密に $\sum x_{s,d} = \text{target}$ を要求（承認済み `TargetDaysAdjust` 時のみ $\pm 1$ 許容）。
   - **固定公休曜日 (`AND` / 週4固定1日)**: 固定曜日診療日数 $\le$ 月間許容公休数 (`workable - target`) の月は原則すべて固定公休。月5回等超過時は「共通公休数（月間所定日数）優先」で許容回数以上を固定公休とし、超過1日を出勤へ振り替えて所定一致＆連勤上限を厳守。
   - **祝日週公休 (`holiday_week_off_rule`)**: 所定日数を割り込まない範囲の **Tier 2 ソフト目標** (`WEIGHT_HOLIDAY_WEEK_OFF_PREF = 600_000`) へ変更（`小田切` 等の月間所定不足を解消）。
   - **2階裏（外来）日次総数・職種最低**:
     - 全非休診日で無断クランプなしの厳格ハード制約を適用（有休1名まで減算なし・2名以上減算）。
     - 火曜: $N_d \ge 2, E_d \ge 1$、木曜: $N_d \ge 1, E_d \ge 2$、土曜: $C_d \ge 3$（非OpeかつPM有休あり土曜は $C_d \ge 2$）。
     - Ope日: $C_d \ge 3, N_d \ge 1$、かつ **火曜Ope日は $E_d \ge 1$（長谷川火曜固定休を考慮）、火曜以外Ope日は $E_d \ge 2$**。
   - **個別ルール**: `is_ope_dedicated`（Ope/注射日出勤必須）、`is_holiday_independent_week4`（祝日非連動週4維持）、`has_conditional_wed_off`（非Ope・非注射週の水曜公休）、`is_special`（無承認時は週5禁止・週4厳守、承認週のみ週5許可）。
   - **制約条件シート `HARD` 系ルール**: 上限・下限ともに厳格ハード適用（承認時のみ `SheetHardRule` 緩和）。
3. **Tier 2（品質最適化ソフト制約）**:
   - **2階裏 4名枠優先順位**: **① Ope推奨 (`5.5M`) ＞ ② 非Ope早上がり重複フルタイム+1 (`3.5M`) ＞ ③ 通常週4名最低日数 (`2.5M`)**。
   - **暦3連休抑制**: 日祝を含む連続3暦日（全員休診の3連休・有休/特有を含む窓を除く）。
   - **7暦日ローリング平準化**: 日祝・共通休診・個人有休・特別有休を除外した稼働可能日ベースで純粋公休を平準化。

### 5.2 適切解なし時の診断フロー (`diagnose_infeasibility`)
1. `Strict` モードで `Infeasible` の場合、構造的原因（`ConflictCause`）を解析し、診断専用 MILP（`SolveMode::Diagnose`）を実行。
2. Tier 0 で既に破綻している場合（例: 非コア連勤上限競合、個人稼働可能日が所定 $\pm 1$ 日を超えて不足、無効適用条件）は `tier0_unsolvable = true` として設定見直しを案内。
3. Tier 1 競合の場合は `evaluate_diag_solution` により最小限の **推奨ピンポイント候補**（日/週単位）＋ **代替・週/月一括候補** ＋ **最終手段（所定±1日）** を抽出して `DiagnosisResult` を返却。
4. 解取得時は `build_audit_report` により `ShiftAuditReport`（適用済み緩和・所定不一致・週5発動・固定公休振替・推奨未達・主公休スライド・早上がり重複・2階裏警告日）を生成。

---

## 6. 現場フォーマット Excel 出力仕様 (`renderer.rs`)

- **出力シート構成**:
  1. 成功月: `"{year}年{month:02}月シフト"`（従来フォーマット・Excel数式 `COUNTIF`/`SUM`・印刷設定 `E3` 固定完全維持）。
     - **Ope日 2階裏人数式**: `sec2_ope_dedicated_cols` に基づき Ope専従スタッフ列を自動減算。
     - **警告列 (`⚠️`)**: クリニック全体休診日（`common_sp_vac`：年末年始・夏季休診）は `is_clinic_closure` として除外し誤表示を防止。
  2. 未生成月: `"{year}年{month:02}月_未生成"`（診断サマリー・原因一覧・提案緩和候補・実行ログを構造化出力）。
  3. **ブック末尾**: **`"生成診断レポート"`**（月別生成サマリー表・承認済み緩和条件一覧・適切解なし月の原因＆緩和候補・品質詳細ログを一括出力）。

---

## 7. 対話ステップ生成・GUI・CLI 仕様 (`generator.rs` / `gui.rs` / `main.rs`)

- **`ShiftGenerationSession`**: 対象月を 1 ヶ月ずつ最適化し、`NeedsRelaxation` 時に現在セッション状態・既解決月の末尾シフト＆3ヶ月主公休履歴を保持したまま停止。承認済み `HashSet<RelaxationKind>` を受けて同一月を再ソルブ、または `skip_current_month` で未生成記録して次月へ進む。
- **GUI (`eframe` / `egui`)**: 一時停止診断パネルで原因を表示し、「① 推奨ピンポイント緩和候補」「② 代替・週/月一括候補」「⚠️ ③ 最終手段（所定勤務日数±1日調整）」のチェックボックス、「✨ 推奨候補を一括選択」「🔄 選択した緩和条件で再試行」「⏭ この月をスキップ」「🛑 中止」を提供（タイムアウト/診断不可時もフォールバック操作可能）。
- **CLI**: `ShiftGenerator::generate` により無承認自動緩和を行わず一括生成し、未生成月の原因・緩和候補を標準出力および Excel レポートへ出力。

---

## 8. 品質保証テスト仕様 (`cargo test` 全35件)

- **ライブラリ単体テスト (`17件`)**: `config` (3), `holidays` (2), `parser` (9), `renderer` (2), `solver` (1)。
- **統合回帰テスト (`tests/regression_suite.rs` 全18件)**:
  - `test_01`〜`test_09`: 基本祝日・日付・指定公休・振替公休・祝日週（所定制約優先＋ORソフト配分）・2階裏4名枠・前月末尾引き継ぎ・第2公休ローテ・Excel帳票＆`生成診断レポート`出力・複数月生成。
  - `test_10`〜`test_13`: 2026/11 小島全休週の厳格停止＆緩和承認再生成・9件整理済み制約条件・4ヶ月ローテ・2階裏職種/早上がり重複/Ope推奨優先(4-A)・全7ヶ月生成（通常6ヶ月 `stage=1`＋11月 `stage=2`）。
  - `test_14`: `v2.0.0` 厳格停止（非コア連勤Tier 0停止、石橋前月5連勤+9/1 Ope診断＆承認解決、希望休超過無断クランプ廃止＆`TargetDaysAdjust`、週6日Ope診断＆解決、HARDペア全休診断＆解決、ひな形警告0件）。
  - `test_15`〜`test_17`: 単体Excel日付年解決、4種手術統一・`作成対象`列廃止、2階裏1名減算なし・堀通常月週4厳守。
  - **`test_18_v2_0_0_comprehensive_verification`**: 通常6ヶ月全スタッフ所定完全一致・2027/03 小田切(22/22)＆長谷川(22/22・火曜1回振替・5連勤以内)・全通常月堀週4厳守、`ShiftGenerationSession` による 2026/11 停止→診断検証→推奨承認→次月引き継ぎ→`生成診断レポート`＆休診日`⚠️`なし検証、および全バグ修正（除外ルール・同時休み・未知条件・PM半休厳格化・全角`週４日`・旧Excel互換）の完全検証。
