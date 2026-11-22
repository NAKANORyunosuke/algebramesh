# ADR 0003: 結果の検算と制限時間の扱い

- 状態: 採用
- 日付: 2026-11-03
- 関連: 設計書 §3.2 correctness, §5.4, §6 規則4, §7.2

## 背景

`polynomial.factor` の結果は CAS が返した因数のリストである.
設計書は表示文字列による同値判定を禁じ, 指定した代数系の中で検算するよう定めている.
初版では検算を非同期処理の中で同期的に実行したため, 重い入力では制限時間 (`wall_seconds = 3`) が効かず 93 秒後に `SUCCEEDED` になった.
実 `ox_asir` 上の (x+y+z+1)^30 (x-y+2z+3)^30 (39680 項) で確認した.

## 決定

- `polynomial.factor` は因数の積を `algebramesh-math-types` で展開し, 入力と構造的に一致した場合だけ成功とする. 結果には `verified_product: true` を付ける. 一致しなければ `UNSUPPORTED_RESULT` で失敗させる.
- 検算は OpenXM セッションを閉じた後に blocking thread で行い, CAS 呼出しと同じ締切 (ジョブ開始からの `wall_seconds`) の下に置く. (すべての backend への適用と, 期限を過ぎた検算の扱いは [ADR 0016](0016-job-deadline-covers-result-check.md) に書いた.)
- 多項式の積は, 係数がすべて整数なら整数演算で行い, 冪は二乗の繰り返しで計算する. 上記の例では CAS 0.8 秒, 検算 2.7 秒になった.
- CAS 呼出し中に締切を過ぎるか取り消された場合は, セッションを使用不可にし, `SM_control_kill` と `SM_shutdown` で server と launcher を終了させる. 実機で, 終了後に `ox_asir` と `ox_launch` のプロセスが残らないことを確認した.
- 記録上の状態は, 締切超過が `TIMED_OUT`, 取消しが `CANCELLING` を経た `CANCELLED`, 接続断が `LOST` である. `LOST` を `FAILED` と同一視しない.

## 残る課題

- 締切超過後も blocking thread 上の検算は最後まで走る. 結果は破棄するが CPU は使う.
- `limits.memory_mb` は v0.1 では強制しない. ジョブ記録の `log` にその旨を残し, コンテナの制限で代える. (この扱いは [ADR 0014](0014-job-memory-limit.md) で改めた. 強制できない backend は `memory_mb` 付きのジョブを断る.)
