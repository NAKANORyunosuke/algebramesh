# ADR 0031: 古い記録の写像と, 記録の再検証

- 状態: 採用 (関数 `Registry::legacy_guarantee`, `Registry::check_again`, `OperationVerifier::legacy_claims`, module `algebramesh_verify::recheck`, CLI `algebramesh verify` とその出力の key の名前は提案)
- 日付: 2026-11-23
- 関連: [ロードマップ V4, §1.5](../roadmap.md), Issue #108, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0020](0020-cluster-feature-agreement.md), [ADR 0021](0021-content-addressed-blob-store.md), [ADR 0028](0028-gkz-package-and-its-checks.md), [ADR 0029](0029-cheap-certificates-for-three-claims.md), [主張の一覧](../claims.md)

## 背景

- `guarantee` を持たない記録が 2 種類ある.
  - 保証を記録する前 (#47 より前) の binary が書いた記録.
  - #47 以後の binary が, cluster が機能 `result_guarantee` を有効にする前に書いた記録. agent はこの欄を外して報告する (ADR 0020).
- 既存の 4 つの operation (`polynomial.factor`, `gkz.generate`, `gkz.indicial`, `gkz.series`) の値は, 書いた時点の検査の結果を欄に持つ. 欄は `verified_product`, `checks.*`, 級数の `status`, `parameters`, `canonical` などである. 検査に落ちた結果はジョブを失敗させたので, 成功した記録の欄は, その検査に通ったことを表す.
- 新しい operation の値は, 判定を `guarantee` にだけ置き, 値の欄には持たない (ADR 0018).
- 値には後から欄が増えた. `gkz.series` の `evidence_terms` (ADR 0028) と, `gkz.generate` と `gkz.series` の `certificate` (ADR 0029) である. 今の検査器は, `evidence_terms` のない `not_a_solution` の級数を断る. そのため, 古い記録をそのまま検査し直すと失敗する.
- ADR 0021 §13 (Q26) は, store から読み直して検査し直すことを V4 と V5 に回した.

## 決定

**2 つの読み方.** 記録が何を主張したかと, 今何が言えるかを分ける.

| 読み方 | 入力 | すること |
|---|---|---|
| `Registry::legacy_guarantee(operation, value, driver_version)` | 記録の値と driver の版 | 値の欄が記録する検査を, 主張と水準に写す. spec に対しては何も確かめない |
| `Registry::check_again(spec, value, backend, interrupt)` | spec と値 | agent と同じ検査器で検査し直す. 値は変えない |

- どちらも `algebramesh-verify` の `Registry` の method とした. GKZ の検査器は package の crate (`algebramesh-gkz`) にあり, 同じ registry に登録される. 写像も検査器ごとに持たせたので, 自由な関数ではなく registry に置いた.
- CLI は, agent と同じ `algebramesh_agent::registry()` を使う. package の検査器もこれに含まれる.

**`legacy_guarantee` の規則.**
- 検査器は `OperationVerifier::legacy_claims(value, context)` を持つ. 既定は `None` である.
- 主張の集合, method, `depends_on`, `scope` は, 今の検査器が同じ形の値に与えるものと同じにする. 水準は次の規則で決める.
  - 値の欄が, 書いた時点の検査に通ったことを記録する主張は `checked` とする. `by` は記録を書いた driver の版で, `reason` に欄の名前を書く (`Context::recorded`).
  - 値だけから決まる主張は, 写像の中で確かめて `checked` とする. `factor.domain`, `factor.primitive`, `factor.non_associate` がそうである. これらに対応する欄はない. #47 より前の backend は積だけを確かめたからである.
  - certificate が支える主張は `asserted` とする. `certificate.membership[j]` は spec の作用素 P_j を使うので, 値だけでは確かめられない. `certificate.independence` も写像では確かめない.
  - `evidence_terms` のない `not_a_solution` の級数が 1 つでもあれば, `series.not_a_solution` を `asserted` とし, 理由を書く.
  - 欄が偽であるか, 値が読めないときは, 誤り (`UNSUPPORTED_RESULT`) とする. 検査器が断るのと同じである.
- operation ごとの根拠は次のとおりである.

  | operation | `checked` とその根拠 | `asserted` |
  |---|---|---|
  | `polynomial.factor` | `factor.product`: `verified_product`. `factor.domain`, `factor.primitive` (ZZ のとき), `factor.non_associate`: 値から確かめる | `factor.irreducible` |
  | `gkz.generate` | `gkz.euler`: `checks.euler_operators_match`. `gkz.toric_in_IA`: `checks.toric_binomials_valid` | `gkz.toric_generates_IA`, `gkz.generated_by_toric_and_euler`, `gkz.membership[j]` |
  | `gkz.indicial` | `local.exponent_equations`, `local.exponent_roots`: `checks.exponents_are_roots`. `local.parameters`: `parameters` | `local.indicial_ideal`, `local.roots_complete`, `local.rank`, `local.rank_sum` |
  | `gkz.series` | `gkz.indicial` と同じ主張. `gkz.toric_in_IA`: `checks.toric_binomials_valid`. `series.shape`, `series.euler_exact`, `series.toric_to_order`, `series.not_a_solution`: 級数の `status`. `series.canonical`: `canonical` | `gkz.toric_generates_IA`, `gkz.generated_by_toric_and_euler`, `series.log_independent`, `series.missing`. `evidence_terms` のない級数があれば `series.not_a_solution` |

- 新しい operation の記録で `guarantee` を欠くものは, 主張 `value` 1 つを `asserted` とし, 理由 (`NOT_RECORDED`) を書く.
  - ロードマップは「すべての主張を `asserted` とする」とした. しかし, spec なしでは値がどの主張をするかが決まらない. 例: `matrix.solve` の値は, `solve.solution` か `solve.no_solution` のどちらかを主張する. 一覧の主張をすべて並べると, producer が答えていない主張を `asserted` として記録することになる.
  - 主張ごとの水準は, `algebramesh verify` が検査し直して出す. 表の記録の側は `-` になる.

**検査し直すときの古い値.**
- `Context::recheck` を足した. `check_again` はこれを立てる. agent の検査 (`ResultVerifier::verify`) は立てない.
- 立っているとき, `SeriesVerifier` は `evidence_terms` のない `not_a_solution` の級数を断らない. 代わりに `series.not_a_solution` を `asserted` にし, 理由を書く. agent は今も `evidence_terms` を書くので, agent の側では断る.
- certificate を持たない値は, もともと断られない (ADR 0029). 主張は `asserted` になる.
- したがって, 古い値について `legacy_guarantee` と `check_again` は同じ水準を出す.

**`recheck`.**
- `algebramesh_verify::recheck::recheck(registry, record, bodies, interrupt)` が, 記録と, 呼び手が取得した blob の本体から, 答え `Recheck` を作る. 網は読まない. 記録は `&JobRecord` で受け取り, 変えない.
- 手順は次のとおりである.
  1. `input_sha256` が `input_normalized` の SHA-256 であることと, `input_normalized` が記録の spec であることを確かめる.
  2. 記録の marker を集める. 場所は `result`, `result.<key>`, `arguments.<key>` である (ADR 0021 §3).
  3. 各本体について, 大きさ, SHA-256, canonical JSON であることを確かめる. 取得が `DATA_LOSS` で終わった本体は, `data_loss` と報告する.
  4. 1 と 3 のどれかが合わなければ, 何も検査しない (`not_checked`).
  5. 成功していないジョブと, 結果のないジョブには, `cannot_recheck` と答える.
  6. 記録が `guarantee` を持たなければ, `legacy_guarantee` を記録の側に置く.
  7. 値の `certificate` が `{"omitted": ...}` なら, `cannot_recheck` と答える. 省いた certificate が支えた主張は, 検査し直せないからである. 別の値は出さない.
  8. marker を本体に置き換えた spec と値を, `check_again` で検査し直す. 断られれば `refused` と答える.

**CLI `algebramesh verify <job-id> [--format table|json]`.**
- `Result` を `accept_blob_refs` 付きで呼び, marker のまま記録を取る. 各 blob は `GetBlob` で取る (`Client::blob`). 取得の途中で SHA-256 が合わなければ client が誤りにするので, それを `hash_mismatch` と読む. 本体は `recheck` がもう一度数える.
- 検査は blocking の thread で行う.
- table の出力は, job, input, blob ごとの状態, 記録の要約, 再検証の要約, 主張ごとの記録の水準と再検証の水準 (実効水準が低いときは併記), 水準の差の数である.
- JSON の key は次のとおりである.
  - `job_id`, `operation`, `state`.
  - `input`: `status` (`ok`, `hash_mismatch`, `spec_mismatch`) と `message`.
  - `blobs[]`: `place`, `id`, `size`, `status` (`ok`, `hash_mismatch`, `size_mismatch`, `not_canonical`, `data_loss`, `unreadable`), `message`.
  - `recorded`: `source` (`record`, `legacy_mapping`, `legacy_refused`, `nothing`) と, `guarantee`, `error`, `reason` のどれか.
  - `recheck`: `status` (`rechecked`, `cannot_recheck`, `refused`, `not_checked`) と, `guarantee`, `reason`, `error` のどれか.
- 終了 status は, 0 が再検証に通ったとき, 1 が通らなかったときと再検証できなかったとき, 2 が記録を読めなかったときである.
- 値は出さない. 記録は読むだけで, 変えない.

## 結果

- `guarantee` を持たない記録も, 主張の表で読める. 既存の 4 つの operation では, 同じ結果の新しい記録と同じ水準になる. ただし, 古い値にない certificate と `evidence_terms` が支える主張は `asserted` になる.
- store に置いた結果を, 後から手元の検査器で検査し直せる. 本体が失われていれば `DATA_LOSS` と答え, 別の値は返さない.
- 検査器が変わった後は, 記録の水準と今の水準の差が表に出る. 例: #47 と #102 の間に記録した `factor.non_associate` は, 記録で `asserted`, 再検証で `checked` になる.
- 検証ジョブ (V5) は, 同じ `recheck` を agent の中で使える. 取得の部分だけを `AgentGateway.GetRecord` と `GetBlob` に替えればよい.

## 採らなかった案

- **写像の中で spec に対して検査し直す.** 記録が何を主張したかと, 今何が言えるかが混ざる. 前者を写像, 後者を再検証に分けた.
- **欄のない主張 (`factor.domain` など) を `asserted` に写す.** 値だけで確かめられるので, 写像の中で確かめ, 新しい記録と同じ水準にした.
- **新しい operation の記録で, 一覧の主張をすべて `asserted` にする.** 上に書いたとおり, producer が答えていない主張が入る.
- **古い値でも `evidence_terms` がなければ断る.** 記録は成功しており, 書いた時点の検査は級数が解でないことを確かめた. 証拠が値に残っていないだけなので, 水準を下げて読む.

## 確認

- 同じ因数分解の古い記録と新しい記録で, 写像と再検証が, agent の記録と同じ水準を出す (`old_and_new_records_of_a_factorization_get_the_same_levels`).
- GKZ の package の試験の値で, 写像が agent と同じ主張と水準を出す. certificate と `evidence_terms` を除いた値では, それらが支える主張だけが `asserted` になり, 写像と再検証が一致する. agent は証拠のない値を断る (`old_and_new_records_of_a_gkz_result_get_the_same_levels`, `an_old_value_lowers_the_claims_of_its_missing_fields_and_is_not_refused`).
- 1 byte 変えた本体は `hash_mismatch` になり, 何も検査しない (`a_body_with_one_byte_changed_is_a_hash_error`).
- 本体のない blob は `data_loss` になる. scheduler と agent を同じ process で動かし, 本体を消して CLI を呼ぶ試験もある (`a_blob_without_a_body_is_data_loss`, `a_result_in_blobs_is_read_and_a_lost_body_is_data_loss`).
- certificate を省いた値は `cannot_recheck` になり, 値を出さない (`a_value_whose_certificate_was_left_out_cannot_be_rechecked`).
- 記録は変わらない. 検査の前後で, scheduler の記録が同じである (`the_record_is_not_changed`, `a_factorization_is_checked_again_to_its_recorded_levels`).
