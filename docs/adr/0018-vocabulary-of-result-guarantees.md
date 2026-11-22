# ADR 0018: 結果の保証の語彙

- 状態: 採用
- 日付: 2026-11-06
- 関連: [ロードマップ §1.3, §1.4](../roadmap.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0003](0003-result-check-and-deadlines.md), [ADR 0015](0015-gkz-local-data-guarantees.md), [ADR 0016](0016-job-deadline-covers-result-check.md), [信頼性の監査](../audits/2026-11-05-reliability-audit.md)

## 背景

結果の検査は, operation ごとに別の形で記録している. 因数分解は `verified_product`, GKZ の系は `checks` の object, 級数は項目ごとの `status` である. 失敗した検査はジョブを失敗させるので, 成功した結果に残る印はほとんど常に真であり, 何を確かめたのか, 何を CAS の答えのまま記録したのかが区別されない. たとえば `generated_by_toric_and_euler` は Macaulay2 の判定をそのまま写したものだが, Rust が確かめた `euler_operators_match` と同じ場所に並んでいる.

ロードマップの Phase 3 (Verification Framework) は, operation ごとに `verified`, `partially_verified`, `unverified` などの保証の範囲を表すことを目的にしている. その前提として, 主張ごとの根拠の水準と, 結果全体の要約の 2 層の語彙を決める.

## 決定

2026-11-06 に利用者が次の語彙を認めた. 主張の水準は `checked`, `cross_checked`, `asserted` の 3 つで, 結果の意味に含まれない性質は `not_claimed` に並べる. 結果全体の要約は `verified`, `partially_verified`, `unverified` である.

**根拠の水準 (`level`)**

| 値 | 意味 |
|---|---|
| `checked` | AlgebraMesh の検査器が, spec の入力, CAS の出力, certificate だけを使い, 正確な演算で真と判定した. CAS は TCB に入らない. `recomputation` の場合は, Rust の再計算コードが TCB に入る |
| `cross_checked` | 申告した実装の系統 (`lineage`) が重ならない 2 つの producer の結果が, 一意な形か表示の係数で一致した. 証明ではなく, 独立性の仮定に立つ |
| `asserted` | 名前の付いた producer の答えをそのまま記録した. producer は, CAS の名前と版, driver の版, 関数名で特定する |

- `not_claimed` は, 結果の意味に含まれない性質を並べる欄である. 水準の順序には入らない. 例は, 収束, 解空間の基底であること, 重みが generic であることである.

**method ごとの水準の上限**

| method | 水準の上限 |
|---|---|
| `self_certifying`, `certificate` | `checked` |
| `recomputation` | `checked`. Rust の再計算コードが TCB に入ることを記録する |
| `theorem` | 次の 3 つがそろったときだけ `checked`: 定理の出典と番号を記録した. 利用者が命題を確かめた. 仮定の主張がすべて実効水準で `checked` である. そろわなければ, 仮定のうち最も弱い水準 |
| `independent_implementation` | `cross_checked` |
| `consistency` | 水準を与えない. 整合しなければ結果を断る |
| `reference` | 実行時の主張には使わない. 回帰試験だけに使う |

- `scope` は `exact` か `truncated {weight, order}` である.
- 既存の `verified_to_order` は, 次のように写す.
  - 自分の水準は `checked`, `scope` は `truncated` とする.
  - 実効水準は `gkz.toric_generates_IA` に従う. 【現状】ではこれが `asserted` なので, 実効水準も `asserted` になる.
- `depends_on` には前提の主張を書く. 実効水準は, 自分の水準と前提の実効水準のうち最も弱いものとする.
- **契約.**
  - operation ごとに, 主張の集合と `not_claimed` を `docs/claims.md` に書く. これを契約と呼ぶ.
  - `verified` を出せるのは, 利用者 (数学者) が命題を確かめたと記した operation だけである. 確かめるまでは, `partially_verified` が上限である.
  - この規則は, 既存の operation にも新しい operation にも当てる.
