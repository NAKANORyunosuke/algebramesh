-- AlgebraMesh driver for Macaulay2.
--
--   M2 --script algebramesh.m2 request.json
--
-- The request is JSON written by the macaulay2 backend after validation:
--   {"operation": "polynomial.factor", "domain": "QQ"|"ZZ", "nvars": n,
--    "terms": [{"coefficient": "p/q", "exponents": [...]}, ...]}
--   {"operation": "matrix.rank" | "matrix.kernel" | "matrix.solve" |
--    "matrix.hermite_normal_form" | "matrix.smith_normal_form",
--    "domain": "ZZ"|"QQ", "matrix": [["p/q", ...], ...], "rhs": ["p/q", ...],
--    "certificate": true|false}
--   {"operation": "system.info"}
--   {"operation": "system.json_integers", "values": ["n", ...]}  (a self-check of
--    how integers are printed; the backend never sends it)
--   {"operation": "system.leading_terms", "nvars": n, "order": [item, ...],
--    "polynomials": [[{"coefficient": "p/q", "exponents": [...]}, ...], ...]}
--    (a self-check of the monomial orders; the backend never sends it)
--   {"operation": "ideal.groebner_basis", "domain": "QQ", "nvars": n, "order": [item, ...],
--    "generators": [[{"coefficient": "p/q", "exponents": [...]}, ...], ...],
--    "certificate": "representation"|"s_pairs"|"none"}
--   {"operation": "ideal.normal_form" | "ideal.membership", the keys of
--    ideal.groebner_basis, and "polynomial": [{"coefficient": "p/q", "exponents":
--    [...]}, ...]}
--   {"operation": "module.syzygies", "domain": "QQ", "nvars": n, "order": [item, ...],
--    "rows": m, "columns": [[{"row": i, "terms": [...]}, ...], ...]}
--   {"operation": "complex.free_resolution", "domain": "QQ", "nvars": n, "order": [...],
--    "generators": [[...], ...], "homogeneous": true|false, "certificate": "include"|"none"}
--   {"operation": "ideal.hilbert_series", "domain": "QQ", "nvars": n, "order": [...],
--    "generators": [[...], ...], "betti_table": true|false}
--
-- An order-dependent operation gives the monomial order of its ring as the items
-- of Macaulay2's MonomialOrder option, which the backend writes (order_items):
-- ["Weights", [w, ...]], ["Lex", k] or ["GRevLex", k]. orderedRing builds the
-- ring with them; Macaulay2's default order is never used (design §6, rule 2).
--
-- A matrix job is answered as {"value": ..., "info": ...}, the value in the form
-- of algebramesh-ops with every entry as a decimal string; algebramesh-verify
-- checks its certificate (ADR 0019). As checked in this image: smithNormalForm
-- gives D = P*A*Q but not always d_1 | d_2 | ..., so D is made canonical here;
-- LLLBases' hermite is column-style and sometimes leaves an entry unreduced, so
-- the row form H = U*A is built from it and reduced here; rank, inverse, solve
-- and kernel on ZZ matrices run Groebner bases (slow, and solve may answer a
-- non-solution), so they are done over QQ or through the Smith form; solve over
-- QQ crashes Macaulay2 1.24.11 when A and b are both zero, so b = 0 is answered
-- without it.
--
-- An ideal job is answered in the same way, {"value": {"basis": [...],
-- "certificate": {"basis_in_generators": C, "generators_in_basis": N}}, "info":
-- ...}: the reduced Groebner basis G, monic and by increasing leading monomial,
-- with G = F C and F = G N, which algebramesh-verify checks (Issue #66). A normal
-- form is answered as {"basis": G, "remainder": r, "certificate":
-- {"basis_in_generators": C, "generators_in_basis": N, "quotients": [q_1, ...,
-- q_t]}} with f - r = sum_k q_k g_k, and a membership as {"member": true,
-- "certificate": {"coefficients": [c_1, ..., c_s]}} with f = sum_i c_i f_i, or
-- {"member": false, "certificate": {"basis": G, "generators_in_basis": N,
-- "quotients": [...], "remainder": r}} with r != 0 (Issue #67); no certificate
-- when the job asks for none. When the job asks for "s_pairs" (Issue #68), each
-- of these certificates but that of a member also holds "s_pairs": {"pairs":
-- [[i, j], ...], "representations": H}: the pairs i < j of elements of G whose
-- leading monomials are not coprime, and H (t x P) with S(g_i, g_j) = sum_k g_k
-- H_kp and lm(H_kp g_k) < lcm(lm(g_i), lm(g_j)), from which algebramesh-verify
-- checks that G is a Groebner basis. Matrices of polynomials are compared with
-- == here only against 0: == on two matrices also compares their degrees.
--
-- A module job (Issues #97, #98) is answered in the same way: {"syzygies": S}
-- with F S = 0; {"differentials": [d_1, ...], "degrees": [[0], ...],
-- "certificate": {"differential_in_generators": C, "generators_in_differential":
-- N}} with d_1 = F C and F = d_1 N, the degrees only for homogeneous generators
-- and no certificate when the job asks for none; {"numerator": ["k_0", ...],
-- "denominator_exponent": n, "betti": [[i, j, beta], ...]}, the Betti numbers
-- only when the job asks for them. algebramesh-verify checks them (ADR 0027).
--
-- Variable names never reach Macaulay2: polynomials use the indexed variables
-- t_0..t_(n-1) and are exchanged as exponent vectors. Coefficients are strings
-- checked against a strict pattern before conversion.
--
-- The operations of a package are in a file of their own next to this one
-- (gkz.m2, Issue #100; topology.m2, Issue #103), loaded when it is there: it
-- registers its operations in packageOperations, and its header lists their
-- requests.
--
-- The answer is one line on stdout:
--   ALGEBRAMESH_RESULT {json}   or   ALGEBRAMESH_ERROR message
-- Everything else on stdout or stderr is log output, never data.

needsPackage "JSON";
needsPackage "HolonomicSystems";
needsPackage "Polyhedra";
needsPackage "LLLBases";

fail = msg -> (
    << "ALGEBRAMESH_ERROR " << msg << endl;
    exit 1;
);

rationalPattern = "^-?[0-9]+(/[0-9]+)?$";

parseRational = s -> (
    if not instance(s, String) or not match(rationalPattern, s) then
        fail("invalid rational number " | toExternalString s);
    q := value s;
    q + 0/1
);

parseInteger = v -> (
    if instance(v, ZZ) then v
    else if instance(v, String) and match("^-?[0-9]+$", v) then value v
    else fail("invalid integer " | toExternalString v)
);

jsonString = s -> toJSON toString s;

-- An integer printed as a JSON number when it fits in 64 bits, as a decimal
-- string beyond (Issue #60): JSON readers take larger numbers as floating point.
-- toJSON prints integers of any size as bare digits, so it is not used on them.
intJSON = z -> if z >= -2^63 and z < 2^63 then toString z else jsonString z;
intsJSON = L -> "[" | demark(",", apply(L, intJSON)) | "]";

-- Terms of an element as JSON, with exponent vectors in ring variable order.
termsJSON = f -> (
    "[" | demark(",", apply(listForm f, t ->
        "{\"coefficient\":" | jsonString(t#1) | ",\"exponents\":" | intsJSON(t#0) | "}")) | "]"
);

systemInfo = () -> (
    "{\"macaulay2_version\":" | jsonString(version#"VERSION")
    | ",\"holonomic_systems_version\":" | jsonString((options HolonomicSystems).Version) | "}"
);

factorRequest = req -> (
    n := req#"nvars";
    if not instance(n, ZZ) or n < 0 then fail "invalid nvars";
    domain := req#"domain";
    K := if domain === "QQ" then QQ else if domain === "ZZ" then ZZ else fail "invalid domain";
    t := getSymbol "t";
    R := K(monoid[t_0..t_(max(n,1)-1)]);
    f := sum(req#"terms", term -> (
        e := term#"exponents";
        if not instance(e, List) or #e != n or any(e, k -> not instance(k, ZZ) or k < 0) then
            fail "invalid exponent vector";
        c := parseRational(term#"coefficient");
        if K === ZZ and denominator c != 1 then fail "coefficient not in ZZ";
        c * (if n == 0 then 1_R else product(n, i -> R_i^(e#i)))
    ));
    if f == 0 then fail "cannot factor the zero polynomial";
    F := factor f;
    "{\"factors\":[" | demark(",", apply(toList F, p ->
        "{\"terms\":" | termsJSON(sub(p#0, R)) | ",\"multiplicity\":" | intJSON(p#1) | "}"))
    | "],\"info\":" | systemInfo() | "}"
);

-- ------------------------------------------------------------- monomial orders

-- The ring QQ[t_0..t_(m-1)], m = max(n, 1), with the monomial order given by the
-- items of the request (Issue #64), laid out as MonomialOrder => {Weights => {w,
-- ...}, Lex => k, GRevLex => k}: Macaulay2 compares by the items in turn. Every
-- order-dependent operation builds its ring here. A ring without variables keeps
-- the driver's own variable (Issue #35), and its items cover that one.
-- As checked in Macaulay2 1.24.11: GLex is refused as an item of the list, so a
-- grlex block of k variables comes as Weights => {1, ..., 1}, Lex => k; a Weights
-- item weighs the variables from the first one not yet in a block on, and those
-- past its end by 0; GRevLex => k uses the degrees of the variables, all 1 here.
-- Macaulay2 accepts blocks that cover fewer variables than the ring, so the
-- cover is checked here.
orderedRing = (n, items) -> (
    if not instance(n, ZZ) or n < 0 then fail "invalid nvars";
    m := max(n, 1);
    if not instance(items, List) or #items == 0 then fail "invalid order";
    covered := 0;
    monomialOrder := apply(items, item -> (
        if not instance(item, List) or #item != 2 then fail("invalid order item " | toExternalString item);
        (itemName, itemArg) := toSequence item;
        if itemName === "Weights" then (
            if not instance(itemArg, List) or #itemArg > m - covered
                or any(itemArg, w -> not instance(w, ZZ) or w < 0) then
                fail("invalid order item " | toExternalString item);
            Weights => itemArg)
        else if itemName === "Lex" or itemName === "GRevLex" then (
            if not instance(itemArg, ZZ) or itemArg < 1 or covered + itemArg > m then
                fail "the order items do not cover the variables";
            covered = covered + itemArg;
            (if itemName === "Lex" then Lex else GRevLex) => itemArg)
        else fail("invalid order item " | toExternalString item)));
    if covered != m then fail "the order items do not cover the variables";
    t := getSymbol "t";
    QQ(monoid[t_0..t_(m-1), MonomialOrder => monomialOrder])
);

-- The polynomial of R with the given terms, each with n exponents.
parsePolynomial = (R, n, terms) -> (
    if not instance(terms, List) then fail "invalid polynomial";
    sum(terms, term -> (
        e := term#"exponents";
        if not instance(e, List) or #e != n or any(e, k -> not instance(k, ZZ) or k < 0) then
            fail "invalid exponent vector";
        parseRational(term#"coefficient") * (if n == 0 then 1_R else product(n, i -> R_i^(e#i)))
    )) + 0_R
);

-- The exponent vector of the leading term of each polynomial in the order of
-- the request, null for zero: a self-check of orderedRing against the Rust
-- comparison (tests/integration/check-driver-orders.sh). leadMonomial follows
-- the whole order, Weights items included.
leadingTermsRequest = req -> (
    n := req#"nvars";
    R := orderedRing(n, req#"order");
    if not instance(req#"polynomials", List) then fail "invalid polynomials";
    "{\"leading_exponents\":[" | demark(",", apply(req#"polynomials", terms -> (
        f := parsePolynomial(R, n, terms);
        if f == 0 then "null" else intsJSON take(first exponents leadMonomial f, n)))) | "]}"
);

gensJSON = L -> "[" | demark(",", apply(L, termsJSON)) | "]";
rationalsJSON = L -> toJSON apply(L, toString);

-- ------------------------------------------------------------- packages

-- The operations of the packages next to this file, by name: each package file
-- adds its own (Issue #100).
packageOperations = new MutableHashTable;
if fileExists(currentFileDirectory | "gkz.m2") then load(currentFileDirectory | "gkz.m2");
if fileExists(currentFileDirectory | "topology.m2") then load(currentFileDirectory | "topology.m2");

-- ------------------------------------------------------------- matrices

-- The rows of a matrix as decimal strings (n x 0 gives n empty rows).
matrixJSON = M -> "[" | demark(",", apply(entries M, r -> "[" | demark(",", apply(r, jsonString)) | "]")) | "]";
vectorJSON = M -> "[" | demark(",", apply(flatten entries M, jsonString)) | "]";

parseMatrix = (rows, K) -> (
    if not instance(rows, List) or #rows == 0 then fail "the matrix has no rows";
    if not instance(rows#0, List) or #(rows#0) == 0 then fail "the matrix has no columns";
    n := #(rows#0);
    if any(rows, r -> not instance(r, List) or #r != n) then fail "the rows of the matrix differ in length";
    E := apply(rows, r -> apply(r, parseRational));
    if K === ZZ then (
        if any(flatten E, q -> denominator q != 1) then fail "an entry is not in ZZ";
        map(ZZ^#rows, ZZ^n, apply(E, r -> apply(r, numerator))))
    else map(QQ^#rows, QQ^n, E)
);

-- The inverse of a unimodular matrix, through QQ: inverse on ZZ runs a Groebner
-- basis. lift fails if the inverse is not integral.
unimodularInverse = U -> lift(inverse(U ** QQ), ZZ);
transformJSON = U -> "{\"transform\":" | matrixJSON U | ",\"inverse\":" | matrixJSON unimodularInverse U | "}";

-- An invertible minor of the size of the rank (independent rows and columns,
-- from 0) with its inverse.
minorJSON = AQ -> (
    C := columnRankProfile mutableMatrix AQ;
    R := rowRankProfile mutableMatrix AQ;
    inv := if #R == 0 then "[]" else matrixJSON inverse submatrix(AQ, R, C);
    "{\"rows\":" | toJSON R | ",\"columns\":" | toJSON C | ",\"inverse\":" | inv | "}"
);

-- A basis of the right kernel over QQ in column echelon form read from the top:
-- the first nonzero entries of the columns go strictly down.
echelonKernel = AQ -> (
    n := numcols AQ;
    R := reducedRowEchelonForm AQ;
    E := entries R;
    piv := select(apply(E, row -> position(row, x -> x != 0)), p -> p =!= null);
    free := select(toList(0 .. n - 1), j -> not member(j, piv));
    if #free == 0 then return map(QQ^n, QQ^0, 0);
    K := transpose matrix apply(free, f -> apply(n, j ->
        if j == f then 1_QQ
        else if member(j, free) then 0_QQ
        else -(E#(position(piv, p -> p == j)))#f));
    transpose reducedRowEchelonForm transpose K
);

-- The Smith form D = P*A*Q with d_i >= 0 and d_1 | d_2 | ..., from the raw one.
canonicalSNF = A -> (
    (D0, P0, Q0) := smithNormalForm A;
    m := numrows A; n := numcols A; k := min(m, n);
    P := mutableMatrix P0; Q := mutableMatrix Q0;
    d := new MutableList from apply(k, i -> D0_(i,i));
    for i from 0 to k - 1 do if d#i < 0 then (
        d#i = -d#i; for c from 0 to m - 1 do P_(i,c) = -P_(i,c));
    for i from 0 to k - 1 do for j from i + 1 to k - 1 do (
        a := d#i; b := d#j;
        if (a == 0 and b != 0) or (a != 0 and b % a != 0) then (
            (g, s, t) := toSequence gcdCoefficients(a, b);
            ag := a // g; bg := b // g;
            for c from 0 to m - 1 do (
                u := P_(i,c); w := P_(j,c);
                P_(i,c) = s*u + t*w;
                P_(j,c) = -bg*u + ag*w);
            for r from 0 to n - 1 do (
                u := Q_(r,i); w := Q_(r,j);
                Q_(r,i) = u + w;
                Q_(r,j) = -t*bg*u + s*ag*w);
            d#i = g; d#j = a*bg));
    (apply(k, i -> d#i), matrix P, matrix Q)
);

-- The row Hermite normal form H = U*A: hermite of the reversed transpose, read
-- back, then positive pivots and entries above each pivot in [0, pivot).
reversal = k -> map(ZZ^k, ZZ^k, (i,j) -> if i + j == k - 1 then 1 else 0);
rowHNF = A -> (
    m := numrows A; n := numcols A;
    (Hc, C) := hermite(reversal n * transpose A * reversal m, ChangeMatrix => true);
    H := mutableMatrix(reversal m * transpose Hc * reversal n);
    U := mutableMatrix(reversal m * transpose C * reversal m);
    for i from 0 to m - 1 do (
        p := position(toList(0 .. n - 1), j -> H_(i,j) != 0);
        if p =!= null then (
            if H_(i,p) < 0 then (
                for j from 0 to n - 1 do H_(i,j) = -H_(i,j);
                for j from 0 to m - 1 do U_(i,j) = -U_(i,j));
            piv := H_(i,p);
            for k from 0 to i - 1 do (
                q := H_(k,p) // piv;
                if q != 0 then (
                    for j from 0 to n - 1 do H_(k,j) = H_(k,j) - q * H_(i,j);
                    for j from 0 to m - 1 do U_(k,j) = U_(k,j) - q * U_(i,j)))));
    (matrix H, matrix U)
);

-- A solution of A x = b, or null with y showing there is none: over QQ y A = 0
-- and y b = 1 (a vector of the left kernel); over ZZ, from D = P A Q, y A
-- integral and y b not.
solveMatrix = (A, b, K) -> (
    m := numrows A; n := numcols A;
    if K === QQ then (
        x := if b == 0 then map(QQ^n, QQ^1, 0) else solve(A, b);
        if x =!= null then return ("solution", x);
        Y := echelonKernel transpose A;
        j := position(toList(0 .. numcols Y - 1), j -> (transpose Y_{j} * b)_(0,0) != 0);
        if j === null then fail "no certificate found for a system without solution";
        y := transpose Y_{j};
        return ("no_solution", (1 / (y * b)_(0,0)) * y));
    k := min(m, n);
    (D, P, Q) := smithNormalForm A;
    c := P * b;
    d := i -> if i < k then D_(i,i) else 0;
    -- a zero d_i with c_i != 0 first: it shows there is no solution over QQ either
    i := position(toList(0 .. m - 1), i -> d i == 0 and c_(i,0) != 0);
    if i =!= null then return ("no_solution", (1 / (2 * c_(i,0))) * (P^{i} ** QQ));
    i = position(toList(0 .. m - 1), i -> d i != 0 and c_(i,0) % d i != 0);
    if i =!= null then return ("no_solution", (1 / d i) * (P^{i} ** QQ));
    z := map(ZZ^n, ZZ^1, apply(n, j -> {if j < k and D_(j,j) != 0 then c_(j,0) // D_(j,j) else 0}));
    ("solution", Q * z)
);

matrixRequest = req -> (
    op := req#"operation";
    K := if req#"domain" === "QQ" then QQ else if req#"domain" === "ZZ" then ZZ else fail "invalid domain";
    certificate := not req#?"certificate" or req#"certificate" === true;
    A := parseMatrix(req#"matrix", K);
    AQ := A ** QQ;
    m := numrows A; n := numcols A;
    withCertificate := (fields, cert) -> (
        if certificate then "{" | fields | ",\"certificate\":" | cert() | "}" else "{" | fields | "}");
    v := if op === "matrix.rank" then
        withCertificate("\"type\":\"matrix_rank\",\"rank\":" | intJSON rank AQ,
            () -> "{\"minor\":" | minorJSON AQ | ",\"kernel\":" | matrixJSON echelonKernel AQ | "}")
    else if op === "matrix.kernel" and K === QQ then
        withCertificate("\"type\":\"matrix_kernel\",\"basis\":" | matrixJSON echelonKernel AQ,
            () -> "{\"minor\":" | minorJSON AQ | "}")
    else if op === "matrix.kernel" then (
        -- U A^T = H in row Hermite form; its zero rows are the last n - r.
        (HT, UT) := rowHNF transpose A;
        nz := #select(entries HT, r -> any(r, x -> x != 0));
        withCertificate("\"type\":\"matrix_kernel\",\"basis\":" | matrixJSON transpose UT^(toList(nz .. n - 1)),
            () -> transformJSON UT))
    else if op === "matrix.solve" then (
        rhs := req#"rhs";
        if not instance(rhs, List) or #rhs != m then fail "the right-hand side has not one entry per row";
        bq := apply(rhs, parseRational);
        if K === ZZ and any(bq, q -> denominator q != 1) then fail "an entry of the right-hand side is not in ZZ";
        b := if K === ZZ then map(ZZ^m, ZZ^1, apply(bq, q -> {numerator q})) else map(QQ^m, QQ^1, apply(bq, q -> {q}));
        (kind, w) := solveMatrix(if K === ZZ then A else AQ, b, K);
        if kind === "solution" then "{\"type\":\"matrix_solution\",\"solution\":" | vectorJSON w | "}"
        else withCertificate("\"type\":\"matrix_solution\",\"solution\":null",
            () -> "{\"y\":" | vectorJSON w | "}"))
    else if op === "matrix.hermite_normal_form" then (
        if K =!= ZZ then fail "the Hermite normal form is over ZZ only";
        (H, U) := rowHNF A;
        withCertificate("\"type\":\"hermite_normal_form\",\"form\":" | matrixJSON H, () -> transformJSON U))
    else if op === "matrix.smith_normal_form" then (
        if K =!= ZZ then fail "the Smith normal form is over ZZ only";
        (d, P, Q) := canonicalSNF A;
        withCertificate("\"type\":\"smith_normal_form\",\"diagonal\":[" | demark(",", apply(d, jsonString)) | "]",
            () -> "{\"left\":" | transformJSON P | ",\"right\":" | transformJSON Q | "}"))
    else fail("unsupported operation " | toExternalString op);
    "{\"value\":" | v | ",\"info\":" | systemInfo() | "}"
);

-- ------------------------------------------------------------- ideals

-- A matrix of polynomials as {"rows": r, "columns": [[{"row": i, "terms": [...]},
-- ...], ...]}: each column by its nonzero entries, rows from 0.
polyMatrixJSON = M -> (
    "{\"rows\":" | toString numrows M | ",\"columns\":[" | demark(",", apply(numcols M, j ->
        "[" | demark(",", apply(select(numrows M, i -> M_(i,j) != 0), i ->
            "{\"row\":" | toString i | ",\"terms\":" | termsJSON(M_(i,j)) | "}")) | "]"))
    | "]}"
);

-- The job of an ideal operation: the ring R of n variables in the order of the
-- request, the generators fs and F = (f_1 ... f_s) as a 1 x s matrix, whether
-- the job asks for a certificate and whether for that of the S-pairs, and the
-- polynomial f of ideal.normal_form and ideal.membership (null for
-- ideal.groebner_basis).
idealSetup = (req, withPolynomial) -> (
    known := {"operation", "domain", "nvars", "order", "generators", "certificate"}
        | if withPolynomial then {"polynomial"} else {};
    for k in keys req do if not member(k, known) then fail("unknown request key " | toExternalString k);
    if not all(known, k -> req#?k) then fail "incomplete request";
    if req#"domain" =!= "QQ" then fail "the ideal operations are over QQ only";
    n := req#"nvars";
    if not instance(n, ZZ) or n < 1 then fail "invalid nvars";
    mode := req#"certificate";
    if not member(mode, {"representation", "s_pairs", "none"}) then
        fail("invalid certificate " | toExternalString mode);
    R := orderedRing(n, req#"order");
    gs := req#"generators";
    if not instance(gs, List) then fail "invalid generators";
    fs := apply(gs, g -> parsePolynomial(R, n, g));
    f := if withPolynomial then parsePolynomial(R, n, req#"polynomial") else null;
    (R, n, fs, map(R^1, R^(#fs), {fs}), mode =!= "none", mode === "s_pairs", f)
);

-- The reduced Groebner basis G of the ideal of F in the order of R (Issue #66)
-- as a 1 x t matrix. As checked in Macaulay2 1.24.11: gens gb over QQ is
-- reduced but not monic, so each element is divided by its leading coefficient
-- (leadCoefficient follows the whole order of the ring, Weights items
-- included), and G is sorted by increasing leading monomial with sort(...,
-- DegreeOrder => null, MonomialOrder => Ascending).
reducedBasis = (R, F) -> (
    G0 := select(flatten entries gens gb F, g -> g != 0);
    if #G0 == 0 then map(R^1, R^0, 0) else
        sort(matrix{apply(G0, g -> (1 / leadCoefficient g) * g)}, DegreeOrder => null, MonomialOrder => Ascending)
);

-- The quotients of the 1 x k matrix M by the generators fs, as an s x k matrix
-- Q with M = F Q, the rows of the zero generators 0; M must lie in the ideal of
-- F, which has a nonzero generator. M is lifted to F in a grevlex copy of R: a
-- Groebner basis computed with ChangeMatrix => true gives, by division, the
-- quotients by the generators themselves. Any quotients will do, and for lex
-- and block orders this is faster and far smaller than gb(F, ChangeMatrix =>
-- true) in the order of the request.
liftToGenerators = (R, n, fs, F, M) -> (
    S := newRing(R, MonomialOrder => {GRevLex => n});
    toS := map(S, R, gens S);
    toR := map(R, S, gens R);
    FS := gb(toS F, ChangeMatrix => true);
    if (toS M) % FS != 0 then fail "internal error: a polynomial does not lift to the generators";
    diagonalMatrix(R, apply(fs, f -> if f == 0 then 0 else 1)) * toR((toS M) // FS)
);

-- N (t x s) with F = G N: F divided by G (H = forceGB G), the remainder 0.
generatorsInBasis = (R, F, G, H) -> (
    if numcols G == 0 then return map(R^0, R^(numcols F), 0);
    if F % H != 0 then fail "internal error: a generator does not reduce to 0 by the basis";
    F // H
);

-- The certificate of the basis G (1 x t) of ideal.groebner_basis and
-- ideal.normal_form: C (s x t) with G = F C, and N.
basisCertificateJSON = (R, n, fs, F, G, H) -> (
    C := if numcols G == 0 then map(R^(#fs), R^0, 0) else liftToGenerators(R, n, fs, F, G);
    "\"basis_in_generators\":" | polyMatrixJSON C
    | ",\"generators_in_basis\":" | polyMatrixJSON generatorsInBasis(R, F, G, H)
);

-- The remainder r and the quotients q_1, ..., q_t of f by G (H = forceGB G):
-- f % H and f // H, with f = sum_k q_k g_k + r. As checked in Macaulay2
-- 1.24.11, in lex, grevlex and a weight order, on random polynomials and on the
-- twisted cubic: % reduces every term of f, not only the leading one, so that
-- no term of r is divisible by a leading monomial of G; f // H is t x 1. For the
-- zero ideal (t = 0) r = f and there are no quotients; for the unit ideal (G =
-- (1)) r = 0 and q_1 = f.
reduceByBasis = (R, f, H) -> (
    fm := map(R^1, R^1, {{f}});
    ((fm % H)_(0,0), flatten entries(fm // H))
);

basisJSON = G -> gensJSON flatten entries G;

-- The certificate of the S-pairs of the basis G (1 x t, monic, H = forceGB G)
-- (Issue #68): the pairs (i, j), i < j, from 0, whose leading monomials are not
-- coprime (some variable has a positive exponent in both), in increasing
-- lexicographic order of (i, j), as algebramesh-verify computes them; and H (t
-- x P) whose column p holds the quotients h of S(g_i, g_j) = (L / lm(g_i)) g_i
-- - (L / lm(g_j)) g_j, L = lcm(lm(g_i), lm(g_j)), by G: S // H, with S % H ==
-- 0 (else G is not a Groebner basis). As checked in Macaulay2 1.24.11 on the
-- recorded cases, cyclic 4 in grevlex and lex, and 417 random ideals in four
-- variables in lex, grevlex, weight and block orders (6936 pairs): S // H is a
-- standard representation, lm(h_k g_k) <= lm(S) for every nonzero h_k, hence
-- lm(h_k g_k) < L since lm(S) < L; algebramesh-verify checks this inequality
-- and S = sum_k g_k h_k, and refuses the value otherwise.
sPairsJSON = (R, n, G, H) -> (
    gs := flatten entries G;
    t := #gs;
    lms := apply(gs, g -> take(first exponents leadMonomial g, n));
    pairs := select(flatten apply(t, i -> apply(toList(i + 1 .. t - 1), j -> (i, j))),
        p -> any(n, v -> lms#(p#0)#v > 0 and lms#(p#1)#v > 0));
    Q := if #pairs == 0 then map(R^t, R^0, 0) else (
        S := map(R^1, R^(#pairs), {apply(pairs, p -> (
            (i, j) := p;
            L := lcm(leadMonomial gs#i, leadMonomial gs#j);
            (L // leadMonomial gs#i) * gs#i - (L // leadMonomial gs#j) * gs#j))});
        if S % H != 0 then fail "internal error: an S-pair does not reduce to 0 by the basis";
        S // H);
    "\"s_pairs\":{\"pairs\":[" | demark(",", apply(pairs, p -> "[" | toString p#0 | "," | toString p#1 | "]"))
    | "],\"representations\":" | polyMatrixJSON Q | "}"
);

-- The certificate of the S-pairs after a comma when the job asks for it, else
-- nothing, with the time it took.
sPairsPart = (withSPairs, R, n, G, H) -> (
    t0 := cpuTime();
    part := if withSPairs then "," | sPairsJSON(R, n, G, H) else "";
    (part, cpuTime() - t0)
);

-- The value of ideal.groebner_basis (Issue #66): the reduced Groebner basis G,
-- and unless the job asks for no certificate the matrices C (s x t) with G = F C
-- and N (t x s) with F = G N, and the certificate of the S-pairs of G when the
-- job asks for it (Issue #68). The products are not formed here,
-- algebramesh-verify checks them (ADR 0019).
groebnerRequest = req -> (
    (R, n, fs, F, withCertificate, withSPairs, f) := idealSetup(req, false);
    t0 := cpuTime();
    G := reducedBasis(R, F);
    H := forceGB G;
    t1 := cpuTime();
    (sPairs, sPairsTime) := sPairsPart(withSPairs, R, n, G, H);
    certificate := if withCertificate then
        ",\"certificate\":{" | basisCertificateJSON(R, n, fs, F, G, H) | sPairs | "}" else "";
    t2 := cpuTime();
    stderr << "ideal.groebner_basis: " << numcols G << " elements; basis " << t1 - t0
        << " s, certificate " << t2 - t1 << " s, of which S-pairs " << sPairsTime << " s" << endl;
    "{\"value\":{\"basis\":" | basisJSON G | certificate | "},\"info\":" | systemInfo() | "}"
);

-- The value of ideal.normal_form (Issue #67): the reduced Groebner basis G, the
-- remainder r of f by G, and unless the job asks for no certificate C and N as
-- for ideal.groebner_basis and the quotients q_1, ..., q_t with f - r = sum_k
-- q_k g_k, and the certificate of the S-pairs of G when the job asks for it
-- (Issue #68). The products are not formed here, algebramesh-verify checks them.
normalFormRequest = req -> (
    (R, n, fs, F, withCertificate, withSPairs, f) := idealSetup(req, true);
    t0 := cpuTime();
    G := reducedBasis(R, F);
    H := forceGB G;
    (r, q) := reduceByBasis(R, f, H);
    t1 := cpuTime();
    (sPairs, sPairsTime) := sPairsPart(withSPairs, R, n, G, H);
    certificate := if withCertificate then
        ",\"certificate\":{" | basisCertificateJSON(R, n, fs, F, G, H)
        | ",\"quotients\":" | gensJSON q | sPairs | "}" else "";
    t2 := cpuTime();
    stderr << "ideal.normal_form: " << numcols G << " elements; basis and remainder " << t1 - t0
        << " s, certificate " << t2 - t1 << " s, of which S-pairs " << sPairsTime << " s" << endl;
    "{\"value\":{\"basis\":" | basisJSON G | ",\"remainder\":" | termsJSON r | certificate
    | "},\"info\":" | systemInfo() | "}"
);

-- The value of ideal.membership (Issue #67): whether f lies in the ideal I of F,
-- which holds iff the remainder r of f by the reduced Groebner basis G is 0, and
-- unless the job asks for no certificate: for f in I the coefficients c_1, ...,
-- c_s with f = sum_i c_i f_i, lifted as C is (the zero ideal has only f = 0,
-- with c = 0); for f not in I the basis G, N with F = G N, and the quotients q
-- and the remainder r of f by G, r != 0 (no C: algebramesh-verify does not need
-- it for f not in I), and the certificate of the S-pairs of G when the job asks
-- for it (Issue #68; not for f in I, whose coefficients need no basis).
membershipRequest = req -> (
    (R, n, fs, F, withCertificate, withSPairs, f) := idealSetup(req, true);
    t0 := cpuTime();
    G := reducedBasis(R, F);
    H := forceGB G;
    (r, q) := reduceByBasis(R, f, H);
    member := r == 0;
    t1 := cpuTime();
    (sPairs, sPairsTime) := sPairsPart(withSPairs and not member, R, n, G, H);
    certificate := if not withCertificate then ""
    else if member then (
        c := if numcols G == 0 then toList(#fs : 0_R)
            else flatten entries liftToGenerators(R, n, fs, F, map(R^1, R^1, {{f}}));
        ",\"certificate\":{\"coefficients\":" | gensJSON c | "}")
    else (
        ",\"certificate\":{\"basis\":" | basisJSON G
        | ",\"generators_in_basis\":" | polyMatrixJSON generatorsInBasis(R, F, G, H)
        | ",\"quotients\":" | gensJSON q
        | ",\"remainder\":" | termsJSON r | sPairs | "}");
    t2 := cpuTime();
    stderr << "ideal.membership: " << toString member << "; basis and remainder " << t1 - t0
        << " s, certificate " << t2 - t1 << " s, of which S-pairs " << sPairsTime << " s" << endl;
    "{\"value\":{\"member\":" | toJSON member | certificate | "},\"info\":" | systemInfo() | "}"
);

-- ------------------------------------------------------------- modules

-- The ring and the request of a module job (Issues #97, #98): the ring R of n
-- variables in the order of the request (no operation of modules depends on
-- it, but the ring is built in the same way as for ideals), after checking
-- that the request has exactly the keys `known` and is over QQ.
moduleRing = (req, known) -> (
    known = {"operation", "domain", "nvars", "order"} | known;
    for k in keys req do if not member(k, known) then fail("unknown request key " | toExternalString k);
    if not all(known, k -> req#?k) then fail "incomplete request";
    if req#"domain" =!= "QQ" then fail "the module operations are over QQ only";
    n := req#"nvars";
    if not instance(n, ZZ) or n < 1 then fail "invalid nvars";
    (orderedRing(n, req#"order"), n)
);

-- M (rows x s) with M = A Q (A is 1 x k, M is 1 x s and in the image of A):
-- the quotients by A in a grevlex copy of R, as liftToGenerators does for the
-- generators of an ideal; the columns of M that are 0 have quotients 0.
liftToRow = (R, n, A, M) -> (
    if numcols A == 0 then return map(R^0, R^(numcols M), 0);
    S := newRing(R, MonomialOrder => {GRevLex => n});
    toS := map(S, R, gens S);
    toR := map(R, S, gens R);
    AS := gb(toS A, ChangeMatrix => true);
    if (toS M) % AS != 0 then fail "internal error: a polynomial does not lift to the differential";
    toR((toS M) // AS)
);

-- module.syzygies (Issue #97): generators S (s x r) of the syzygies of the
-- columns of the m x s matrix F, F S = 0, from syz. The request gives F as
-- "rows" and "columns" of entries {"row": i, "terms": [...]}. As checked in
-- Macaulay2 1.24.11: syz takes matrices that are not homogeneous, and a zero
-- column of F gives the unit vector at its place among the syzygies.
syzygiesRequest = req -> (
    (R, n) := moduleRing(req, {"rows", "columns"});
    m := req#"rows";
    if not instance(m, ZZ) or m < 0 then fail "invalid rows";
    columns := req#"columns";
    if not instance(columns, List) then fail "invalid columns";
    s := #columns;
    entryList := flatten apply(s, j -> (
        if not instance(columns#j, List) then fail "invalid column";
        apply(columns#j, e -> (
            i := e#"row";
            if not instance(i, ZZ) or i < 0 or i >= m then fail "invalid row";
            (i, j) => parsePolynomial(R, n, e#"terms")))));
    F := map(R^m, R^s, entryList);
    t0 := cpuTime();
    S := syz F;
    stderr << "module.syzygies: " << numcols S << " syzygies in " << cpuTime() - t0 << " s" << endl;
    "{\"value\":{\"syzygies\":" | polyMatrixJSON S | "},\"info\":" | systemInfo() | "}"
);

-- The ideal of a request without its zero generators, and the generators.
nonzeroIdeal = (R, n, req) -> (
    gs := req#"generators";
    if not instance(gs, List) then fail "invalid generators";
    fs := apply(gs, g -> parsePolynomial(R, n, g));
    (fs, select(fs, f -> f != 0))
);

-- The degrees of the basis of each free module F_0, ..., F_k of C, for the
-- standard grading of R: degrees C_i gives one list {d} for each basis element.
resolutionDegrees = C -> apply(length C + 1, i -> apply(degrees C_i, first));

-- complex.free_resolution (Issue #97): a free resolution of R/I with res, its
-- differentials d_1, ..., d_k (C.dd_i, 1 x n_1 for d_1), for homogeneous
-- generators the degrees of the basis of each F_i, and unless the job asks for
-- none the certificate C (s x n_1) with d_1 = F C and N (n_1 x s) with F = d_1
-- N. The zero generators are left out before res and come back as zero rows of
-- C and zero columns of N. As checked in Macaulay2 1.24.11: res of an ideal
-- resolves R/I; homogeneous generators give the minimal graded resolution;
-- other generators a resolution that need not be minimal, whose
-- differentials may have constant entries; the unit ideal gives R <- R with
-- d_1 = (1) and the zero ideal a resolution of length 0.
resolutionRequest = req -> (
    (R, n) := moduleRing(req, {"generators", "homogeneous", "certificate"});
    homogeneous := req#"homogeneous";
    if not instance(homogeneous, Boolean) then fail "invalid homogeneous";
    mode := req#"certificate";
    if not member(mode, {"include", "none"}) then fail("invalid certificate " | toExternalString mode);
    (fs, nonzero) := nonzeroIdeal(R, n, req);
    F := map(R^1, R^(#fs), {fs});
    t0 := cpuTime();
    (differentials, degreeLists) := if #nonzero == 0 then ({}, {{0}}) else (
        C := res ideal nonzero;
        (apply(length C, i -> C.dd_(i + 1)), resolutionDegrees C));
    t1 := cpuTime();
    d1 := if #differentials == 0 then map(R^1, R^0, 0) else differentials#0;
    certificate := if mode === "none" then "" else (
        ",\"certificate\":{\"differential_in_generators\":"
        | polyMatrixJSON(if #nonzero == 0 then map(R^(#fs), R^0, 0) else liftToGenerators(R, n, fs, F, d1))
        | ",\"generators_in_differential\":" | polyMatrixJSON liftToRow(R, n, d1, F) | "}");
    t2 := cpuTime();
    stderr << "complex.free_resolution: length " << #differentials << "; resolution " << t1 - t0
        << " s, certificate " << t2 - t1 << " s" << endl;
    "{\"value\":{\"differentials\":[" | demark(",", apply(differentials, polyMatrixJSON)) | "]"
    | (if homogeneous then ",\"degrees\":[" | demark(",", apply(degreeLists, intsJSON)) | "]" else "")
    | certificate | "},\"info\":" | systemInfo() | "}"
);

-- ideal.hilbert_series (Issue #98): the Hilbert series of R/I for homogeneous
-- generators, as the numerator K(t) over (1 - t)^n with K = poincare of R/I,
-- the numerator of hilbertSeries before it is reduced; the backend divides out
-- the common factors 1 - t. On request, the graded Betti numbers of R/I from
-- the degrees of the minimal resolution res. As checked in Macaulay2 1.24.11:
-- poincare comodule I is in ZZ[T] with T of degree 1, and is 0 for the unit
-- ideal and 1 for the zero ideal.
hilbertSeriesRequest = req -> (
    (R, n) := moduleRing(req, {"generators", "betti_table"});
    bettiTable := req#"betti_table";
    if not instance(bettiTable, Boolean) then fail "invalid betti_table";
    (fs, nonzero) := nonzeroIdeal(R, n, req);
    if not all(nonzero, isHomogeneous) then fail "the generators are not homogeneous";
    I := if #nonzero == 0 then ideal map(R^1, R^0, 0) else ideal nonzero;
    t0 := cpuTime();
    K := poincare comodule I;
    terms := listForm K;
    top := max(apply(terms, t -> first t#0) | {-1});
    numerator := apply(top + 1, j -> sum(select(terms, t -> first t#0 == j), t -> t#1));
    betti := if not bettiTable then "" else (
        degreeLists := if #nonzero == 0 then {{0}} else resolutionDegrees res I;
        ",\"betti\":[" | demark(",", flatten apply(#degreeLists, i -> (
            apply(sort unique degreeLists#i, j ->
                intsJSON {i, j, #select(degreeLists#i, d -> d == j)})))) | "]");
    stderr << "ideal.hilbert_series: " << cpuTime() - t0 << " s" << endl;
    "{\"value\":{\"numerator\":[" | demark(",", apply(numerator, c -> jsonString c)) | "]"
    | ",\"denominator_exponent\":" | toString n | betti | "},\"info\":" | systemInfo() | "}"
);

if #scriptCommandLine < 2 then fail "usage: M2 --script algebramesh.m2 request.json";
req := fromJSON get scriptCommandLine#1;
if not instance(req, HashTable) or not req#?"operation" then fail "invalid request";
op := req#"operation";
result := (
    if op === "polynomial.factor" then factorRequest req
    else if instance(op, String) and packageOperations#?op then packageOperations#op req
    else if op === "system.info" then systemInfo()
    else if op === "system.json_integers" then "{\"values\":" | intsJSON apply(req#"values", parseInteger) | "}"
    else if op === "system.leading_terms" then leadingTermsRequest req
    else if instance(op, String) and match("^matrix\\.", op) then matrixRequest req
    else if op === "ideal.groebner_basis" then groebnerRequest req
    else if op === "ideal.normal_form" then normalFormRequest req
    else if op === "ideal.membership" then membershipRequest req
    else if op === "module.syzygies" then syzygiesRequest req
    else if op === "complex.free_resolution" then resolutionRequest req
    else if op === "ideal.hilbert_series" then hilbertSeriesRequest req
    else fail("unsupported operation " | toExternalString op)
);
<< "ALGEBRAMESH_RESULT " << result << endl;
exit 0;
