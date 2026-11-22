# ADR 0030: topology の package と `simplicial.homology`

- 状態: 採用 (crate, module, 型, feature, 型の名前 `simplicial_complex` と `simplicial_homology`, 値の key, 主張 ID, 上限の定数の名前は提案)
- 日付: 2026-11-23
- 関連: [ロードマップ K1, §6.1](../roadmap.md), Issue #103, [ADR 0018](0018-vocabulary-of-result-guarantees.md), [ADR 0019](0019-rust-checks-certificates-and-bounded-recomputation.md), [ADR 0028](0028-gkz-package-and-its-checks.md), [主張の一覧](../claims.md), [topology の package](../packages/topology.md)

## 背景

- ロードマップ K1 は, 単体的複体のホモロジーを ZZ 係数で求める operation `simplicial.homology` を, GKZ に続く 2 つ目の package として足すと定めた. 必要な CAS の計算は, A2 で入れた Smith 標準形 (`matrix.smith_normal_form`) だけである.
- 境界行列 ∂_k の Smith 標準形とその certificate (単模な P_k, Q_k) があれば, ∂_k ∂_{k+1} = 0 のもとで H_k の階数と torsion が決まる. 主張の多くを `checked` にできる.
- 境界行列は facet から組み立てる. k 次元の facet 1 つから 2^{k+1} - 1 個の面が出るので, 仕事量は入力の大きさの多項式で抑えられない. ロードマップ §1.2 の表は, これを「データの構成」とし, 面の数に上限を置くとした.

## 決定

**package の形.** ADR 0028 の形に従う.

| 部分 | 置き場所 |
|---|---|
| crate | `crates/algebramesh-topology`. 複体の検査と面の列挙 (`SimplicialComplex`), 値の組立て (`value`), 検査器 (`verify`). IO は持たない |
| 引数の型と記述子 | `crates/algebramesh-ops/src/topology.rs` (`SimplicialHomologyArguments`, `SimplicialComplexData`) と `SimplicialHomology`. 結果は記述子では JSON とし, 型付きの値は crate に置く |
| backend の glue | `crates/algebramesh-backend-macaulay2/src/packages/topology.rs`. feature `pkg-topology` (backend では既定で外れている) |
| agent | feature `pkg-topology` (既定で入る). backend の `pkg-topology` を入れ, `algebramesh_agent::registry()` が `algebramesh_topology::verify::register` を呼ぶ |
| driver | `drivers/macaulay2/topology.m2`. `algebramesh.m2` が, 隣にあれば読み込む |
| Python | `algebramesh.topology.homology` |
| 回帰試験 | `tests/regression/cases/topology_*.json` (種類 `topology`) |
| 文書 | [`docs/packages/topology.md`](../packages/topology.md) と, この ADR |

- server, core, rpc, proto は変えていない.
- 記述子を `algebramesh-ops` に置くのは ADR 0028 と同じ理由による. scheduler が投入を受けるときに記述子を引くからである. 記述子を登録制にするかは, 2 つ目の package の時点で決めるとしていたが, 今回も決めない. 記述子の追加は 1 行で済み, server は変わらないからである.

**入力.**
- 引数は `complex` (型 `simplicial_complex`. `{"type": "simplicial_complex", "facets": [[0, 1, 2], ...]}`), `domain` (型 `coefficient_domain`), `certificate` (型 `certificate_request`. `include` か `none`. 既定は `include`) である.
- 頂点は 0 以上の 32 bit の整数である. facet の中の頂点の順序は問わない.
- 次の入力は断る. 直して受け取ることはしない.
  - facet がない. 空の facet がある. facet の中に同じ頂点が 2 度ある.
  - facet が他の facet に含まれる (同じ facet が 2 度ある場合を含む). 引数の名前のとおり, facet だけを与える.
  - `domain` が ZZ でない. GF(p) は A5 の後に扱う.
- 上限は次の 2 つである. 超える入力は, backend が CAS を起動する前に `INVALID_ARGUMENTS` で断り, 検査器も断る.
  - `MAX_FACES` = 512. 全ての次元の面の数の和である. facet の数もこれで抑える.
  - `MAX_FACE_WORK` = 16384. 面の列挙で調べる部分集合の数, すなわち facet F についての 2^|F| - 1 の和である. 列挙の前に数える. 頂点が 15 個以上の facet は, それだけでこれを超える.
- 上限の理由. 検査器の費用は, 密な行列の積 P_k ∂_k Q_k と P_k P_k', Q_k Q_k' が大部分を占め, f_k ≤ 512 なので, 1 つの積は成分の積 512^3 回 (約 1.3 × 10^8) 以内である. 積は checkpoint で止まり, ジョブの期限に従う (ADR 0016). certificate は f_{k-1}^2 + f_k^2 個の成分の 2 倍で, 多くの場合は結果の上限 (8 MiB) に入る. 入らないときは, 既存の規則で検査器が certificate を省き, `homology.snf_equivalent` を `asserted` にする.

**面の順序と境界行列.**
- k 次元の面は, 頂点を小さい順に並べた k + 1 個の列とし, 次元ごとに辞書式順序で並べる.
- ∂_k (1 ≤ k ≤ d) は f_{k-1} 行 f_k 列である. 面 [v_0, ..., v_k] の列は, v_i を除いた面の行に (-1)^i を持ち, 他の成分は 0 である.
- ∂_0 は送らない. d = 0 の複体では, 行列のない要求を送る.

