# AlgebraMesh 基本設計書

- 版: 0.2 (プロジェクト命名・独立リポジトリ構成の改訂)
- 作成日: 2026-11-03
- 位置づけ: 新規リポジトリでの実装開始に向けた基本設計. 確定事項と検証事項を区別する.
- 実装言語: Rust を中核とし, Python / Julia は利用者向けインターフェースおよび計算エンジンとして使う.

## 0. プロジェクト名・リポジトリの独立性

### 0.1 命名（採用案）

- **プロジェクトの表示名:** `AlgebraMesh`
- **新規 GitHub リポジトリ:** `NAKANORyunosuke/algebramesh`
- **ローカルディレクトリ:** `algebramesh/`
- **Rust workspace:** `algebramesh/` （monorepo）
- **CLI 実行名:** `algebramesh` （仮仕様）
- **Python import 名:** `algebramesh` （仮仕様）
- **Rust package prefix:** `algebramesh-*`
- **Docker image namespace:** `ghcr.io/nakanoryunosuke/algebramesh-*` （公開時に確定）
- **短い説明:** `Rust-based distributed computer algebra and research computing platform.`

他の候補は `mathfabric`, `casweave`, `mathcluster` とする. GitHub の公開リポジトリ検索では `algebramesh` という完全一致の既存リポジトリは見つからなかった（2026-11-03 時点）. ただし crates.io / PyPI / 商標の空きは未確定であり, 公開・登録前に再確認する.

### 0.2 リポジトリを分ける理由と規則

