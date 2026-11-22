# ADR 0015: GKZ の局所データが保証すること

- 状態: 採用
- 日付: 2026-11-05
- 関連: [ADR 0007](0007-gkz-indicial-and-series.md), [ADR 0011](0011-gkz-logarithmic-and-canonical-series.md), Issue #4

## 背景

`gkz.indicial` は, 指数が indicial ideal の根であることと `A v = beta` を Rust で確かめていた.
一方, 指数の重複度の和が holonomic rank と一致しないときも `checks.multiplicities_sum_to_rank = false` を付けて成功として返していた.
この結果を級数の基底の入力に使うと, 解の空間の一部が抜けたまま完全な基底として扱われうる.

## 決定

- 重複度の和が holonomic rank と一致しない結果は成功にしない. `gkz.indicial` と `gkz.series` はどちらも `UNSUPPORTED_RESULT` で失敗し, 和, rank, 指数の数, 重みを示す. 別の重みを選び直すことを勧める.
- 成功した結果の `checks.multiplicities_sum_to_rank` は常に `true` である (互換のために残す).

## 成功した `gkz.indicial` が保証すること

- 各指数は, 与えた重みの indicial ideal (Macaulay2 が計算したもの) の根であり, `A v = beta` を満たす (Rust で確かめる).
- 指数の重複度の和が, Macaulay2 が計算した `H_A(beta)` の holonomic rank に等しい. したがって, この重みについての形式解の空間の次元の勘定が合っている.
- rank は与えた `beta` について計算するので, rank jump する `beta` でもその `beta` の rank と比べる.

## 保証しないこと

- 重みが generic であることを直接は確かめない. A が斉次 (H_A(beta) が regular holonomic) なら, 重複度の和と rank の一致は generic であることの必要条件として確かめるにとどまる. A が斉次でなければ, generic な重みでも和が rank より小さいことがあり (例: A = (1 2), beta = 1/3, w = (1, 3)), そのときも結果を断る (Issue #61 の review で分かった).
- rank と indicial ideal はどちらも同じ Macaulay2 の driver が計算する. ジョブの実行時に独立な計算はしない. 回帰試験の例に限り, rank を独立な参照と照合する (Issue #39, ADR 0006).
- 非斉次な A では, 形式解が収束するとは限らない. `parameters.homogeneous` で区別する.
- `gkz.series` の `verified_to_order` は, 打ち切った次数までの形式的な検算である (ADR 0007). 級数の収束や, 解の空間の基底であることの証明ではない.

## 確認

- 単体試験: 記録した Macaulay2 の応答の rank を書き換え (2 個の単純な指数に対して rank 3 と rank 1), `gkz.indicial` と `gkz.series` がどちらも `UNSUPPORTED_RESULT` で失敗すること. 元の応答 (rank 2) は成功すること.
- 回帰試験 (`tests/regression`): 既存の `gkz.indicial` と `gkz.series` の例は, どれも重複度の和が rank に一致する.
