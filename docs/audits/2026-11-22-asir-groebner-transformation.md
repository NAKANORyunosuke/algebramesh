# Asir で Groebner basis の変換行列を得られるか (2026-11-22)

- 追跡: Issue #69 (ロードマップ A3).
- 対象:
  - image `algebramesh-agent-asir:dev` (`sha256:4a68639a5d12...`). 元は Dockerfile の `ASIR_IMAGE` (`risa-asir-container@sha256:a572b603f710...`) である.
  - Asir の `version()` は `20260331` を返した. 参照した source は image の `/OpenXM_contrib2/asir2018/engine/nd.c` である.
  - Rust 側は commit `8641263` の `algebramesh-openxm-client` と, `algebramesh-backend-asir` の `convert` を使った.
- この Issue では, backend と driver のコードを変えていない. 試した script は scratchpad に置き, 要点をこの記録に写した.

## 結論

| 項目 | 結果 |
|---|---|
| 1. C と N を得られるか | 使える. `nd_gr(F, V, 0, O | gentrace=1)` と `nd_btog` で C を得る. `dp_true_nf_and_quotient` で N を得る |
| 2. 宣言した項順序を渡せるか | 使える. lex, grlex, grevlex, 重み, block のどれも表せる. ただし, 重みに上限がある |
| 3. G, C, N を CMO で無損失に受け取れるか | 使える. 多項式のリストのリストとして受け取る. Asir の行列や distributed polynomial のままでは送らない |

決定: Asir に `ideal.groebner_basis` を足す. 対象は `representation` の certificate とし, Issue を起票した (下の「決定」).

## 1. C と N

### 手順

- **G と C.** 次の 2 つを呼ぶ.
  - `R = nd_gr(F, V, 0, O | gentrace=1)`: `R[0]` が G である.
  - `C = nd_btog(F, V, 0, O, R)`: 第 5 引数には, trace の一部ではなく `R` 全体を渡す. `R[1]` を渡すと, Asir が SEGV で落ちた.
- **C の形と掛ける向き.**
  - C は #F 行 #G 列の行列で, G_j = Σ_i F_i C_ij を満たす.
  - 成分は distributed polynomial なので, `dp_dtop(C[I][J], V)` で多項式に直す.
  - `nd_btog` は進み具合の数を stdout に出す.
- **整数係数への直し.** `nd_gr` に有理数係数の F を渡すと, trace を取るときに `ptond : input must be integer-coefficient` で断られる.
  - そこで, `F = map(ptozp, F0)` と `Cf_i = F0_i / F_i` で整数係数に直してから, `nd_gr` に渡す.
  - 返った C は, 行 i を Cf_i で割って元の F0 に戻す.
