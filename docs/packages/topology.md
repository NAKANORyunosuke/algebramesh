# topology の package

単体的複体のホモロジーを扱う package である ([ADR 0030](../adr/0030-topology-package-and-simplicial-homology.md)). Macaulay2 で境界行列の Smith 標準形を計算する.

## operation

| operation | 内容 | 値の型 | ADR |
|---|---|---|---|
| `simplicial.homology` | facet で与えた単体的複体 X の ZZ 係数のホモロジー H_0, ..., H_d. 面の数, 境界行列の Smith 標準形, Euler 標数も返す | `simplicial_homology` | [0030](../adr/0030-topology-package-and-simplicial-homology.md) |

引数は `complex` (`{"type": "simplicial_complex", "facets": [[0, 1, 2], ...]}`), `domain` (今は `ZZ` だけ), `certificate` (`include` か `none`. 既定は `include`) である.

- 頂点は 0 以上の整数で, facet は頂点の空でない集合である. facet が他の facet に含まれる入力と, 頂点が重なる facet は断る.
- 面の数が 512 を超える複体と, facet の部分集合の数 (facet F についての 2^|F| - 1 の和) が 16384 を超える複体は, CAS を起動する前に `INVALID_ARGUMENTS` で断る.
- 値の `homology[k]` は `{"rank": b_k, "torsion": ["t", ...]}` で, H_k は ZZ^{b_k} と各 t についての ZZ/t の直和に同型である. ホモロジーは reduced でない.
- 面の順序 (頂点を小さい順に並べた列の辞書式順序) と境界行列の符号は [主張の一覧](../claims.md) の `simplicial.homology` の節にある. certificate の P_k, Q_k はこの順序で読む.

## 保証

- 主張の命題, 前提, 方法, 依存, 水準は [主張の一覧](../claims.md) の `simplicial.homology` の節にある.
- 主張は, agent が spec と値だけから確かめる (`algebramesh_topology::verify`). 検査器は境界行列を spec から組み立て直す. 成り立たない値は `UNSUPPORTED_RESULT` で断る.
- `checked` になるもの: 面の数と ∂_k ∂_{k+1} = 0 (`homology.boundary`), Smith 標準形の形 (`homology.snf_form`), certificate による Smith 標準形の検査 (`homology.snf_equivalent`), それらからの H_k (`homology.groups`. 定理), Euler 標数 (`homology.euler_characteristic`).
- job が certificate を求めないときは, `homology.snf_equivalent` が `asserted` になり, `homology.groups` の実効水準も `asserted` になる.
- 契約は未確認なので, 要約は `partially_verified` である. 契約の確認の後は, certificate のある結果の要約が `verified` になる.

## 主張しないこと (`not_claimed`)

- 各ホモロジー群の生成元を表す輪体.
- P_k と Q_k の一意性.
- 複体が三角形分割として表すことを意図した空間についての性質.

結果の `guarantee.not_claimed` に, 英語の一覧がある.

## 使い方

```python
from algebramesh import Client, topology

client = Client("https://scheduler.internal:50051", ca="ca.pem", token_file="client.token")
# 四面体の境界: H_0 = ZZ, H_1 = 0, H_2 = ZZ.
value = topology.homology(client, [[0, 1, 2], [0, 1, 3], [0, 2, 3], [1, 2, 3]])
print([(h["rank"], h["torsion"]) for h in value["homology"]])
```

`certificate=False` を渡すと, certificate なしで送る. CLI では, 6 頂点の実射影平面の例を次のように送れる.

```bash
algebramesh submit examples/simplicial_homology_projective_plane.json --wait
```

## build

| 部分 | feature | 既定 |
|---|---|---|
| `algebramesh-backend-macaulay2` | `pkg-topology` | 外れている |
| `algebramesh-agent` (と, それを使う CLI) | `pkg-topology` | 入っている |

- feature を外した agent は `simplicial.homology` を申告せず, その検査器も登録しない. scheduler は, それを持つ worker が online にいなければ投入を断る.
- Macaulay2 の image には, `drivers/macaulay2/topology.m2` を `algebramesh.m2` の隣に置く. driver の digest にはこの file も入る.

## 場所

| 部分 | 場所 |
|---|---|
| 複体, 境界行列, 値, 検査器 | `crates/algebramesh-topology` (`SimplicialComplex`, `value`, `verify`) |
| 引数の型と記述子 | `crates/algebramesh-ops/src/topology.rs` |
| 記録した値と検査器の試験 | `crates/algebramesh-topology/testdata/values.json`, `crates/algebramesh-topology/tests/verify.rs` |
| backend の glue と記録した返答 | `crates/algebramesh-backend-macaulay2/src/packages/topology.rs`, `crates/algebramesh-backend-macaulay2/testdata/topology.json` |
| driver | `drivers/macaulay2/topology.m2` |
| Python | `bindings/python/python/algebramesh/topology.py` |
| 回帰試験 | `tests/regression/cases/` の `topology_*` |
