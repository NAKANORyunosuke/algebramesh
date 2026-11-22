-- AlgebraMesh driver for Macaulay2: the GKZ package (Issues #99, #100).
--
-- algebramesh.m2 loads this file when it is next to it, before it reads the
-- request; the macaulay2 backend offers gkz.* only when it is built with the
-- feature pkg-gkz, and then hashes this file with the driver. It uses the
-- helpers of algebramesh.m2 (fail, parseRational, termsJSON, gensJSON,
-- rationalsJSON, jsonString, systemInfo) and registers its operations in
-- packageOperations. The requests:
--   {"operation": "gkz.generate", "A": [[...], ...], "beta": ["p/q", ...],
--    "expected": [[{"coefficient": "p/q", "exponents": [x..., d...]}, ...], ...]}
--   {"operation": "gkz.indicial", "A": ..., "beta": ..., "weight": [w_1, ..., w_n]}
--   {"operation": "gkz.series", "A": ..., "beta": ..., "weight": [...], "order": k,
--    "max_terms": m}
-- The answers are read by crates/algebramesh-backend-macaulay2/src/packages/gkz.rs;
-- algebramesh-gkz builds the value from them and checks it (docs/claims.md).

-- A, beta and the GKZ ideal of a request.
gkzSetup = req -> (
    rows := req#"A";
    if not instance(rows, List) or #rows == 0 then fail "invalid A";
    A := matrix apply(rows, r -> apply(r, parseInteger));
    beta := apply(req#"beta", parseRational);
    if #beta != numrows A then fail "beta has the wrong length";
    I := gkz(A, beta);
    (A, beta, I, ring I, numcols A)
);

gkzRequest = req -> (
    (A, beta, I, W, n) := gkzSetup req;
    -- toricIdealPartials returns an ideal of a ring in D_1..D_n only; its
    -- generators are mapped into W (by variable name) before use.
    toric := toricIdealPartials(A, W);
    euler := eulerOperators(A, beta, W);
    -- Left ideal comparisons in the Weyl algebra use Groebner bases.
    J := ideal apply(flatten entries gens toric | euler, g -> sub(g, W));
    sameIdeal := I == J;
    -- Optional expected generators, as exponent vectors over x_1..x_n, D_1..D_n
    -- (normally ordered: x to the left of D).
    expected := if req#?"expected" then apply(req#"expected", g -> sum(g, term -> (
        e := term#"exponents";
        if not instance(e, List) or #e != 2 * n or any(e, k -> not instance(k, ZZ) or k < 0) then
            fail "invalid exponent vector in expected";
        parseRational(term#"coefficient") * product(2 * n, i -> W_i^(e#i))
    ))) else {};
    expectedIn := apply(expected, g -> isSubset(ideal g, I));
    -- For an operator found in I, its left coefficients over the toric
    -- generators and then the Euler operators (Issue #102): g = sum_k q_k g_k,
    -- which algebramesh-gkz checks by products in the Weyl algebra. In the
    -- Weyl algebra, Q = F // G gives G * Q = F with the entries of Q on the
    -- left (as checked in Macaulay2 1.24.11). null when the division leaves a
    -- remainder, which the backend then reports without a certificate.
    G := matrix{apply(flatten entries gens toric, g -> sub(g, W)) | apply(euler, g -> sub(g, W))};
    coefficients := apply(#expected, j -> (
        if not expectedIn#j then "null" else (
            F := matrix{{expected#j}};
            Q := F // G;
            if G * Q - F == 0 then gensJSON flatten entries Q else "null")));
    "{\"n\":" | toString n
    | ",\"generated_by_toric_and_euler\":" | toJSON sameIdeal
    | ",\"expected_in_ideal\":" | toJSON expectedIn
    | ",\"membership_coefficients\":[" | demark(",", coefficients) | "]"
    | ",\"variables\":" | toJSON(apply(gens W, toString))
    | ",\"generators\":" | gensJSON(flatten entries gens I)
    | ",\"toric\":" | gensJSON(apply(flatten entries gens toric, g -> sub(g, W)))
    | ",\"euler\":" | gensJSON(apply(euler, g -> sub(g, W)))
    | ",\"info\":" | systemInfo() | "}"
);

-- The indicial ideal of the GKZ ideal I for the weight w (SST, Theorem 2.3.9) and
-- its roots with multiplicities. The initial ideal must be torus-fixed, which holds
-- for generic w; otherwise the request is rejected rather than answered for a
-- different weight.
indicialData = (req, I, W, n) -> (
    w := req#"weight";
    if not instance(w, List) or #w != n then fail "weight has the wrong length";
    w = apply(w, parseInteger);
    J := inw(I, (-w) | w);
    if not isTorusFixed J then
        fail "the initial ideal for this weight is not torus-fixed; choose a generic weight";
    T := first createThetaRing W;
    ind := distraction(J, T);
    if dim ind > 0 then fail "the indicial ideal is not zero-dimensional; choose a generic weight";
    roots := apply(primaryDecomposition ind, C -> (
        point := try first entries lift(vars T % radical C, QQ) else
            fail "an exponent is not rational";
        {degree C, apply(point, q -> q + 0/1)}));
    (w, ind, holonomicRank I, roots)
);

indicialJSON = (n, W, ind, hrank) -> (
    "{\"n\":" | toString n
    | ",\"variables\":" | toJSON(apply(gens W, toString))
    | ",\"holonomic_rank\":" | toString hrank
    | ",\"indicial\":" | gensJSON(flatten entries gens ind)
);

indicialRequest = req -> (
    (A, beta, I, W, n) := gkzSetup req;
    (w, ind, hrank, roots) := indicialData(req, I, W, n);
    indicialJSON(n, W, ind, hrank)
    | ",\"exponents\":[" | demark(",", apply(roots, r ->
        "{\"multiplicity\":" | toString(r#0) | ",\"point\":" | rationalsJSON(r#1) | "}"))
    | "],\"info\":" | systemInfo() | "}"
);

-- Falling factorial a (a-1) ... (a-k+1).
falling = (a, k) -> product(k, i -> a - i);
isNonnegativeInteger = q -> denominator q == 1 and q >= 0;
isNegativeInteger = q -> denominator q == 1 and q < 0;

-- The Gamma-series with exponent v (SST, Chapter 3)
--   sum over u in ker_ZZ A of [v]_(u_-) / [v+u]_(u_+) x^(v+u),
-- over the u that keep the negative integer support of v+u equal to that of v,
-- truncated to w.u <= k. L is a lattice basis of ker_ZZ A (as columns).
-- Returns ("computed", terms) or ("unsupported", reason). Whether the result solves
-- the GKZ system is checked by the caller, not assumed here.
gammaSeries = (v, L, w, k, maxTerms) -> (
    n := #v;
    r := numcols L;
    pts := if r == 0 then {map(ZZ^0, ZZ^1, 0)} else (
        rowsH := {};
        rhs := {};
        for j to n - 1 do (
            Lj := L^{j};
            if isNonnegativeInteger(v#j) then (rowsH = append(rowsH, -Lj); rhs = append(rhs, v#j))
            else if isNegativeInteger(v#j) then (rowsH = append(rowsH, Lj); rhs = append(rhs, -1 - v#j)));
        rowsH = append(rowsH, matrix{w} * L);
        rhs = append(rhs, k);
        P := polyhedronFromHData(promote(fold((a, b) -> a || b, rowsH), QQ),
            promote(transpose matrix{rhs}, QQ));
        if not isCompact P then return ("unsupported",
            "the support of the series is not bounded by the weight");
        latticePoints P);
    if #pts > maxTerms then return ("unsupported", "more than " | toString maxTerms | " terms");
    terms := select(apply(pts, m -> (
        u := flatten entries (L * lift(m, ZZ));
        num := product(n, j -> if u#j < 0 then falling(v#j, -u#j) else 1);
        den := product(n, j -> if u#j > 0 then falling(v#j + u#j, u#j) else 1);
        if num == 0 or den == 0 then null else (u, num / den))), t -> t =!= null);
    ("computed", terms)
);

-- The primitive integer vector on the line of d, with its first non-zero entry
-- positive.
primitiveVector = d -> (
    g := fold(gcd, 0, d);
    if g == 0 then return d;
    s := if (select(d, x -> x != 0))#0 < 0 then -1 else 1;
    apply(d, x -> x // (s * g))
);

-- Directions d in ker_ZZ A for perturbing exponents, at most `limit` of them: for
-- each set Z of coordinates in J, taken by increasing size, the kernel vectors
-- that vanish on Z (a basis and one combination). Vanishing on Z keeps those
-- coordinates of the exponent fixed, which bounds more of the perturbed series.
perturbationDirections = (L, J, limit) -> (
    r := numcols L;
    out := {};
    for Z in flatten apply(#J + 1, s -> subsets(J, s)) do (
        if #out >= limit then break;
        N := if #Z == 0 then id_(ZZ^r) else gens kernel(L^Z);
        if numcols N == 0 then continue;
        B := L * N;
        candidates := apply(numcols B, i -> flatten entries B_{i})
            | {flatten entries(B * transpose matrix{apply(numcols B, i -> 2^i)})};
        for d in candidates do (
            if all(d, x -> x == 0) then continue;
            p := primitiveVector d;
            if not member(p, out) then out = append(out, p)));
    take(out, limit)
);

-- Solutions with exponent v of multiplicity m, from perturbing v along d in ker A
-- (SST, Section 3.5): F(e) = sum_u c_u(e) x^(v+u) x^(e d), the Gamma-series with
-- exponent v + e d, is annihilated by H_A(beta) for every e, and so is each
-- coefficient of its expansion in e. With x^(e d) = sum_j e^j (d . log x)^j / j!
-- and (d . log x)^j / j! = sum_(|k|=j) d^k l^(k) in divided powers
-- l^(k) = prod_i (log x_i)^(k_i) / k_i!, the coefficient of e^b is
--   phi_b = sum_u sum_(a+j=b) [e^a] c_u(e) x^(v+u) sum_(|k|=j) d^k l^(k),
-- with leading part (d . log x)^b / b! at x^v. Only u with c_u(e) = O(e^b) or
-- larger contribute: c_u has a zero of order (number of coordinates where v is a
-- non-negative integer and v+u is negative) minus (number where v is a negative
-- integer and v+u is non-negative). Those u lie in finitely many polyhedra, one
-- per pattern of sign changes; phi_b is computed only while all those polyhedra
-- are bounded by w.u <= k. Poles at e = 0 (z < 0) are not supported; a zero of
-- the denominator that the numerator shares is cancelled before the expansion.
-- A phi_b with a nonzero term below x^v (u != 0, w.u <= 0) is not returned.
-- Returns (bmax, {phi_1, ..., phi_bmax}, reason) with phi_b a list of
-- (u, k, coefficient).
logSeries = (v, L, d, w, k, m, maxTerms) -> (
    n := #v;
    J := select(n, j -> denominator(v#j) == 1);
    free := select(J, j -> d#j != 0);
    if #free > 12 then return (0, {}, "more than 12 integer coordinates to perturb");
    bmax := m - 1;
    reason := "";
    polyhedra := {};
    for s in subsets free do (
        z := #select(s, j -> v#j >= 0) - #select(s, j -> v#j < 0);
        if z > bmax then continue;
        rowsH := {matrix{w} * L};
        rhs := {k};
        for j in J do (
            Lj := L^{j};
            -- v_j + u_j >= 0 if a non-negative coordinate keeps its sign or a
            -- negative one changes it; v_j + u_j <= -1 otherwise.
            if member(j, s) != (v#j >= 0) then (rowsH = append(rowsH, -Lj); rhs = append(rhs, v#j))
            else (rowsH = append(rowsH, Lj); rhs = append(rhs, -1 - v#j)));
        P := polyhedronFromHData(promote(fold((a, b) -> a || b, rowsH), QQ),
            promote(transpose matrix{rhs}, QQ));
        if isEmpty P then continue;
        if not isCompact P then (
            if z <= 0 then return (0, {}, "the perturbed series is not bounded by the weight");
            bmax = z - 1;
            reason = "the perturbed series is not bounded by the weight at log degree " | toString z;
            continue);
        polyhedra = append(polyhedra, (z, P)));
    if bmax == 0 then return (0, {}, reason);
    pts := {};
    for zP in polyhedra do if zP#0 <= bmax then (
        lp := latticePoints zP#1;
        if #lp > 0 and zP#0 < 0 then
            return (0, {}, "the perturbed Gamma-series has a pole at e = 0 within the order");
        pts = pts | apply(lp, p -> flatten entries(L * lift(p, ZZ)));
        if #pts > maxTerms then return (0, {}, "more than " | toString maxTerms | " terms"));
    R := QQ[getSymbol "e"];
    eps := R_0;
    -- The coefficients of e^s, ..., e^(s + bmax) of f.
    taylor := (f, s) -> apply(bmax + 1, i -> lift(coefficient(eps^(s + i), f), QQ));
    -- The order of the zero of f at e = 0 (f != 0).
    zeroOrder := f -> min apply(exponents f, ex -> first ex);
    vt := apply(n, j -> v#j + d#j * eps);
    coefficients := apply(pts, u -> (
        num := product(n, j -> if u#j < 0 then product(-u#j, i -> vt#j - i) else 1_R);
        den := product(n, j -> if u#j > 0 then product(u#j, i -> vt#j + u#j - i) else 1_R);
        -- Each sign change from v_j < 0 to v_j + u_j >= 0 puts a factor d_j e into
        -- den, and each one the other way a factor d_j e into num. A zero of den
        -- at e = 0 that num shares is removable: the coefficients of num / den are
        -- those of (num / e^s) / (den / e^s) with s = zeroOrder den (Issue #88).
        if num == 0 then fail "internal error: a zero factor in the perturbed series";
        s := zeroOrder den;
        if zeroOrder num < s then fail "internal error: pole in the perturbed series";
        nc := taylor(num * 1_R, s);
        dc := taylor(den * 1_R, s);
        g := new MutableList from toList(bmax + 1 : 0/1);
        for t to bmax do g#t = (nc#t - sum(toList(1..t), i -> dc#i * g#(t - i))) / dc#0;
        (u, toList g)));
    -- A term with u != 0 and w.u <= 0 whose coefficient of e^t is not 0 lies
    -- below x^v in phi_b for every b >= t, so x^v would not lead them: phi_b is
    -- returned only for b < t, and the reason says why the others are missing.
    low := bmax + 1;
    for uc in coefficients do (
        u := uc#0;
        if any(u, x -> x != 0) and sum(n, j -> w#j * u#j) <= 0 then (
            t := position(uc#1, c -> c != 0);
            if t =!= null and t < low then low = t));
    if low <= bmax then (
        bmax = low - 1;
        reason = "the perturbed series has a term below x^v at log degree " | toString low);
    if bmax <= 0 then return (0, {}, reason);
    -- The exponents k with |k| = j and d^k != 0, with d^k.
    logParts := j -> select(apply(compositions(n, j), kk -> (kk, product(n, i -> (d#i)^(kk#i)))),
        p -> p#1 != 0);
    (bmax, apply(toList(1..bmax), b -> flatten apply(coefficients, uc ->
        flatten apply(b + 1, j -> (
            c := uc#1#(b - j);
            if c == 0 then {} else apply(logParts j, p -> (uc#0, p#0, c * p#1)))))), reason)
);

-- Up to m independent solutions with exponent v (multiplicity m): the
-- Gamma-series, then phi_b for the directions in turn, kept when the leading part
-- (d . log x)^b is independent of those kept so far.
logBasis = (v, L, directions, w, k, m, maxTerms) -> (
    n := #v;
    l := getSymbol "l";
    T := QQ[l_0 .. l_(n - 1)];
    zeros := toList(n : 0);
    sols := {};
    forms := {};
    reasons := {};
    (status, gamma) := gammaSeries(v, L, w, k, maxTerms);
    if status == "computed" then (
        sols = {(0, null, apply(gamma, t -> (t#0, zeros, t#1)))};
        forms = {1_T})
    else reasons = append(reasons, gamma);
    for d in directions do (
        if #sols >= m then break;
        (bmax, phis, reason) := logSeries(v, L, d, w, k, m, maxTerms);
        if reason != "" then reasons = append(reasons, reason);
        for b from 1 to bmax do (
            if #sols >= m then break;
            f := (sum(n, i -> d#i * T_i))^b;
            if rank((coefficients matrix{forms | {f}})#1) > #forms then (
                sols = append(sols, (b, d, phis#(b - 1)));
                forms = append(forms, f))));
    (sols, unique reasons)
);

termJSON = t -> "{\"shift\":" | toJSON(t#0) | ",\"coefficient\":" | jsonString(t#1) | "}";
logTermJSON = t -> (
    "{\"shift\":" | toJSON(t#0) | ",\"log_powers\":" | toJSON(t#1)
    | ",\"coefficient\":" | jsonString(t#2) | "}");
solutionJSON = s -> (
    "{\"log_degree\":" | toString(s#0)
    | ",\"direction\":" | (if s#1 === null then "null" else toJSON(s#1))
    | ",\"terms\":[" | demark(",", apply(s#2, logTermJSON)) | "]}");

-- A certificate that the solutions of one exponent are linearly independent
-- (Issue #102): B has a row for each log monomial (log_powers) of the terms
-- with shift 0 and a column for each solution, with the coefficient of that
-- term; rows are picked while they raise the rank, and the inverse of the
-- square matrix B_R they give is returned with them. algebramesh-gkz checks
-- B_R * inverse = 1. null when the shift-0 parts are dependent.
independenceJSON = sols -> (
    if #sols == 0 then return "null";
    zeroTerms := apply(sols, s -> select(s#2, t -> all(t#0, e -> e == 0)));
    monomials := unique flatten apply(zeroTerms, ts -> apply(ts, t -> t#1));
    if #monomials < #sols then return "null";
    B := matrix(QQ, apply(monomials, m -> apply(zeroTerms, ts ->
        sum(select(ts, t -> t#1 == m), t -> t#2) * 1_QQ)));
    picked := {};
    for i from 0 to numrows B - 1 do (
        if #picked == numcols B then break;
        if rank B^(picked | {i}) > #picked then picked = append(picked, i));
    if #picked < numcols B then return "null";
    inv := inverse B^picked;
    "{\"log_powers\":" | toJSON(apply(picked, i -> monomials#i))
    | ",\"inverse\":" | toJSON(apply(entries inv, r -> apply(r, toString))) | "}"
);

seriesRequest = req -> (
    (A, beta, I, W, n) := gkzSetup req;
    (w, ind, hrank, roots) := indicialData(req, I, W, n);
    k := parseInteger req#"order";
    maxTerms := parseInteger req#"max_terms";
    L := gens kernel A;
    if numcols L != n - rank A then fail "unexpected kernel basis";
    toric := apply(flatten entries gens toricIdealPartials(A, W), g -> sub(g, W));
    sameIdeal := I == ideal(toric | apply(eulerOperators(A, beta, W), g -> sub(g, W)));
    -- Directions for the exponents of multiplicity > 1.
    J := select(n, j -> any(roots, r -> r#0 > 1 and denominator(r#1#j) == 1));
    directions := if numcols L == 0 or not any(roots, r -> r#0 > 1) then {}
        else perturbationDirections(L, J, 24);
    seriesJSON := r -> (
        if r#0 > 1 then (
            (sols, reasons) := logBasis(r#1, L, directions, w, k, r#0, maxTerms);
            return "{\"status\":\"logarithmic\",\"solutions\":["
                | demark(",", apply(sols, solutionJSON)) | "],\"missing\":" | toString(r#0 - #sols)
                | ",\"independence\":" | independenceJSON sols
                | ",\"reason\":" | toJSON demark("; ", reasons) | "}");
        (status, data) := gammaSeries(r#1, L, w, k, maxTerms);
        if status == "unsupported" then
            return "{\"status\":\"unsupported\",\"reason\":" | toJSON data | "}";
        "{\"status\":\"computed\",\"terms\":[" | demark(",", apply(data, termJSON)) | "]}"
    );
    indicialJSON(n, W, ind, hrank)
    | ",\"toric\":" | gensJSON toric
    | ",\"generated_by_toric_and_euler\":" | toJSON sameIdeal
    | ",\"exponents\":[" | demark(",", apply(roots, r ->
        "{\"multiplicity\":" | toString(r#0) | ",\"point\":" | rationalsJSON(r#1)
        | ",\"series\":" | seriesJSON r | "}"))
    | "],\"info\":" | systemInfo() | "}"
);

packageOperations#"gkz.generate" = gkzRequest;
packageOperations#"gkz.indicial" = indicialRequest;
packageOperations#"gkz.series" = seriesRequest;