- **N.** `dp_ord(O)` の後に, 各 i について `Q = dp_true_nf_and_quotient(Ind, dp_ptod(F_i, V), GV)` を呼ぶ.
  - `Ind` は [0, ..., #G-1], `GV` は G の distributed polynomial のベクトルである.
  - 返る形は `[nf, dn, quotients]` で, dn · F_i = Σ_k quotients[k] · G_k + nf を満たす.
  - F_i ∈ (G) なので nf = 0 となる. dn は ±1 などの定数で, 小さな例でも -1 が返った.
  - よって, N_ki = Cf_i · quotients[k] / dn は F0_i = Σ_k G_k N_ki を満たす.
- **Asir の G の形.**
  - G は reduced である. ただし monic ではなく, 各元を整数係数で primitive にしてある.
  - 元の順は Asir が決める.
  - 上の検査は, この G のままで行った.
  - 契約の形 (monic, 先頭項の降順) にそろえるのは driver の仕事である. G を lc で割って並べ替えれば, C の列と N の行も同じく割って並べ替える.

### 確かめた例

| 例 | 順序 | G = F C | F = G N |
|---|---|---|---|
| twisted cubic (xz - y^2, xw - yz, yw - z^2) | grevlex, lex | 成り立つ | 成り立つ |
| (x^2 y - z/2, 2xy^2 - x, 3xz - y) | lex, grevlex, grlex | 成り立つ | 成り立つ |
| 同じもの | 重み (1, 2, 3) の後に grevlex. 重み (5, 0, 1) の後に lex. 重み (2^20, 1, 0) の後に grevlex | 成り立つ | 成り立つ |
| twisted cubic | block 3 種 (grevlex/grevlex, lex/grevlex, grlex/lex) | 成り立つ | 成り立つ |
| 単位イデアル (x + 1, x) | grevlex | 成り立つ. G = [1] | 成り立つ |
| 1 元 (2x^2 y - 3) | grevlex | 成り立つ | 成り立つ |

- 判定は 2 か所で行った.
  - Asir の中: 両辺の差を `red` で簡約して 0 になるか.
  - Rust: 下の 3 の手順で受け取った値を, `algebramesh-math-types` の多項式で掛けて足し, 比べた.

### 零の生成元

- `nd_gr` は零の生成元を捨てる. その結果, `nd_btog` の C の行が F と合わなくなる (G = F C が 2 列で崩れた).
- また, `dp_true_nf_and_quotient` は零の多項式で `getarray : array or list expected` になる.
- そこで driver は, 零の生成元を除いてから Asir に渡す. その上で, C には零の行を, N には零の列を戻す.

## 2. 項順序

### 対応

| ADR 0023 の順序 | Asir の順序 |
|---|---|
| lex | `2` |
| grlex | `1` |
| grevlex | `0` |
| 重み w の後に `then` | 行列順序 `newmat(n+1, n, [w, 行_1, ..., 行_n])` |
| block | `[[o_1, k_1], ..., [o_m, k_m]]`. o_i は上の 0, 1, 2 で, k_i は区画の変数の数 |

- 行列の 2 行目以降は, `then` を次のように表す.
  - lex: 単位行列の行.
  - grlex: (1, ..., 1) と, 単位行列の最初の n - 1 行.
  - grevlex: (1, ..., 1), (1, ..., 1, 0), ..., (1, 0, ..., 0).
- grevlex を (1, ..., 1), (0, ..., 0, -1), ... と負の成分で書く方法もある. 次数が等しければ, 後ろの変数の指数が小さいことと, 前の変数の指数の和が大きいことは同じである. そこで, 負の成分を使わない上の行を選んだ.
- block の中身は重みを含まない 3 つだけなので, ADR 0023 の block はすべて表せる.

### 重みの上限

- `nd_setup_parameters` は, 次の式が成り立つと `too large weight` で断る.
  - 式: (指数 1 つ分の bit 数 `nd_bpe`) + (重みの最大値の bit 数) + (変数の数の bit 数) ≥ 31.
  - 制御: `ctrl("weight_check")`. 既定は 1.
  - `nd_bpe` は入力の最大の指数で決まり, 計算の途中で指数が伸びると大きくなる.
- 実際の結果は次のとおりである.
  - 3 変数で重み 2^20 は通った.
  - 2^24 は断られた.
  - MAX_WEIGHT (2^31 - 1) は断られた.
- 断られた場合は CAS の誤りとして返る. 溢れたまま計算を続けることはない. ADR 0023 の上限 2^31 - 1 より狭い.
- `nd_btog` は内部で `nd_bpe = 32` に固定する (`nd.c` の `nd_btog`). そのため, 重みを含む順序では常に `too large weight` になる.
  - 確かめた回避策: `nd_btog` の間だけ `ctrl("weight_check", 0)` にする.
  - `nd_btog` が扱うのは `nd_gr` の trace と同じ多項式なので, `nd_gr` の検査に通った指数の範囲に収まる.
  - それでも C か N が崩れれば, 検査器の `gb.contained` と `gb.contains` で断られる. 先頭項の取り違えは `gb.reduced_form` で断られる.

## 3. CMO での受け渡し

### 手順

- 小さな Rust の program を, scratchpad に作った.
  - 依存先: `algebramesh-openxm-client` (`spawn_local`), `algebramesh-backend-asir::convert` (`polynomial_to_cmo`, `cmo_to_polynomial`), `algebramesh-math-types`.
  - これを Asir の image の中で, `/OpenXM/lib/asir/ox_launch` と `/OpenXM/bin/ox_asir` に対して動かした.
- 次の順に呼んだ.
  1. mathcap を受け取る.
  2. `load("/w/gbprobe.rr")` で, 下の Asir の関数を読む.
  3. `am_gb(F, V, O)` を `SM_executeFunction` で呼び, `[G, C, N]` を受け取る.
     - C は #F 個の行のリストで, G_j = Σ_i F_i C[i][j] を満たす.
     - N は #F 個の行のリストで, F_i = Σ_k G_k N[i][k] を満たす.
  4. Rust で `cmo_to_polynomial` で読み, 2 つの恒等式を確かめる.
  5. 読んだ値を `polynomial_to_cmo` で送り返す. `am_check` が, Asir で計算し直した値と等しいかと, 恒等式を確かめる.

### 結果

- **mathcap.**
  - `ox_asir` の版は 199901160 である.
  - `SM_executeFunction` (269) を受け付ける.
  - DATA で受け付ける CMO は次のとおりである: 1, 2, 3, 4, 5, ERROR2, 17 (LIST), 19, 20 (ZZ), 21 (QQ), 22 (ZERO), 24, 25, 26, 31 (DISTRIBUTED_POLYNOMIAL), 34, 27 (RECURSIVE_POLYNOMIAL), 33, 60 (INDETERMINATE), 61, 52, 35.
  - CMO_ARRAY (16) と CMO_MATRIX はない. そこで, C と N は行列ではなく, 多項式のリストのリストにして送る.
  - `algebramesh-openxm-protocol` は DISTRIBUTED_POLYNOMIAL を読まない. そこで, 成分は `dp_dtop` で recursive polynomial に直す.
- **変換結果.** 次の 6 例で, Rust で確かめた恒等式の失敗は 0 だった. また, 送り返した値で Asir の `am_check` は `[1, 0]` を返した (計算し直した値と同じで, 恒等式の失敗は 0).
  - twisted cubic, grevlex.
  - 有理数係数の 3 元, lex.
  - 係数 2^70 + 1 と -1/3, grlex (72 bit の係数が往復した).
  - 重み (1, 2, 3) の後に grevlex (行列は CMO のリストで送り, Asir 側で `newmat` に直した).
  - block (x, y に lex, z, w に grevlex).
  - 単位イデアル.
- **零の表し方.** 多項式 0 と数 0 は, `CMO_NULL` で返った. `cmo_to_rational` は NULL を 0 と読む.

### 見つかった落とし穴

driver を書くときに守ること. どれも probe で実際に起きた.

1. **変数のリストを CMO_INDETERMINATE で送ると, 誤った値が黙って返る.**
   - V を `CMO_INDETERMINATE` のリストで送ると, `nd_gr` を直接呼んだときは `invalid argument` で断られた.
   - ところが, `am_gb` の中では誤りにならず, G = [1], C = 0 が返った.
   - F の変数が V の変数と別のものとして扱われ, 係数の側に回ったと考えられる.
   - V は, 各変数の recursive polynomial (`x` そのもの) のリストで送る.
   - なお, この誤った値は G = F C を満たさないので, 検査器の `gb.contained` で断られる.
2. **関数の中の誤りで, server が止まる.**
   - 読み込んだ関数の中で誤りが起きると, `ox_asir` は debugger に入って入力を待つ. その結果, 応答が返らない (90 秒の timeout で止めた).
   - session の初めに `ctrl("no_debug_on_error", 1)` を呼ぶと, `CMO_ERROR2` が返るようになった.
   - 組込み関数 (`fctr`) を直接呼ぶ今の backend では, 起きない.
3. **整数は ZZ で送る.**
   - `ctrl` と `dp_ord` は, `CMO_INT32` の引数を `ZTOS : invalid argument` や `invalid order specification` で断った.
   - 順序と行列の成分を含め, 整数は `CMO_ZZ` で送る.

### 使った Asir の関数

```
def am_order(O) {
  if (type(O) == 4 && length(O) == 2 && type(O[0]) == 7) {
    Rows = O[1];
    return newmat(length(Rows), length(Rows[0]), Rows);
  }
  return O;
}
def am_gb(F0, V, O0) {
  O = am_order(O0);
  S = length(F0);
  F = map(ptozp, F0);
  Cf = newvect(S);
  for (I = 0; I < S; I++) Cf[I] = red(F0[I]/F[I]);
  R = nd_gr(F, V, 0, O | gentrace=1);
  G = R[0];
  T = length(G);
  Wc = ctrl("weight_check"); ctrl("weight_check", 0);
  C = nd_btog(F, V, 0, O, R);
  ctrl("weight_check", Wc);
  CL = [];
  for (I = S-1; I >= 0; I--) {
    Row = [];
    for (J = T-1; J >= 0; J--) Row = cons(red(dp_dtop(C[I][J], V)/Cf[I]), Row);
    CL = cons(Row, CL);
  }
  dp_ord(O);
  GV = newvect(T, map(dp_ptod, G, V));
  Ind = [];
  for (K = T-1; K >= 0; K--) Ind = cons(K, Ind);
  NL = [];
  for (I = S-1; I >= 0; I--) {
    Q = dp_true_nf_and_quotient(Ind, dp_ptod(F[I], V), GV);
    if (Q[0] != 0) error("am_gb: nonzero remainder");
    Row = [];
    for (K = T-1; K >= 0; K--) Row = cons(red(Cf[I]*dp_dtop(Q[2][K], V)/Q[1]), Row);
    NL = cons(Row, NL);
  }
  return [G, CL, NL];
}
```

`am_check(F0, V, O0, G, CL, NL)` は, `am_gb` を呼び直して等しいかを確かめ, 2 つの恒等式の失敗を数える.

## 確かめていないこと

- `s_pairs` の certificate.
  - S 対を `dp_true_nf_and_quotient` で割った商が, lm(h_k g_k) < L_ij を満たすかは試していない.
  - ADR 0026 の検査器は, 満たさなければ断る.
- 大きな入力での時間と記憶. `nd_btog` は trace を最初からたどり直す.
- `ideal.normal_form` と `ideal.membership`. 商は上の N と同じ関数で得られる見込みだが, 試していない.
- 重みの上限の正確な境目. 上の不等式による. `nd_bpe` が計算の途中で変わるので, 事前には決まらない.

## 決定

- Asir に `ideal.groebner_basis` を足す.
  - 理由 1: 3 つの項目のどれも, 使える手順が見つかった.
  - 理由 2: K4 の照合では, Singular を共有しうる M2 と OSCAR ではなく, M2 と Asir の組で reduced Groebner basis を比べる.
- 起票した Issue は #94 である. 範囲は次のとおりである.
  - driver の関数を読む仕組み.
  - 上の落とし穴への対処.
  - 順序の変換と, 重みの上限の扱い.
  - `representation` の certificate.
  - `s_pairs` の調査.
- 時期は, A3 の残り (v1.4) の後とし, K4 より前とする. v1.4 の受入条件には含めない.
