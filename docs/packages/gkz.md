# GKZ の package

A-超幾何系 (GKZ 系) H_A(β) を扱う package である ([ADR 0028](../adr/0028-gkz-package-and-its-checks.md)). Macaulay2 で計算する.

## operation

| operation | 内容 | 値の型 | ADR |
|---|---|---|---|
| `gkz.generate` | H_A(β) の生成元 (Euler 作用素と toric の生成元) と, 与えた作用素の所属 | `gkz_system` | [0005](../adr/0005-cas-drivers.md) |
| `gkz.indicial` | 重み w についての indicial ideal, その根と重複度, holonomic rank, cone(A) の facet と共鳴 | `gkz_indicial` | [0007](../adr/0007-gkz-indicial-and-series.md), [0015](../adr/0015-gkz-local-data-guarantees.md) |
| `gkz.series` | 各根の Γ 級数 (重複度 2 以上なら対数解) の, 重み N までの打切り. 標準形 | `gkz_series` | [0007](../adr/0007-gkz-indicial-and-series.md), [0011](../adr/0011-gkz-logarithmic-and-canonical-series.md) |

引数は `A` (整数の行列), `beta` (有理数の列), `gkz.indicial` と `gkz.series` では `weight` (整数の列), `gkz.series` では `order` である.

## 保証

- 主張の命題, 前提, 方法, 依存, 水準は [主張の一覧](../claims.md) の `gkz.generate`, `gkz.indicial`, `gkz.series` の節にある. 検査の欄の読み方は [検算の印の意味](../verification.md) にある.
- 主張は, agent が spec と値だけから確かめる (`algebramesh_gkz::verify`). 成り立たない値は `UNSUPPORTED_RESULT` で断る.
- `checked` になるもの: Euler 作用素の形 (`gkz.euler`), toric の生成元が I_A に属すること (`gkz.toric_in_IA`), 指数の方程式と根, 級数の形と Euler 作用素での厳密な消滅, 解でないことの証拠 (`series.not_a_solution`), 標準形, facet と共鳴 (`local.parameters`).
- `asserted` のもの: toric の生成元が I_A を生成すること (`gkz.toric_generates_IA`), CAS の左イデアルが H_A(β) であること, 所属の答え (`gkz.membership[j]`), indicial ideal, 根が尽きていること, holonomic rank, 対数解の独立性, 計算しなかった解の数.
  - `series.toric_to_order` は自身の水準は `checked` だが, `gkz.toric_generates_IA` に依るので実効水準は `asserted` である.

## 主張しないこと (`not_claimed`)

- 級数が収束すること, 重み N を超えて解に延びること.
- 返した解が解空間の基底であること.
- 重みが generic であること.
- `not_a_solution` の理由 (Γ 級数そのものが解でないのか, CAS の係数が誤っているのか).

各 operation の結果の `guarantee.not_claimed` に, 英語の一覧がある.

## 使い方

```python
from algebramesh import Client, gkz

client = Client("https://scheduler.internal:50051", ca="ca.pem", token_file="client.token")
system = gkz.generate(client, A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4])
indicial = gkz.indicial(client, A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"], weight=[1, 0, 1])
series = gkz.series(client, A=[[1, 1, 1], [0, 1, 2]], beta=["1/2", "1/3"],
                    weight=[1, 0, 1], order=6)
```

`client.gkz`, `client.gkz_indicial`, `client.gkz_series` も同じものを送る.

## build

| 部分 | feature | 既定 |
|---|---|---|
| `algebramesh-backend-macaulay2` | `pkg-gkz` | 外れている |
| `algebramesh-agent` (と, それを使う CLI) | `pkg-gkz` | 入っている |

- feature を外した agent は GKZ の operation を申告しない. scheduler は, それを持つ worker が online にいなければ投入を断る.
- Macaulay2 の image には, `drivers/macaulay2/gkz.m2` を `algebramesh.m2` の隣に置く. `gkz.generate` は 4ti2 を使う.

## 場所

| 部分 | 場所 |
|---|---|
| 数学, 値, 検査器 | `crates/algebramesh-gkz` (`cone`, `operator`, `series`, `value`, `verify`) |
| 記録した値と検査器の試験 | `crates/algebramesh-gkz/testdata/values.json`, `crates/algebramesh-gkz/tests/verify.rs` |
| backend の glue | `crates/algebramesh-backend-macaulay2/src/packages/gkz.rs` |
| driver | `drivers/macaulay2/gkz.m2` |
| Python | `bindings/python/python/algebramesh/gkz.py` |
| 回帰試験 | `tests/regression/cases/` の `gkz_*` |
