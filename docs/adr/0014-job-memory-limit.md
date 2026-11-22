# ADR 0014: ジョブごとのメモリの上限

- 状態: 採用
- 日付: 2026-11-05
- 関連: [ADR 0003](0003-result-check-and-deadlines.md), Issue #3

## 背景

`limits.memory_mb` は受け付けるが, どの backend もジョブごとには強制していなかった.
Asir backend はその旨をログに残すだけで, 成功したジョブの記録からは上限が守られたかどうかが分からなかった.

## 決定

- 申告: backend は強制できる上限を `Capabilities.enforced_limits` に申告する. `wall_seconds` はどの backend でも共通の実行処理が強制する. `memory_mb` を申告するのは, ジョブごとにプロセスを起動する backend だけである. 申告は agent の登録で scheduler に届き, `algebramesh backends` と `workers` の能力の JSON に出る. `backends` (`ListBackends`) は同じ backend の online の worker をまとめ, `capabilities.enforced_limits` にはそのすべてが強制する上限 (どの worker で動いても守られる) を, `available_limits` には少なくとも 1 台が強制する上限を示す. その上限付きのジョブは, ジョブの operation を申告する worker のどれかがその上限を強制するときに受け付けられる ([ADR 0022](0022-operation-types-and-signatures.md) で, operation を申告する worker に限った). `max_precision` はすべての worker の最小値である (Issue #15). worker ごとの申告は `workers` が示す.
- 強制: 申告した backend は, driver のプロセスに `RLIMIT_AS` (アドレス空間, 仮想メモリ) を掛ける (Unix). driver が起動したプロセスにも継承される. 上限の下で driver が結果を返さずに失敗し, ログがメモリの確保の失敗を示すか, シグナルで終わった場合は `RESOURCE_LIMIT` になる.
- 断る: 申告しない backend へのジョブは実行しない.
  - 投入時: その backend を持つ online の worker がいて, どれも強制しなければ `FAILED_PRECONDITION`. その backend の worker がまだいなければ受け付けて待たせる.
  - 配置: 強制しない worker には割り当てない.
  - 実行時: worker の共通の実行処理も `INVALID_ARGUMENTS` で断る.
- 値: `memory_mb` は 16 から 2^30 (MiB) まで.

| backend | `memory_mb` | 理由 |
|---|---|---|
| Macaulay2 | 強制する (Unix) | ジョブごとに M2 を起動する |
| Asir | 強制しない | 1 つの OpenXM のセッションが複数のジョブを処理する |
| OSCAR | 強制しない | 常駐する Julia のプロセスが複数のジョブを処理する |

## 結果

- `RLIMIT_AS` は実際に使ったメモリ (RSS) ではなく, 予約したアドレス空間を数える. CAS は起動時に大きく予約するので, 上限は余裕を持って決める. Macaulay2 1.25 は 640 MiB で起動し因数分解ができたが, 512 MiB では起動できなかった (スレッドの作成に失敗する).
- コンテナ全体の上限 (`docker run --memory` など) は worker のすべてのジョブの合計に掛かり, ジョブごとの上限とは別である. 両方を使うことができる.
- 強制しない backend でメモリを抑えたいときは, worker のコンテナに上限を掛け, slot を 1 にする.

## 確認

- 単体試験: 値の範囲, 申告しない backend の拒否 (共通の実行処理), 上限を超えて大きくなる driver の `RESOURCE_LIMIT` (Linux).
- scheduler の試験: 強制する worker がいないときの投入の拒否, 強制する worker だけへの配置.