**1 つのジョブで行列ごとに SNF を呼ぶ.**
- glue は ∂_1, ..., ∂_d を 1 つの要求 `{"operation": "simplicial.homology", "matrices": [...], "certificate": true}` にして driver に送る. `topology.m2` は, 行列ごとに `matrix.smith_normal_form` と同じ手続き (`canonicalSNF`, `transformJSON`) を呼び, `{"smith_forms": [...]}` を返す. 各要素は `matrix.smith_normal_form` の値と同じ形である. 新しい CAS の算法は書いていない.
- 1 つのジョブにするのは, ホモロジーを 1 つの記録と 1 つの `guarantee` にまとめるためである.

**値.** `type` は `simplicial_homology` である.

| key | 内容 |
|---|---|
| `face_counts` | f_0, ..., f_d |
| `smith_diagonals` | ∂_1, ..., ∂_d の Smith 標準形の対角成分. それぞれ min(f_{k-1}, f_k) 個 |
| `homology` | H_0, ..., H_d. それぞれ `{"rank": b_k, "torsion": ["t", ...]}` |
| `euler_characteristic` | Σ_k (-1)^k f_k |
| `certificate` | `{"boundaries": [{"left": T, "right": T}, ...]}`. T は `{"transform", "inverse"}`. job が求めないときはない |

- ホモロジーは reduced でない. H_0 の階数は連結成分の数である.
- certificate を値の一番上の `certificate` に置くのは, 結果の上限を超えたときに検査器が certificate を省く既存の仕組み (`Registry`) をそのまま使うためである.

**主張.** 詳細は [主張の一覧](../claims.md) の `simplicial.homology` の節にある.

| 主張 | 方法 | 水準 |
|---|---|---|
| `homology.boundary`: `face_counts` は複体の面の数で, ∂_k ∂_{k+1} = 0 | `recomputation` | `checked` |
| `homology.snf_form`: 各対角成分が 0 以上で, 順に割り切る | `self_certifying` | `checked` |
| `homology.snf_equivalent`: diag(d^(k)) = P_k ∂_k Q_k, P_k と Q_k は単模 | `certificate` | `checked`. certificate がなければ `asserted` |
| `homology.groups`: H_k は対角成分から決まる群である | `theorem` (前提は上の 3 つ) | `checked` |
| `homology.euler_characteristic`: Σ (-1)^k f_k に等しく, Σ (-1)^k b_k にも等しい | `recomputation`. 階数の交代和との比較は `consistency` | `checked` |

- `homology.boundary` の ∂_k は, 検査器が spec から組み立て直したものである. CAS の出力ではないので, 面の列挙と組立てのコードは TCB に入る. これはロードマップ §1.2 の表の「データの構成」に当たる. ADR 0019 の (b) と同じく, 上限をこの ADR に数で書き, 実装で強制し, 主張には `recomputation` を記録する.
- `homology.groups` の定理は, Munkres, Elements of Algebraic Topology, §11 による (定理の番号は要確認). 契約の確認の後は, certificate のある結果の要約が `verified` になる.
- Euler 標数の整合 (ロードマップの `consistency`) は, 値の `homology` の階数の交代和と, 検査器が数え直した Σ (-1)^k f_k を比べ, 違えば断るものとした. 水準は与えない. b_k = f_k - r_k - r_{k+1} から, `homology.groups` が成り立てば両者は恒等的に等しいので, この比較は検査器自身の階数の計算を見張るものである. 主張 `homology.euler_characteristic` の水準は, 値の `euler_characteristic` を f_k から数え直すこと (`recomputation`) による.
- 検査器は `matrix.smith_normal_form` と同じ検査を使う. そのため `algebramesh-verify` の `matrix::check_smith_equivalence` と `missing` を公開した. `SmithVerifier` は前者を使うように書き換えたが, 検査の内容は変わらない.

**backend.** glue は Macaulay2 の backend にだけ置いた. OSCAR の backend にも SNF の手続きはあるが, 主張は certificate で `checked` になるので, 2 つ目の backend は照合 (`cross_checked`) を足すときに要る. それは V6 で行う.

## 結果

- `simplicial.homology` の結果は, certificate があれば全ての主張が実効水準で `checked` である. 契約の確認の後は要約が `verified` になる.
- 面の数の上限により, 扱える複体は小さい (面が 512 個まで). 疎な行列の積と疎な certificate を使えば上限を上げられる. 必要になった時点で, この ADR を改訂して上げる.
- package を 1 つ足すのに, server, core, rpc, proto を変えずに済んだ. 記述子だけは `algebramesh-ops` に足した.

## 採らなかった案

- **driver で境界行列を組み立てる.** 組立てが CAS の側に入り, 検査器はいずれにせよ spec から組み立て直す. Macaulay2 のコードも増える.
- **行列ごとに `matrix.smith_normal_form` のジョブを投げる.** 記録が行列の数だけに分かれ, ホモロジーを 1 つの `guarantee` にできない.
- **facet でない面を含む入力を受け取り, 極大なものに直す.** 入力は直さずに断る方針である (GKZ の package と同じ).
- **主張を k ごとの家族 (`homology.snf_form[k]` など) にする.** `homology.groups` の depends_on は, 主張の一覧の前提と ID で突き合わせられる. 家族の各要素を前提として書けないので, 全ての k を 1 つの主張にまとめた.