- **要約 (`summary`).** すべて実効水準で決める.

  | 要約 | 条件 |
  |---|---|
  | `verified` | 契約が確認済みで, 契約の主張がすべて実効水準で `checked` |
  | `partially_verified` | 実効水準で `checked` の主張と, そうでない主張がある. または, 契約が未確認 |
  | `unverified` | 実効水準で `checked` の主張が 1 つもない |

  - 同じ語を 2 つの層で使わないため, 主張の水準には `verified` を使わない.
  - 要約だけでは区別がつかないので, `claims` は常に返す.
- **fail-closed.**
  - 検査器の誤りや panic が `checked` になることはない. 【現状】の `check_result` は `JoinError` を `UNSUPPORTED_RESULT` にしており, この性質を保つ.
  - backend が自分で返した `guarantee` は捨てる. その中に `asserted` 以外の水準があれば, 結果を `UNSUPPORTED_RESULT` にする.
- **`checked` の行に結び付けるもの.** 次の 2 つを結び付ける.
  - それを支える監査の行 (`docs/audits/2026-11-05-reliability-audit.md` §4) か, 試験.
  - 変異の試験の名前.
- **変異の試験の規則.**
  - 変異は, 「変えた後のものが主張を偽にする」ことを試験の側で別に確かめてから使う.
  - 1 か所を書き換えても正しいままの certificate があるからである. 例は次の 2 つである.
    - ZZ 上で解がないことの y は, y_i に整数を足しても y A ∈ ZZ^n と y b ∉ ZZ を保つ.
    - M の第 i 列が 0 なら, K の第 i 行を変えても M K = 0 を保つ.
  - こうした certificate では, 次のような変異を使う.
    - y b の小数部分を 0 にする.
    - 主張の値そのもの (D の対角成分, H の成分) を変える.
  - 1 か所の書換えを必ず検出できる certificate もある. 多項式の積で効く certificate (C, N, 商, h_k) と, U V = I や B B' = I がそうである.
  - この区別は, V1 の CI の規則に書く.

### 新しい operation の結果の形

- `value` には, 数学的な値と `certificate` (object) だけを置く. 判定は `guarantee.claims` に置く. `checks` は, 既存の 4 つの operation にだけ互換のために残す (Q3).
- 引数 `certificate: "include" | "none"` を置き, 既定を `include` とする. `none` のときは, その certificate が支える主張を `asserted` にする.
- driver は, 64 bit を超える整数を 10 進の文字列で出す.
  - 数値で出した大きな整数は f64 になり, 検査で落ちるからである.
  - 64 bit を超える成分を含む例を, 回帰試験に置く.
- 大きさの扱いは次のとおりである. どの場合も, 黙って水準を下げることはしない.
  - B1 より前.
    - `value` が 8 MiB を超えるときは, certificate を `{"omitted": "certificate_too_large"}` に置き換える. 関係する主張は `asserted` にし, `reason` を記録する.
    - それでも 8 MiB を超えれば, `RESOURCE_LIMIT` とする.
  - B1 より後.
    - §3.4 の規則で, 値を外に出す.
    - `max_blob_bytes` を超える certificate は, B1 より前と同じく省く.
- `guarantee` の大きさは, 主張の種類の数で抑えられる. `guarantee` も `MAX_RESULT_BYTES` に数える.
- 検算の時間は, ジョブの締切に含める (ADR 0016).

## 結果

- 既存の 4 つの operation の `checks` と `verified_product` は, 互換のために残す. 主張への写像は V1 と V4 で行う.
- `verified` は, 利用者が契約を確かめるまで出ない. 確かめていない operation の要約は, 最大でも `partially_verified` である.
- 主張の ID と命題は `docs/claims.md` に置く (V1). ID はすべて提案であり, 各 milestone で確かめる.

## 確認

- 偽の backend (自分で `checked` を名乗る, 積が入力と違う, 重複度 0 の因子を返す) が, どれも `UNSUPPORTED_RESULT` になることを試験する (A1).
- `checked` の主張ごとに, 変異の試験を CI で強制する (V1).