| リポジトリ | 役割 | 変更方針 |
|---|---|---|
| [`Risa-Asir-container`](https://github.com/NAKANORyunosuke/Risa-Asir-container) | 既存の Risa/Asir コンテナ構築と例題 | **変更しない** |
| `NAKANORyunosuke/algebramesh` | Rust OpenXM クライアント, 分散クラスタ, CAS 統合, GKZ | **新規作成する. 既存の fork ではない** |

- Git history, Issue, CI, release, license, README は新規リポジトリに独立して設置する.
- 既存の Shell script をコピーして保守することを標準構成とせず, 新しい Rust agent と `deploy/` の設定で環境を再現する.
- 既存の GHCR イメージ利用は開発時の *任意の外部依存* とする. セキュリティと再現性のため digest pin を使用する.
- 新規コードは既存プロジェクトのビルド/起動スクリプトの仕様に固定的に依存しない. OpenXM の標準プロトコルに対する互換性を優先する.
- 最初は **1つの Rust monorepo** に全 crate を置き, バージョンと CI を一元管理する. `algebramesh-openxm-protocol` と `algebramesh-openxm-client` は platform に依存しないライブラリとして開発し, 必要が出た場合のみ別リポジトリへ分離する. 最初から4つ以上のリポジトリへ分割しない.
- ローカル・CI・コンテナのすべてで新リポジトリのコードと既存の Risa/Asir 実行環境の境界を明示する.

### 0.3 コンポーネントの公開名

| 層 | Rust crate / パッケージ名 | 独立性 |
|---|---|---|
| OpenXM protocol | `algebramesh-openxm-protocol` | スケジューラに依存しない |
| OpenXM client | `algebramesh-openxm-client` | プロトコル crate に依存 |
| 数学データ | `algebramesh-math-types` | CAS 実装に依存しない |
| Core / Scheduler | `algebramesh-core`, `algebramesh-server` | platform 共通 |
| Agent | `algebramesh-agent` | 各ホストに配置 |
| CAS adapters | `algebramesh-backend-asir`, `algebramesh-backend-macaulay2`, `algebramesh-backend-oscar` | 各 CAS の責務を限定 |
| GKZ | `algebramesh-gkz` | 数学的検証と orchestrate |
| CLI | `algebramesh-cli` | バイナリ名 `algebramesh` |
| Python binding | `algebramesh-python` | Python package 名 `algebramesh` |

crate 名と import 名は推奨命名であり, 実際の package registry の空きとは区別する. package publish は v0.1 の受け入れ要件に含めない.

## 1. 目的

AlgebraMesh は, 異なる物理ホストに配置した計算資源と複数の Computer Algebra System (CAS) を統合し, 研究計算を型安全・非同期・再現可能な形で実行する基盤である.

対象となる4機能は次のとおり.

1. Risa/Asir の複数物理ホストへの分散配置.
2. Risa/Asir, Macaulay2, OSCAR/Singular 等の統一的な操作.
3. Rust による OpenXM 通信ライブラリと, その Python API.
4. GKZ 系および D-module の計算パイプライン.

**基本方針:** 数式処理アルゴリズムを Rust で再実装しない. Rust が担当するのは通信, プロセス隔離, スケジューリング, 型付きデータの受け渡し, 記録と検証である. Gröbner basis や D-module の具体的な計算は既存 CAS に委譲する.

## 2. 既存実装と前提条件

既存の [Risa-Asir-container](https://github.com/NAKANORyunosuke/Risa-Asir-container) は, `cluster/run_example.sh` で Compose と Asir スクリプトを生成し, `ox_launch` によって worker が master の control/server ポートへ接続する. master 側の `try_bind_listen` / `try_accept` / `register_server` で接続を登録し, `ox_rpc` でジョブを投入, `ox_select` / `ox_get` で回収する.

**注意:** 現状の `master` コンテナは `sleep infinity` を実行するだけで, 外部クライアントから常時アクセスできる OpenXM サーバではない. Asir スクリプトを起動したときに待受を作る構成である. そのため Rust から直接 `master` に接続する機能は既存実装にはない. Rust 側の listener/launcher または互換ブリッジを新設しなければならない.

既存リポジトリは編集・fork・submodule 化しない. OpenXM 実装と統合基盤のソースは新規リポジトリ `NAKANORyunosuke/algebramesh` に置く. 必要な場合に限り, 既存リポジトリが公開するコンテナイメージを外部依存として開発・互換性テストで参照する. ただし, 依存なしで起動可能な CAS コンテナ構築手順も新リポジトリ側に整備する.

## 3. 機能要件

### 3.1 必須機能

| ID | 要件 | 判定基準 |
|---|---|---|
| F-01 | Rust から OpenXM セッションを確立 | Asir との control/data 接続が正常に確立される |
| F-02 | CMO と Stack Machine コマンドを送受信 | 整数・文字列・リストの往復および関数実行が成功 |
| F-03 | 非同期ジョブを管理 | `submit`, `status`, `result`, `cancel` が利用可能 |
| F-04 | 複数物理ホストに worker を配置 | 異なる2ホストへ独立ジョブを分配できる |
| F-05 | CAS 切替 | Asir と Macaulay2 を同一ジョブ API から呼び出せる |
| F-06 | 型付き数式データ | 係数環, 変数順序, 項順序を保持し, 不正な変換を拒否 |
| F-07 | GKZ 系の生成 | 入力 `(A, beta)` から生成元を構造化して取得 |
| F-08 | 再現性情報の保存 | 入力, バックエンド, バージョン, 結果, 実行ログを追跡できる |
| F-09 | Python API / CLI | 同じジョブ API を Rust CLI と Python から使える |

### 3.2 非機能要件

- correctness: 結果のデータ型と環境情報を保存し, 文字列表示だけで数学的同値性を判定しない.
- portability: Linux を worker の主対象とし, Windows は初期段階では CLI/Python クライアントとして扱う.
- reliability: ジョブの消失や重複実行の可能性を明示し, 再実行は安全な操作に限る.
- observability: ジョブ・セッション・worker ごとに識別子, 状態, 所要時間, エラーを記録する.
- security: OpenXM ポートを公衆インターネットへ公開しない. 任意スクリプト実行は既定で無効にする.

### 3.3 初期段階で実装しないもの

- Gröbner basis や GKZ アルゴリズムの Rust による全面再実装.
- 1個の CAS ジョブ内部を自動的に複数 CPU/ホストへ並列化する機能.
- 任意の CAS オブジェクト間の完全かつ無損失な相互変換.
- 高可用な複数 scheduler, Web UI, マルチテナント課金.
- 全 GKZ パラメータに対する局所解・解析接続・モノドロミーの完全自動計算.

## 4. 全体アーキテクチャ

```text
                   Rust CLI / Python (PyO3) / Jupyter
                                   |
                            gRPC / TLS
                                   |
                      +-----------------------+
                      | algebramesh-server    |
                      | API + Job Scheduler   |
                      | Metadata / Artifacts  |
                      +-----------------------+
                           |               |
                    gRPC / TLS        gRPC / TLS
                           |               |
                   +---------------+ +---------------+
                   | worker agent A| | worker agent B|
                   | host A        | | host B        |
                   +---------------+ +---------------+
                     |           |       |          |
                 OpenXM       Process   Process    Process
                     |           |       |          |
                 Risa/Asir   Macaulay2  OSCAR    Risa/Asir
                                            |
                                          Singular
```

通信経路を2種類に分離する.

- **OpenXM:** Rust worker agent と OpenXM 対応の数式処理サーバ間の通信. CMO と Stack Machine の仕様に従う.
- **gRPC:** scheduler, agent, client 間の独自管理プロトコル. ジョブの投入・状態・キャンセル・結果参照を扱う.

OSCAR および Macaulay2 は OpenXM 対応を前提としない. 各 agent がローカル subprocess/専用 driver を制御し, 機能を共通 API に変換する.

### 4.1 コンポーネントの責務

| コンポーネント | 責務 |
|---|---|
| `algebramesh-openxm-protocol` | バイトオーダー, OX フレーム, CMO codec, タグ定数, 入力検証 |
| `algebramesh-openxm-client` | control/data 接続, Stack Machine 呼出し, capability 問合せ, セッション管理 |
| `algebramesh-math-types` | 整数, 有理数, 多項式, ideal, Weyl algebra, 結果メタデータの型 |
| `algebramesh-core` | Job/Backend/Worker の共通 interface, エラー型, 状態遷移 |
| `algebramesh-server` | 公開 API, スケジューラ, キュー, 永続メタデータ, 実行履歴 |
| `algebramesh-agent` | worker 登録, 実行環境隔離, リソース管理, CAS 接続, health check |
| `algebramesh-backend-asir` | OpenXM を介した Asir の関数呼出しと CMO 変換 |
| `algebramesh-backend-macaulay2` | Macaulay2 driver, D-module/GKZ アダプタ |
| `algebramesh-backend-oscar` | Julia/OSCAR driver, Singular 等の機能公開 |
| `algebramesh-gkz` | GKZ 入力検証, バックエンド操作の組立, 数学的検算 |
| `algebramesh-cli` | Rust 製 CLI |
| `algebramesh-python` | PyO3/maturin による Python binding |

## 5. OpenXM 実装仕様

### 5.1 通信の基本

OpenXM-RFC 100 では, 通常のデータ/Stack Machine コマンド用通信路と, 制御用通信路が分かれる. OX パケットは `message tag`, `serial number`, `body` を持つ. `OX_COMMAND = 513`, `OX_DATA = 514`. 主なコマンドは `SM_popCMO = 262`, `SM_mathcap = 264`, `SM_executeFunction = 269`, `SM_executeStringByLocalParser = 268` である.

接続時の byte-order negotiation は control/data のそれぞれで実施する. Rust クライアントはネットワークバイトオーダーを要求し, まずはこれのみを実装する. サーバ側の希望と異なる場合も規格に従って network byte order となる. OX の serial number はジョブ ID と同義ではないため, scheduler のジョブ照合に流用しない.

### 5.2 接続方式

- `ReverseConnect`: 既存の `ox_launch` と互換. Rust が control/server の listener を確保し, worker が逆接続する. **最優先の接続方式**.
- `SpawnLocal`: 単一ホストで launcher/server を起動し, 接続する開発用方式. 起動時の引数・認証・接続順序は実機試験で固定する.
- 将来: 専用 agent が子プロセスとして `ox_asir` を管理し, TCP ではなくローカルソケット等を使う最適化.

両チャネルの接続元・セッション識別と, 対応付けを検証する. 接続途中で片方が失敗した場合は両方を破棄する. リスナーポートは固定値の埋込みを避け, 動的割当を原則とする.

### 5.3 CMO の段階的対応

- 第1段階: `CMO_NULL`, `CMO_INT32`, `CMO_STRING`, `CMO_LIST`, `CMO_ZZ`, `CMO_QQ`, `CMO_ERROR2` のうち実サーバの Mathcap で確認できた型を優先. さらに `fctr` の構造化結果に必要な `CMO_INDETERMINATE`, `CMO_POLYNOMIAL_IN_ONE_VARIABLE`, `CMO_RECURSIVE_POLYNOMIAL` 等を, 実サーバの出力に合わせて最小実装する.
- 第2段階: より一般の環情報, 分散多項式, その他の数式処理オブジェクトを追加.
- 未対応 CMO は黙って文字列化せず, `UnsupportedCmo` として型タグと状況を返す. 未知の長さのオブジェクトを安全に読み飛ばせると仮定しない.
- 長さ, 深さ, リスト要素数, 多倍長整数のサイズには上限を設ける. 不正なフレームで panic しない.

`SM_mathcap` による実装能力の問合せを優先し, 対応機能を固定的に仮定しない.

### 5.4 関数実行と並列性

`SM_executeFunction` は仕様で定めるスタック順に引数・個数・関数名を push して実行する. 結果は `SM_popCMO` で受信する. スタックの整合性を重視し, **1 OpenXM セッションあたり同時実行1件**とする. 複数ジョブの並列化は複数の独立セッションで行う. 結果の到着順を serial number による RPC 対応付けと誤認しない.

任意の `SM_executeStringByLocalParser` を公開 API にそのまま通さない. 初期の動作確認では固定テスト式を利用し, 通常 API では operation 名と型付き引数を検証して実行する. 生の CAS コード実行は将来の信頼済みローカル利用に限定する.

タイムアウト後のセッション再利用は原則禁止し, 安全な reset/同期回復を確認できた場合のみ例外とする.

## 6. 数学データモデル

数式の正確な意味を保持できる範囲を小さく定め, 徐々に拡張する.

```text
Scalar = Integer(BigInt) | Rational(num, den)
RingSpec = {coefficient_domain, variables, monomial_order}
Polynomial = {ring: RingSpec, terms: [(exponent_vector, coefficient)]}
Ideal = {ring: RingSpec, generators: [Polynomial]}
PolynomialMatrix = {ring: RingSpec, rows, columns: [{row: Polynomial}]}
WeylAlgebraSpec = {coefficient_domain, x_variables, d_variables, ordering}
DifferentialOperator = {algebra: WeylAlgebraSpec, normal_ordered_terms}
DModuleIdeal = {algebra: WeylAlgebraSpec, left_generators}
```

ルール:

1. `Rational` は分母正, 既約化して保存する. 自動的に浮動小数点へ変換しない.
2. 係数環・変数の個数・変数順序・項順序を明示する. CAS の既定値に依存しない. 項順序は `lex`, `grlex`, `grevlex`, 重みと同点の決め方の組, block 順序で, 大域的な順序に限る ([ADR 0023](adr/0023-monomial-orders.md)).
3. Weyl algebra は `d_i x_j - x_j d_i = delta_ij` に従う非可換環として扱う. 一般の可換多項式との暗黙変換を禁止する.
4. 数式データの同値性を表示文字列だけで判定しない. 検算は指定した代数系の中で実施する.
5. 多項式の先頭項は, 環の項順序で決める. 別の順序で見るときは, 環の順序を替える. 多項式の行列は列を生成元とし (変換行列 C は G = F C を満たす), 0 でない成分だけを持つ. 行列の JSON は, 多項式の JSON と同じく環を持たず, 文脈の環で読む.
6. 共通型へ無損失変換できない結果は backend 固有 artifact として扱い, 対応している形式とメタデータを明示する.

最初に共通化するのは `ZZ`, `QQ` 上の多項式, 整数行列, 生成元の有限リストであり, 一般代数体や数値モノドロミーは後段に置く.

## 7. 分散ジョブ管理

### 7.1 Job specification

```json
{
  "schema_version": 1,
  "backend": "asir",
  "operation": "polynomial.factor",
  "arguments": {
    "ring": {"domain": "QQ", "variables": ["x", "y"], "order": "grevlex"},
    "expression": {"type": "polynomial", "terms": [
      {"coefficient": "1", "exponents": [4, 0]},
      {"coefficient": "-1", "exponents": [0, 4]}
    ]}
  },
  "limits": {"wall_seconds": 120, "memory_mb": 1024},
  "retry_policy": "never"
}
```

`limits.memory_mb` は, それを強制する backend だけが受け付ける ([ADR 0014](adr/0014-job-memory-limit.md)). 強制しない backend へのジョブは断る. この例の Asir backend は強制しないので, 実際に投入するときは `memory_mb` を外す.

API で扱う主な要素は `JobId`, `backend`, `operation`, `typed arguments`, `resource limits`, `environment constraints`, `retry policy`, `result artifacts` とする. gRPC の protobuf schema はこの論理モデルをもとに定義し, wire compatibility を維持する.

### 7.2 状態遷移

```text
QUEUED -> ASSIGNED -> RUNNING -> SUCCEEDED
   |          |          |      -> FAILED
   |          |          |      -> TIMED_OUT
   |          |          +------> CANCELLING -> CANCELLED
   |          +-----------------> LOST
   +---------------------------> CANCELLED
```

- `LOST` は通信断等により実行結果を確定できない状態. 自動的に `FAILED` と同一視しない.
- scheduler は `backend` の対応状況, 空き実行枠, CPU/RAM, 版数, 優先度を考慮して配置する.
- 1 OpenXM セッションを1 worker slot と数える. ホスト1台に複数 slot を持てる.
- worker は heartbeat と実行状況を報告する. 切断時は timeout を経て `LOST` とする.
- 自動 retry は副作用のない決定的なジョブに限定する. 切断直後に実行中ジョブが本当に停止したとは仮定しない. Exactly-once execution は保証しない.

### 7.3 結果と再現性

結果ごとに入力の正規化表現, SHA-256, 実行した CAS の名前・版, Docker image digest / ソース commit, 操作名, 実行ホスト, 開始終了時刻, stdout/stderr, エラー, 主要リソース使用量を保存する.

cluster が機能 `backend_provenance` を有効にしていれば ([ADR 0020](adr/0020-cluster-feature-agreement.md)), 記録の `backend` に次の 2 つも保存する.

- `driver_sha256`: backend が読み込む driver の file の SHA-256. driver の file を持たない backend (Asir) では空である.
- `agent_build`: agent を build した commit. 作業ツリーに変更があったか, 分からないときは `-modified` を付ける.

結果ごとに保証 (`guarantee`) を記録する ([ADR 0018](adr/0018-vocabulary-of-result-guarantees.md)). backend が答えた後, 検査器 (`algebramesh-verify`) がジョブの spec の入力と結果を照らし, 主張ごとの水準と要約を書く. 検査はジョブの締切の内側で走る (ADR 0016). agent の処理の順は, backend の実行, 検査と保証の記録, 報告である (#36 の upload は検査と報告の間に入る). backend が自分で書いた保証は捨て, `asserted` より強い水準を名乗る結果は `UNSUPPORTED_RESULT` にする. 記録の欄 `guarantee` は, cluster が機能 `result_guarantee` を有効にしてから書く (ADR 0020).

`build` は CAS の導入の識別子で, image の digest (`sha256:<64 桁の 16 進>` か `<名前>@sha256:<64 桁の 16 進>`) か, 40 桁の git commit とする. それ以外の値は来歴として扱わない.

初期構成は scheduler 1台 + ローカル SQLite (WAL) + ファイル artifact store でよい. SQLite ファイルをネットワークファイルシステムで複数ノードが同時更新する運用はしない. 高可用化は scheduler の Raft で行う (ADR 0013). 8 MiB を超える値の本体は Raft のログの外に置き, scheduler が自前で投票者の過半数に複製する. 記録には内容の SHA-256 で参照する marker だけが入る ([ADR 0021](adr/0021-content-addressed-blob-store.md)). 外部のオブジェクトストレージへは, 境界 `BlobBackend` で置き換えられるようにしておく.

## 8. CAS backend 設計

各バックエンドは次の概念的な interface を満たす.

```rust
#[async_trait::async_trait]
pub trait CasBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    async fn execute(&self, job: CasRequest, ctx: ExecutionContext)
        -> Result<CasResult, CasError>;
}
```

- `algebramesh-backend-asir`: OpenXM の一部を実装した Rust client を使用. `fctr` 等の検証済み操作を登録する.
- `algebramesh-backend-macaulay2`: 専用 driver を subprocess として起動し, 入力を検証済み template から組み立てる. 返却値は構造化 artifact を優先し, ログと数学データを分離する.
- `algebramesh-backend-oscar`: Julia worker process 内で OSCAR をロードする. 最初は Julia driver と agent 間を行単位 JSON 等でやり取りし, 大規模データは別 artifact にする. Singular の重複した直接統合は初期段階では作らない.
- `HyperPrecision.jl` は将来の解析接続・モノドロミー数値計算用 backend として別枠にできるが, GKZ の代数的生成との対応・入力変換・精度証明を追加で検証する.

バックエンドごとに `backend_version`, `driver_version`, `capabilities`, `supported_types`, `max_precision` を明示し, 未対応操作は推測せず `UNSUPPORTED_OPERATION` を返す.

operation の名前, 引数と結果の型, 記述子は crate `algebramesh-ops` に置く ([ADR 0022](adr/0022-operation-types-and-signatures.md)). バックエンドは operation ごとに, 引数と結果の型の名前 (signature) を `capabilities` の `signatures` で申告する.

## 9. GKZ / D-module パイプライン

### 9.1 数学的入力

整数行列 `A in ZZ^(d x n)` とパラメータ `beta in QQ^d` を初期対象とする. 入力検証では, 行列が空でないこと, 列数, `len(beta)=d`, `rank(A)=d` 等を確認する. 階数不足などを機械的に正規化せず, 仕様外として診断を返す.

`D_n = QQ<x_1,...,x_n, d_1,...,d_n>` を Weyl algebra とし, `d_i x_j - x_j d_i = delta_ij` を満たす. `I_A = <d^u-d^v : Au=Av>` とすると,

`H_A(beta) = D_n I_A + sum_{i=1}^d D_n (sum_{j=1}^n a_ij x_j d_j - beta_i)`

の生成元を得ることが第1段階の目標である.

### 9.2 最小検証例

```text
A = [[1,1,1],
     [0,1,2]]
beta = [3,4]
```

期待される生成元の一組は,

```text
d_1*d_3 - d_2^2
x_1*d_1 + x_2*d_2 + x_3*d_3 - 3
x_2*d_2 + 2*x_3*d_3 - 4
```

である. 生成元の符号や順序は CAS により異なるので, **文字列完全一致ではなく生成する左イデアルと数学的関係を検証**する. まずは `HolonomicSystems` の `gkz(A, beta)` と `toricIdealPartials` / `eulerOperators` を使い, 入力と対応する生成元を確認する.

### 9.3 拡張段階

1. `gkz.generate`: GKZ ideal, toric ideal, Euler operators を取得する.
2. `gkz.indicial`: 指定した weight に対し, indicial ideal と指数候補を計算する. 利用可能な関数と適用条件を検証する.
3. `gkz.series`: 局所級数計算を追加する. 収束・共鳴・対数項を区別する.
4. `gkz.numeric`: Pfaffian system / 解析接続 / monodromy の数値計算と接続する. 入力・基底・分岐経路・誤差モデルの整合性を別途保証する.

一般の共鳴パラメータに対する完全性, rank jump, period subspace の決定はソフトウェアが自動的に達成する性質ではなく, 別途数学的な正当化を要する.

## 10. CLI と Python API

### 10.1 CLI (目標形)

```bash
algebramesh server start --config config/server.toml
algebramesh worker start --server https://scheduler.internal:50051
algebramesh submit examples/factor.json
algebramesh status <job-id>
algebramesh result <job-id> --format json
algebramesh cancel <job-id>
algebramesh backends
```

### 10.2 Python (目標形, 未実装)

```python
from algebramesh import Client

client = Client("https://scheduler.internal:50051")
job = client.submit(
    backend="asir",
    operation="polynomial.factor",
    arguments={"polynomial": "x^4-y^4", "variables": ["x", "y"]},
)
result = job.result(timeout=120)

# 後段の実装目標
system = client.gkz(A=[[1, 1, 1], [0, 1, 2]], beta=[3, 4])
```

この例は使い勝手を示す草案. 実 API では文字列の多項式入力を必須とせず, 型付き入力を基本とする. Python との接続は PyO3 / maturin で CLI と同じ Rust client library を公開する. Julia には初期段階では CLI/gRPC を提供し, 専用バインディングは必須としない.

## 11. 配置・セキュリティ

- scheduler と agent の接続には TLS と認証を必須とする. 内部環境では Tailscale 等を使用できるが, それだけでアプリケーションの権限管理を済ませたことにはしない.
- OpenXM の生 TCP ポートは各 worker ホスト/コンテナの閉域ネットワーク内に限定する. 公開ポートへの直接転送を禁止する.
- 処理対象はユーザー指定の任意コードではなく, allowlist に登録した operation と型付き引数を原則とする.
- CAS subprocess は低権限ユーザーで起動し, CPU, RAM, 実行時間, 同時プロセス数, ネットワークへのアクセスを制限する. `cancel` や timeout 後は子プロセス群ごと終了させる.
- worker が接続できる実行ホストとバックエンドを明示的に登録し, 版数・digest をピン留めする. `latest` タグだけで再現性を主張しない.
- OpenXM, Asir, Macaulay2, OSCAR, Singular のライセンスと再配布条件は**個別に**確認し, CI とコンテナ配布に反映する. OpenXM リポジトリのライセンス表示を全依存物へ一般化しない.

## 12. ディレクトリ案

```text
algebramesh/
├── Cargo.toml
├── crates/
│   ├── algebramesh-openxm-protocol/
│   ├── algebramesh-openxm-client/
│   ├── algebramesh-math-types/
│   ├── algebramesh-core/
│   ├── algebramesh-server/
│   ├── algebramesh-agent/
│   ├── algebramesh-backend-asir/
│   ├── algebramesh-backend-macaulay2/
│   ├── algebramesh-backend-oscar/
│   ├── algebramesh-gkz/
│   └── algebramesh-cli/
├── bindings/python/
├── drivers/macaulay2/
├── drivers/oscar/
├── proto/
├── deploy/
├── examples/
├── tests/
├── docs/
└── .github/workflows/
```

Workspace 構成とし, protocol crate はできるだけ scheduler 依存を持たない. `algebramesh-openxm-protocol` と `algebramesh-openxm-client` は将来独立公開可能な構造にする.

## 13. テスト設計と受入条件

### 13.1 テストの層

- 単体テスト: CMO/OX encode-decode, byte-order, 不正な長さ・深さ, 型変換, job state transition.
- property-based / fuzz: 入力バイト列による parser panic, 異常な確保量, スタック不整合を防止.
- protocol integration: 実際の `ox_asir` に接続し, `SM_mathcap`, 数値・リスト往復, `SM_executeFunction`, `SM_popCMO` を確認.
- backend integration: Asir/Macaulay2/OSCAR で演算内容を検証. CAS 固有の結果表示に依存しない.
- cluster e2e: 2物理ホスト, 複数 worker, 障害注入, 再接続, キャンセル, ログ保存.
- mathematical regression: 既知の因数分解と上記 GKZ 例を固定 fixture とし, 版数更新時に検証する.

### 13.2 マイルストーン

| 版 | 範囲 | 必須の完了条件 |
|---|---|---|
| v0.1 | Rust OpenXM protocol/client | byte-order handshake, control/data 接続, CMO 往復, Asir 関数の実行と結果取得 |
| v0.2 | Rust agent + 分散実行 | 異なる物理2ホスト上の Asir worker に独立ジョブを配置・回収 |
| v0.3 | CAS 統合 | Asir と Macaulay2 を共通 API から実行し, 型付き結果・ログを保存 |
| v0.4 | OSCAR / Python | Julia driver と Python binding を追加し, 版数・能力情報を取得 |
| v0.5 | GKZ | `A,beta` から GKZ 生成元を取得し, 既知例の数学的関係を検証 |
| v1.0 | 研究用安定版 | 回帰試験, 障害対策, 再現性記録, ドキュメントとインストール手順が整う |

v1.0 以後の計画 (数学的な共通基盤, Workflow Engine, Verification Framework, 研究パッケージ) は [ロードマップ](roadmap.md) にある.

性能評価は 1, 2, 4, 8 worker 等の実行時間, CPU/RAM, 通信量, worker 故障時の回復時間を測定する. 各ジョブが独立な場合と, 単一ジョブを高速化しようとする場合は別の評価とする. 一律な線形高速化を合格条件にしない.

## 14. 最初の実装チケット

1. [ ] `algebramesh-openxm-protocol` crate を作り, OX tag, SM command, byte-order の定義を追加.
2. [ ] RFC の固定パケット例を golden test にし, Rust CMO encode/decode を実装.
3. [ ] `ReverseConnect` の Rust listener を作り, 既存 worker-entrypoint の `ox_launch` と接続.
4. [ ] control/data を正しく対応付け, `SM_mathcap` で利用可能機能を取得.
5. [ ] `SM_executeFunction` / `SM_popCMO` で整数・リストの往復と `fctr` を検証.
6. [ ] 1セッション1ジョブの実行管理と timeout/cleanup を追加.
7. [ ] `algebramesh-cli` から1ジョブを投げ, 結果を JSON として表示.
8. [ ] CI に encode/decode テストと, Docker 上の OpenXM 結合試験を追加.

**最初の成功条件:** Rust 製 CLI が既存 Asir worker を起動・接続し, `fctr(x^4-y^4)` 相当の処理を実行して, CMO から構造化された因数分解結果を得る. 既存 Asir master の `ox_rpc` への依存なしで達成すること.

## 15. 未確定事項と技術リスク

| 論点 | 対処方針 |
|---|---|
| `ox_launch` の細かい接続確立手順 | 最初に実パケットとプロセス起動を検証し, RFC とコードとの差分を記録 |
| CMO による多項式の無損失な変換 | Mathcap と実サーバの変換結果を検査し, 段階的に型を増やす |
| CAS の異なる ring/term order | 変換前に明示指定し, 対応不能なら拒否 |
| OpenXM session の reset/cancel | スタック状態が不明なら session ごと破棄して再生成 |
| ジョブの重複実行 | 実行状態不明を `LOST` と扱い, 再試行を純粋演算に限定 |
| Macaulay2/OSCAR の機械可読出力 | 専用 driver を用意し, stdout の人間向け表示を標準データ形式としない |
| GKZ の共鳴・局所解・monodromy | 数学的適用条件を明示し, 最初の GKZ ideal 生成と切り離す |
| ライセンス・配布サイズ | 依存関係のライセンス監査, イメージの分離とバージョン固定 |

## 16. 参考資料

- 既存実装: https://github.com/NAKANORyunosuke/Risa-Asir-container
- OpenXM ソース: https://github.com/openxm-org/OpenXM
- OpenXM-RFC 100, OX/TCP/IP encoding: https://www.math.kobe-u.ac.jp/OpenXM/Current/doc/OX-RFC-100-ja/OpenXM/node9.html
- OpenXM-RFC 100, Stack Machine commands: https://www.math.kobe-u.ac.jp/OpenXM/Current/doc/OX-RFC-100-ja/OpenXM/node15.html
- OpenXM-RFC 100, byte-order negotiation: https://www.math.kobe-u.ac.jp/OpenXM/1.2.3-12/doc/OX-RFC-100-ja/OpenXM/node30.html
- Asir distributed computation manual: https://www.math.sci.kobe-u.ac.jp/OpenXM/Current/doc/asir2000/html-en/man/man_47.html
- Macaulay2 `HolonomicSystems/gkz`: https://macaulay2.com/doc/Macaulay2/share/doc/Macaulay2/HolonomicSystems/html/_gkz.html
- OSCAR documentation: https://oscar-system.github.io/oscar-website/documentation/
- Rust Tokio: https://tokio.rs/
- Rust tonic: https://github.com/hyperium/tonic
- PyO3: https://pyo3.rs/

---

本設計は実行環境と各 CAS の実動作を検証する前の初期設計である. とくに OpenXM の起動手順, CMO の対応範囲, 外部 CAS からの構造化出力は実装開始時に検証し, 検証結果を Architecture Decision Record (ADR) として記録してから固定する.


## 17. 新規リポジトリ作成時の最小構成

以下は **作成予定の構成** であり, GitHub 上でリポジトリやファイルを作成済みという意味ではない.

```text
NAKANORyunosuke/algebramesh
├── Cargo.toml                # workspace
├── README.md                 # プロジェクト概要と構築方法
├── LICENSE                   # ライセンス確定後に追加
├── CONTRIBUTING.md
├── crates/
│   ├── algebramesh-openxm-protocol/
│   ├── algebramesh-openxm-client/
│   ├── algebramesh-math-types/
│   ├── algebramesh-core/
│   ├── algebramesh-server/
│   ├── algebramesh-agent/
│   ├── algebramesh-backend-asir/
│   ├── algebramesh-backend-macaulay2/
│   ├── algebramesh-backend-oscar/
│   ├── algebramesh-gkz/
│   └── algebramesh-cli/
├── bindings/python/
├── proto/
├── deploy/
│   ├── compose/
│   └── docker/
├── tests/
│   ├── fixtures/
│   └── integration/
├── examples/
├── docs/
│   ├── design.md
│   └── adr/
└── .github/workflows/
```

### 最初の Issue を切る順番

1. `chore: initialize Rust workspace and independent CI` — 新規リポジトリ作成, formatter, linter, workspace tests.
2. `feat(openxm): define OX framing and byte-order negotiation` — goldens test を先に用意する.
3. `feat(openxm): implement minimal CMO codec` — integer/string/list/error を対象にする.
4. `feat(openxm): implement reverse-connect session` — Rust listener が `ox_launch` の実接続を受ける.
5. `feat(asir): execute first fctr job` — CMO を構造化して取得する.
6. `feat(cli): show typed result in JSON` — `algebramesh` CLI で実行できること.

**v0.1 の完了条件:** `Risa-Asir-container` リポジトリにコミットを加えず, 新規 `algebramesh` 側だけのコードで Asir worker と接続・因数分解・結果回収を実現する.
