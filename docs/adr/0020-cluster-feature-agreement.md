# ADR 0020: 版の混在と機能の合意

- 状態: 採用
- 日付: 2026-11-06
- 関連: [ロードマップ §1.5](../roadmap.md), [ADR 0013](0013-scheduler-raft.md), [ADR 0017](0017-raft-storage-bounds-and-durability.md), [障害注入の記録](../audits/2026-11-06-fault-injection.md)

## 背景

ロードマップの Phase 1 から先では, 次の 3 種類の新しい意味を足していく.

| 種類 | 古い node と混ざると起きること |
|---|---|
| 新しい `Change` の variant | 古い node の `Store::apply_batch` は知らない variant で失敗し, その node の Raft が止まる (`raft stopped`) |
| 記録の新しい欄 (`JobRecord`, `BackendInfo` など) | 古い follower は `UpdateJob` の記録を自分の版の型で読み直して `record_json` に書くので, 知らない欄が黙って落ちる. node ごとに `record_json` が食い違い, snapshot を通じて広まる. leader が古いと, 欄はログに入る前に落ちる |
| 値の中の marker (#36 の `$blob` など) | 古い node は本体を取得も修復もできない. 古い leader は marker を値として返す |

cluster の全 node を同時に入れ替えることはできないので, 新しい意味は, 全 member が扱えると分かってから使う必要がある.

## 決定

**機能.**
- 新しい意味は, 名前の付いた機能 (feature) にまとめる. binary は, 自分が扱える機能の一覧を持つ.
- 1 つの機能には, 同じ版で入る意味だけをまとめる. 機能を名乗れるのは, その意味をすべて実装した binary だけだからである. ロードマップの `record_v2` (記録の新しい欄をすべて守る 1 つの機能) は, 欄の組ごとの機能に分ける.
- この ADR で定める機能は `backend_provenance` だけである. `BackendInfo` の欄 `driver_sha256` と `agent_build` を記録に書いてよいことを表す. 名前は提案であり, 後の機能 (結果の保証, blob の参照, cache の欄) の名前と合わせて確かめる.
- 結果の保証 (`JobRecord.guarantee`, ADR 0018) は機能 `result_guarantee` で守る (#47). 名前は提案である.
- #36 の marker を守る機能 `blob_refs` は, marker を扱う版 (B1) で定める. B0 の binary が名乗ると, marker を扱えないのに有効にされうるからである.
- 機能の合意の試験は, 試験のためだけの機能を足さず, `backend_provenance` で行う. 機能を要する操作は, 結果の記録に来歴の欄を書くことである.

**合意の手順.**
1. 各 node は, `NodeStatus` で自分の版 (`version`), 扱える機能 (`features`), cluster が有効にした機能 (`enabled_features`) を返す. B0 より前の node は `features` を返さない (空).
2. leader は定期的に, membership の全 member (投票者と learner) に `Status` を問い合わせる. まだ有効でない機能のうち, 全 member が扱えるものがあれば, `Change::EnableFeatures` を propose する.
   - 1 台でも答えないか, その機能を持たなければ, 有効にしない.
   - そのため, この変更を適用する時点で, 古い node は member にいない.
3. 有効な機能は, 状態機械の `meta` に記録する. それからは状態から読み, 問い合わせ直さない. 1 台が止まっても, 機能は無効にならない.
4. 機能の有効化と `add_member` は, leader の中の 1 つの lock で順に並べる. `add_member` は, 加える node に `Status` を問い合わせ, 有効な機能をすべて扱えなければ断る.
   - 有効化の commit を待つ時間が尽きると, lock は解かれるが, `EnableFeatures` は取り消されず, 後で commit されうる. そこで `add_member` は, 状態機械で有効な機能に加えて, leader の作業用の store に書いた有効化中の機能も扱えることを求める (#85).
   - 作業用の store は, それより前の term の entry をすべて適用した状態機械の写しなので, 前の leader が有効にした機能も含む. leader が scheduler をまだ始めていなければ, `add_member` は `UNAVAILABLE` で断る.
5. 有効化と同時に, `meta` に `min_binary_version` を記録する. 値は, 問い合わせた member の版のうち最も古いものである.
   - B0 以後の binary は, 自分の版がこれより古ければ起動を断る.
   - B0 より前の binary はこの検査を持たない. そのため, 機能を有効にした後は, B0 より前の版に戻さない. 戻した node は, `EnableFeatures` の適用で止まる.
6. scheduler が 1 台の構成では, 混在が起きないので, 扱える機能をすべて起動時に有効にする.

**client と agent への公開.**
- 有効な機能は, `RegisterResponse.features` と `ListBackendsResponse.server_features` で知らせる.
- agent は, 有効と知らされた機能だけを使う. 知らされていない機能の欄は報告に入れない.
- scheduler も, 有効でない機能の欄を報告から外してから記録する. agent が古い情報で入れても, 記録は食い違わない.
- 機能が新しい RPC を要するとき, 機能の合意を持つ scheduler は, 機能が有効でなければ `FAILED_PRECONDITION` を返す. それより古い scheduler は `UNIMPLEMENTED` を返す. client と agent は, どちらも「機能なし」と扱う. `backend_provenance` は新しい RPC を持たない.
- 最初の新しい RPC は, 読むだけの `LookupRequest` と `ListJobs` (W1, Issue #63) である. 確定した状態を読むだけで記録の形を変えないので, 機能には結び付けない. 古い scheduler は `UNIMPLEMENTED` を返すので, 版の混ざった cluster では, 新しい scheduler が leader の間だけ答える. client は, `UNIMPLEMENTED` を返した scheduler を飛ばして他の scheduler を試す. どの scheduler も答えず, 1 つ以上が `UNIMPLEMENTED` を返したときに「機能なし」(`ClientError::Unsupported`) とする. ただし, 1 巡の中に `UNAVAILABLE` を返した scheduler があれば, それは一時的に答えられない新しい leader かもしれない. そこで, 選挙の間と同じく `RETRY_UNAVAILABLE` (5 秒) まで巡り直し, その間に答えがなければ「機能なし」とする. すべての scheduler が `UNIMPLEMENTED` を返したときは, 巡り直しても変わらないので, すぐに「機能なし」とする (review の指摘 #83). 「機能なし」は永続的な性質ではない. 新しい scheduler が leader になるか, 古い scheduler を更新すれば, 同じ呼出しが成功しうる.

**来歴の欄 (`backend_provenance`).**
- `driver_sha256`: agent が起動時に計算する, backend の driver の file の SHA-256 である. driver の file を持たない backend (Asir) では空である.
- `agent_build`: agent を build した commit である. 作業ツリーに変更があったか, 分からないときは `-modified` を付ける. commit が分からなければ空である.
- image には, build の引数 `ALGEBRAMESH_BUILD_COMMIT` で commit を渡す. Rust の build が読む file (`Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `proto`, `crates`, Python の binding の Rust の部分) が commit と同じときだけ, `ALGEBRAMESH_BUILD_CLEAN=1` を渡す. 統合試験の script は, `tests/integration/build-env.sh` でこの 2 つを git から決める.
- `build` (`BackendOffer.build`, `BackendInfo.build`) は CAS の導入の識別子で, image の digest (`sha256:<64 桁の 16 進>` か `<名前>@sha256:<64 桁の 16 進>`) か, 40 桁の git commit とする. それ以外の値は来歴として扱わない (cache の鍵にしない).

## 結果

- 新しい `Change` の variant と記録の新しい欄は, 全 member を更新するまで使われない. 版の混ざった cluster は, 止まらず, 食い違わない.
- 機能を有効にした後は, B0 より前の版に戻せない (`docs/install.md` に書く).
- 更新の順序 (scheduler, agent, client) を `docs/install.md` に書く. ただし, 正しさをこの順序に頼ることはせず, 機能の合意で守る.

## 確認

- 1 プロセスの Raft の試験: 1 台が機能を持たない cluster では機能が有効にならず, 全 node の `record_json` が一致する. その node を新しい版で起動し直すと有効になり, その後で 1 台を止めても有効のままである. 機能を持たない node の `add_member` は断られる.
- 1 プロセスの Raft の試験 (#85): leader が有効化の entry の propose を止めている間に, 有効化の commit を待つ時間が尽きても, 機能を持たない node の `add_member` は断られる. entry を流すと機能が有効になり, その node は member にいない.
- Docker の試験 (`tests/integration/run-mixed-version.sh`, CI の `mixed-version`): B0 より前の commit から build した scheduler を 1 台含む cluster で確かめる.
  - leader が新しい node のときも古い node のときも, 機能は有効にならず, 結果に来歴の欄は入らない. どの node も止まらず, 全 node の記録 (`record_json`) が一致する.
  - 古い node を更新すると機能が有効になり, 結果に `agent_build` が入る. 記録は一致したままである.
  - 古い image の scheduler の `add-member` は断られる.
- `tests/integration/run-raft-e2e.sh` も, 各段の後で全 node の記録の digest を比べる (`tests/integration/records_digest.py`).
