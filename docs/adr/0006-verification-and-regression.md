# ADR 0006: 回帰試験と障害試験の方針

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §3.2 reliability, §13.1, §13.2 v1.0

## 背景

v1.0 の完了条件は, 回帰試験, 障害対策, 再現性記録, 文書とインストール手順がそろうことである.
CAS の版を更新したときに, 数学的な結果が変わっていないことを機械的に確かめる手段が要る.

## 決定

### 数学的回帰試験

- `tests/regression/cases/*.json` に, 入力・対象 backend・期待値を固定する. 期待値は手で検証できる例に限る.
  - `x^4-y^4`, 定数倍を含む例 (QQ と ZZ), 既約な例, 重複度のある例, 円分因子に分かれる `x^40-y^40`
  - 設計書 §9.2 の GKZ 系, 3次の rational normal curve (`A = [[1,1,1,1],[0,1,2,3]]`, `beta = [-1/2, 0]`) の GKZ 系
- 比較は型付きの結果で行う. 因数分解では定数・因子・重複度を, GKZ 系では Euler 作用素・toric 生成元の数・CAS による所属判定を比べる.
- `display` は Rust 側で正準形から作る表示なので, CAS の表示形式に依存しない.
- 期待値とは別に, 所属しないはずの作用素 (`d1` 単独) も入れ, 判定が常に「属する」と答える誤りを検出する.
- `gkz.indicial` と `gkz.series` の例 (Issue #39) は種類 (`category`) で分ける. generic (斉次で非共鳴), resonant (斉次で共鳴. toric 環が Cohen-Macaulay なので rank は変わらない), rank_jump (`A = [[1,1,1,1],[0,1,3,4]]`, `beta = (1,2)`. rank が 5 になる), inhomogeneous (`A = [[1,2]]`) の 4 種である.
  - 各例の `reference` に, backend と独立に求めた holonomic rank を記録する. 値は Singular の `holonomicRank` で計算した (`tests/regression/references/holonomic_rank.sing`). rank jump の例は文献の値 (Sturmfels と Takayama) とも一致する.
  - `run.py` は, backend が返す rank を参照と比べる. また, A から vol(A) (`conv(0, a_1, ..., a_n)` の正規化体積. d = 1, 2) を計算し, rank_jump の例では rank が vol(A) を超えること, 他の例では等しいことを確かめる. 等しいことの根拠は, 非共鳴なら Gelfand, Kapranov, Zelevinsky と Adolphson, 共鳴でも toric 環が Cohen-Macaulay なら Matusevich, Miller, Walther である.
  - `ALGEBRAMESH_BACKENDS=macaulay2` のように, 一部の backend の例だけを流せる.
- 実行結果には CAS の版を表示する. CAS を更新したら `tests/integration/run-regression.sh` を実行する.
- 値の比較 (Issue #42). 回帰試験 (`run.py`) と `bindings/python/tests/test_cluster.py` は, 各例の結果の値 (記録の `result`) を canonical JSON で保存し (`ALGEBRAMESH_VALUES_SAVE`), 前の版で保存した値と比べられる (`ALGEBRAMESH_VALUES_COMPARE`). 違いは JSON pointer で示し, 失敗として数える. job id, 時刻, host, log は記録の側にあり, 値には含まれないので比べない. 部品は `tests/regression/values.py` で, 単体試験は `tests/regression/test_values.py` (CI で流す).
  - 2026-11-06 に, 同じ image で回帰試験を 3 回流して値を保存した. 29 例の値は, list の要素の順序まで含めて 3 回とも一致した. したがって, 順序を問わずに比べる欄 (`values.UNORDERED`) は今は置かない. CAS の版を上げたときに順序が変われば, そこで一覧に足す.
  - 1 か所を変えた値 (因数の重複度) と比べると, その例だけが失敗し, `/factors/0/multiplicity` が示された.

### 障害試験

`tests/integration/run-fault-e2e.sh` で次を確かめる.

1. 実行中のジョブを持つ worker コンテナを kill する. heartbeat の途絶でジョブが `LOST` になり, `retry_policy = if_deterministic` の再試行が別の worker で成功する.
2. 実行中に scheduler を再起動する. そのジョブは `LOST` になり, agent が再登録して, 次のジョブが成功する.

重いジョブは試験のたびに生成する ((x+y+z+1)^34 (x−y+2z+3)^34 の展開, 約5.7万項). 期待値が既知なので, 再試行後の結果も検算で確かめられる.

## 残る課題

- 物理的に異なるホスト間のネットワーク断 (パケット損失, 片方向の遮断) は試していない.
- 性能評価は同一マシン上の 1, 2, 4 worker だけを測った ([benchmarks.md](../benchmarks.md)). 8 worker と別ホストは未測定.
