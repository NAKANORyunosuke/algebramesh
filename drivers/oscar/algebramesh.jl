# AlgebraMesh driver for OSCAR (Julia).
#
#   julia --project=/opt/algebramesh-oscar algebramesh.jl
#
# Loading OSCAR takes tens of seconds, so one process serves many requests. After
# start-up it prints
#   ALGEBRAMESH_READY {"oscar_version": ..., "julia_version": ...}
# and then reads one JSON request per line on stdin and answers each with one line
#   ALGEBRAMESH_RESULT {json}   or   ALGEBRAMESH_ERROR message
# Any other output is log text, never data.
#
# Requests (built by the oscar backend after validation):
#   {"operation": "polynomial.factor", "domain": "QQ"|"ZZ", "nvars": n,
#    "terms": [{"coefficient": "p/q", "exponents": [...]}, ...]}
#   {"operation": "matrix.rank" | "matrix.kernel" | "matrix.solve" |
#    "matrix.hermite_normal_form" | "matrix.smith_normal_form",
#    "domain": "ZZ"|"QQ", "matrix": [["p/q", ...], ...], "rhs": ["p/q", ...],
#    "certificate": true|false}
#   {"operation": "system.info"}
#   {"operation": "system.json_integers", "values": ["n", ...]}  (a self-check of
#    how integers are printed; the backend never sends it)
#   {"operation": "system.leading_terms", "nvars": n, "order": [item, ...],
#    "polynomials": [[{"coefficient": "p/q", "exponents": [...]}, ...], ...]}
#    (a self-check of the monomial orders; the backend never sends it)
#   {"operation": "ideal.groebner_basis", "domain": "QQ", "nvars": n >= 1,
#    "order": [item, ...], "generators": [[term, ...], ...],
#    "certificate": "representation"|"s_pairs"|"none"}
#    answered with {"basis": [[term, ...], ...], "certificate":
#    {"basis_in_generators": C, "generators_in_basis": N}} (no "certificate"
#    for "none"), C and N as {"rows": r, "columns": [[{"row": i, "terms":
#    [...]}, ...], ...]}; for "s_pairs" (Issue #68) the certificate has one
#    more key, "s_pairs": {"pairs": [[i, j], ...], "representations": H}, H
#    written as C and N (see s_pairs_json)
#   {"operation": "ideal.normal_form" | "ideal.membership", the keys of
#    ideal.groebner_basis, "polynomial": [term, ...]}
#    answered for ideal.normal_form with {"basis": [...], "remainder": [term,
#    ...], "certificate": {"basis_in_generators": C, "generators_in_basis": N,
#    "quotients": [[term, ...], ...]}} and for ideal.membership with
#    {"member": true, "certificate": {"coefficients": [[term, ...], ...]}} or
#    {"member": false, "certificate": {"basis": [...], "generators_in_basis":
#    N, "quotients": [...], "remainder": [...]}} (no "certificate" for "none");
#    for "s_pairs", the certificate of ideal.normal_form and that of
#    ideal.membership for member false have "s_pairs" too, and that of a
#    member stays {"coefficients": [...]}
#   {"operation": "module.syzygies", "domain": "QQ", "nvars": n >= 1, "order":
#    [item, ...], "rows": m, "columns": [[{"row": i, "terms": [...]}, ...], ...]}
#    answered with {"syzygies": S}, S written as C and N
#   {"operation": "complex.free_resolution", "domain": "QQ", "nvars": n >= 1,
#    "order": [...], "generators": [[term, ...], ...], "homogeneous": bool,
#    "certificate": "include"|"none"} answered with {"differentials": [d_1,
#    ...], "degrees": [[0], ...], "certificate": {"differential_in_generators":
#    C, "generators_in_differential": N}} (the degrees only for homogeneous
#    generators, no "certificate" for "none")
#   {"operation": "ideal.hilbert_series", "domain": "QQ", "nvars": n >= 1,
#    "order": [...], "generators": [...], "betti_table": bool} answered with
#    {"numerator": ["k_0", ...], "denominator_exponent": d, "betti": [[i, j,
#    beta], ...]} (the Betti numbers only when asked for)
#
# Variable names never reach OSCAR: polynomials use t1..tn and exponent vectors.
#
# An order-dependent operation gives the monomial order of its ring as items the
# backend writes (order_items): ["weights", [w, ...]] and [name, first, last].
# ordered_ring builds the ring and the ordering from them, and the ordering is
# passed to every function that takes one: OSCAR's default ordering is never
# used (design §6, rule 2).
#
# The value of a matrix job is answered in the form of algebramesh-ops (types
# matrix_rank, matrix_kernel, matrix_solution, hermite_normal_form,
# smith_normal_form), with every entry as a decimal string; the backend reads it
# by that type and algebramesh-verify checks its certificate (ADR 0019).
# Conventions, as checked in this image: snf_with_transform gives D = U*A*V;
# hnf_with_transform gives the row form H = U*A; kernel and
# can_solve_with_solution are called with side = :right.

using Oscar
using JSON3

const RATIONAL = r"^-?[0-9]+(/[0-9]+)?$"

struct RequestError <: Exception
    msg::String
end
Base.showerror(io::IO, e::RequestError) = print(io, e.msg)

function parse_rational(s)
    (s isa AbstractString && occursin(RATIONAL, s)) || throw(RequestError("invalid rational number $(repr(s))"))
    parts = split(s, '/')
    num = parse(BigInt, parts[1])
    den = length(parts) == 2 ? parse(BigInt, parts[2]) : BigInt(1)
    den == 0 && throw(RequestError("zero denominator"))
    return QQ(num, den)
end

qstring(c) = replace(string(c), "//" => "/")

# An integer printed as a JSON number when it fits in 64 bits, as a decimal
# string beyond (Issue #60): JSON readers take larger numbers as floating
# point. Never hand Nemo or BigInt values to JSON3 directly.
json_integer(x) = (b = BigInt(x); typemin(Int64) <= b <= typemax(Int64) ? Int64(b) : string(b))

# The rows of a matrix as decimal strings.
matrix_json(M) = [[qstring(M[i, j]) for j in 1:ncols(M)] for i in 1:nrows(M)]

terms_json(p) = [Dict("coefficient" => qstring(c), "exponents" => json_integer.(collect(e)))
                 for (c, e) in zip(coefficients(p), exponents(p))]

system_info() = Dict(
    "oscar_version" => string(pkgversion(Oscar)),
    "julia_version" => string(VERSION),
)

function factor_request(req)
    n = req.nvars
    (n isa Integer && n >= 0) || throw(RequestError("invalid nvars"))
    domain = req.domain
    K = domain == "QQ" ? QQ : domain == "ZZ" ? ZZ : throw(RequestError("invalid domain"))
    R, t = polynomial_ring(K, max(n, 1); cached = false)
    f = zero(R)
    for term in req.terms
        e = term.exponents
        (length(e) == n && all(k -> k isa Integer && k >= 0, e)) || throw(RequestError("invalid exponent vector"))
        c = parse_rational(term.coefficient)
        if K === ZZ
            isone(denominator(c)) || throw(RequestError("coefficient not in ZZ"))
            c = numerator(c)
        end
        m = one(R)
        for i in 1:n
            m *= t[i]^e[i]
        end
        f += K(c) * m
    end
    iszero(f) && throw(RequestError("cannot factor the zero polynomial"))
    F = factor(f)
    factors = Any[]
    u = unit(F)
    isone(u) || push!(factors, Dict("terms" => terms_json(R(u)), "multiplicity" => 1))
    for (p, k) in F
        push!(factors, Dict("terms" => terms_json(p), "multiplicity" => json_integer(k)))
    end
    return Dict("factors" => factors, "info" => system_info())
end

# ------------------------------------------------------------- monomial orders

const BLOCK_ORDERINGS = Dict("lex" => lex, "deglex" => deglex, "degrevlex" => degrevlex)

# The ring QQ[t1..tm], m = max(n, 1), its variables and the monomial ordering
# given by the items of the request (Issue #64): [name, first, last] is the
# ordering name (lex, deglex or degrevlex) on t[first:last], and these blocks
# follow one another from t1 to tm; their product o compares by the first block
# first. A ["weights", [w_1, ..., w_m]] item, which comes first, makes the
# ordering weight_ordering(w, o): by the weighted degree, then by o. Every
# order-dependent operation builds its ring here. A ring without variables
# keeps the driver's own variable (Issue #35), and its items cover that one.
# As checked in OSCAR 1.8.2: lex, deglex and degrevlex are lex, grlex and
# grevlex with t1 > t2 > ... > tm.
function ordered_ring(n, items)
    (n isa Integer && n >= 0) || throw(RequestError("invalid nvars"))
    R, t = polynomial_ring(QQ, max(n, 1); cached = false)
    (items isa AbstractVector && !isempty(items)) || throw(RequestError("invalid order"))
    weights = nothing
    if items[1] isa AbstractVector && length(items[1]) == 2 && items[1][1] == "weights"
        w = items[1][2]
        (w isa AbstractVector && length(w) == length(t) && all(k -> k isa Integer && k >= 0, w)) ||
            throw(RequestError("invalid order item $(JSON3.write(items[1]))"))
        weights = Int[k for k in w]
        items = items[2:end]
    end
    blocks = MonomialOrdering[]
    next = 1
    for item in items
        (item isa AbstractVector && length(item) == 3 && haskey(BLOCK_ORDERINGS, item[1])) ||
            throw(RequestError("invalid order item $(JSON3.write(item))"))
        lo, hi = item[2], item[3]
        (lo == next && hi isa Integer && lo <= hi <= length(t)) ||
            throw(RequestError("the order items do not cover the variables"))
        push!(blocks, BLOCK_ORDERINGS[item[1]](t[lo:hi]))
        next = hi + 1
    end
    next == length(t) + 1 || throw(RequestError("the order items do not cover the variables"))
    o = prod(blocks)
    weights === nothing || (o = weight_ordering(weights, o))
    is_global(o) || throw(RequestError("the monomial ordering is not global"))
    return R, t, o
end

# The polynomial of R with the given terms, each with n exponents.
function parse_polynomial(R, n, terms)
    terms isa AbstractVector || throw(RequestError("invalid polynomial"))
    t = gens(R)
    f = zero(R)
    for term in terms
        e = term.exponents
        (e isa AbstractVector && length(e) == n && all(k -> k isa Integer && k >= 0, e)) ||
            throw(RequestError("invalid exponent vector"))
        m = one(R)
        for i in 1:n
            m *= t[i]^e[i]
        end
        f += parse_rational(term.coefficient) * m
    end
    return f
end

# The exponent vector of the leading term of each polynomial in the ordering of
# the request, nothing (null) for zero: a self-check of ordered_ring against the
# Rust comparison (tests/integration/check-driver-orders.sh).
function leading_terms_request(req)
    n = req.nvars
    R, _, o = ordered_ring(n, req.order)
    req.polynomials isa AbstractVector || throw(RequestError("invalid polynomials"))
    lead(f) = iszero(f) ? nothing :
        json_integer.(first(exponents(leading_monomial(f; ordering = o)))[1:n])
    return Dict("leading_exponents" => [lead(parse_polynomial(R, n, p)) for p in req.polynomials])
end

# ------------------------------------------------------------- ideals

const IDEAL_KEYS = Set([:operation, :domain, :nvars, :order, :generators, :certificate])
const IDEAL_POLYNOMIAL_KEYS = union(IDEAL_KEYS, Set([:polynomial]))

# A matrix of polynomials as the backend reads it: the number of rows, and each
# column as its nonzero entries by row (from 0).
poly_matrix_json(M) = Dict(
    "rows" => nrows(M),
    "columns" => [[Dict("row" => i - 1, "terms" => terms_json(M[i, j])) for i in 1:nrows(M) if !iszero(M[i, j])]
                  for j in 1:ncols(M)],
)

# C (s x t) with G = F C, F and G as rows: the elements P of the ideal of F
# lifted to F through its degrevlex basis D = F T, P = D Q^T. Any C with
# G = F C will do; for lex this is faster and far smaller than the
# transformation matrix in the declared order (measured in the design of #66).
# As checked in this image: groebner_basis_with_transformation_matrix gives T
# of s x |D| (a row for each generator, zero ones included) with F T = D, and
# reduce_with_quotients(P, D) gives Q of |P| x |D| with Q D = P as columns.
# The coefficients of a member f of the ideal (ideal.membership) are the one
# column of the result for P = [f].
function lift_to_generators(R, F, P)
    d = degrevlex(R)
    D, T = groebner_basis_with_transformation_matrix(ideal(R, F); ordering = d)
    (nrows(T) == length(F) && ncols(T) == length(D)) || throw(RequestError("the transformation matrix does not match the basis"))
    Q, r = reduce_with_quotients(P, collect(D); ordering = d)
    all(iszero, r) || throw(RequestError("an element of the basis does not lift to the generators"))
    return T * transpose(Q)
end

# The parts of an ideal request (Issues #66, #67, #68): the ring R, the
# ordering o, the generators F, the polynomial f of ideal.normal_form and
# ideal.membership (nothing for ideal.groebner_basis), whether the
# representation is asked for (by "representation" or "s_pairs"), whether the
# S-pairs are, and the reduced Groebner basis G of the ideal
# of F in o, without zero elements (OSCAR gives [0] for the zero ideal), each
# element monic (OSCAR's basis over QQ is not) and by strictly increasing
# leading monomial.
function ideal_parts(req)
    with_polynomial = req.operation != "ideal.groebner_basis"
    allowed = with_polynomial ? IDEAL_POLYNOMIAL_KEYS : IDEAL_KEYS
    for k in keys(req)
        k in allowed || throw(RequestError("unknown key $(repr(String(k)))"))
    end
    req.domain == "QQ" || throw(RequestError("the ideal operations are over QQ only"))
    n = req.nvars
    (n isa Integer && n >= 1) || throw(RequestError("invalid nvars"))
    mode = get(req, :certificate, nothing)
    mode in ("representation", "s_pairs", "none") || throw(RequestError("invalid certificate $(repr(mode))"))
    R, _, o = ordered_ring(n, req.order)
    req.generators isa AbstractVector || throw(RequestError("invalid generators"))
    F = [parse_polynomial(R, n, g) for g in req.generators]
    f = nothing
    if with_polynomial
        haskey(req, :polynomial) || throw(RequestError("the request has no polynomial"))
        f = parse_polynomial(R, n, req.polynomial)
    end
    lm(g) = leading_monomial(g; ordering = o)
    G = elem_type(R)[]
    if any(!iszero, F)
        G = filter(!iszero, collect(groebner_basis(ideal(R, F); ordering = o, complete_reduction = true)))
        sort!(G; lt = (a, b) -> cmp(o, lm(a), lm(b)) < 0)
        G = [g * inv(leading_coefficient(g; ordering = o)) for g in G]
    end
    return (R = R, o = o, F = F, f = f, G = G, representation = mode != "none", s_pairs = mode == "s_pairs")
end

# C (s x t) with G = F C.
basis_in_generators(R, F, G) = isempty(G) ? zero_matrix(R, length(F), 0) : lift_to_generators(R, F, G)

# N (t x s) with F = G N: the transpose of the quotients of F on division by
# G (one row of quotients for each element of F, as checked in this image),
# the remainders 0.
function generators_in_basis(R, o, F, G)
    isempty(G) && return zero_matrix(R, 0, length(F))
    Q, r = reduce_with_quotients(F, G; ordering = o, complete_reduction = true)
    all(iszero, r) || throw(RequestError("a generator does not reduce to 0 by the basis"))
    return transpose(Q)
end

# The S-pairs of G (Issue #68), G monic in o and in the order of the reply:
# the pairs (i, j), i < j, from 0, whose leading monomials in o are not coprime
# (some variable has a positive exponent in both), in increasing lexicographic
# order of (i, j), as the checker lists them; and H (t x P) whose column p
# holds h_1..h_t with S(g_i, g_j) = sum_k h_k g_k for the p-th pair (i, j),
# where S(g_i, g_j) = (L / lm(g_i)) g_i - (L / lm(g_j)) g_j and L is the lcm
# of lm(g_i) and lm(g_j). The h_k are the quotients of S on division by G,
# the remainders 0 (G is a Groebner basis). As checked in this image, for a
# vector of polynomials, reduce_with_quotients with or without
# complete_reduction gives remainder 0 and quotients with
# lm(h_k g_k) <= lm(S) < L, the strict inequality the checker requires, for
# the twisted cubic, cyclic-4 and 200 random ideals in lex, grlex, grevlex, a
# weight and a block ordering; without complete_reduction only leading terms
# are reduced, which is all S needs. A vector of zero polynomials only (as
# for a basis of monomials, where every S is 0) comes back with one row of
# quotients, so only the nonzero S are divided, and a zero S has a zero
# column. The checker checks all of it again.
function s_pairs_json(R, o, G)
    t = length(G)
    x = gens(R)
    E = [first(exponents(leading_monomial(g; ordering = o))) for g in G]
    monomial_of(e) = prod(x[k]^e[k] for k in eachindex(e); init = one(R))
    pairs = Tuple{Int,Int}[]
    S = elem_type(R)[]
    for i in 1:t, j in i+1:t
        any(k -> E[i][k] > 0 && E[j][k] > 0, eachindex(E[i])) || continue
        L = max.(E[i], E[j])
        push!(pairs, (i, j))
        push!(S, monomial_of(L - E[i]) * G[i] - monomial_of(L - E[j]) * G[j])
    end
    H = zero_matrix(R, t, length(pairs))
    nonzero = findall(!iszero, S)
    if !isempty(nonzero)
        Q, r = reduce_with_quotients(S[nonzero], G; ordering = o, complete_reduction = false)
        size(Q) == (length(nonzero), t) || throw(RequestError("the quotients of the S-pairs have the wrong shape"))
        all(iszero, r) || throw(RequestError("an S-pair does not reduce to 0 by the basis"))
        for (row, p) in enumerate(nonzero), k in 1:t
            H[k, p] = Q[row, k]
        end
    end
    return Dict("pairs" => [[i - 1, j - 1] for (i, j) in pairs], "representations" => poly_matrix_json(H))
end

# C and N as the reply gives them, after a check of their shapes, and the
# S-pairs when they are asked for.
function representation_json(R, o, F, G, with_s_pairs)
    s, t = length(F), length(G)
    C, N = basis_in_generators(R, F, G), generators_in_basis(R, o, F, G)
    (size(C) == (s, t) && size(N) == (t, s)) || throw(RequestError("the certificate has the wrong shape"))
    certificate = Dict{String,Any}("basis_in_generators" => poly_matrix_json(C), "generators_in_basis" => poly_matrix_json(N))
    with_s_pairs && (certificate["s_pairs"] = s_pairs_json(R, o, G))
    return certificate
end

# The quotients q (one for each element of G) and the remainder r of f on
# division by G with every term reduced: f = sum_k q_k g_k + r. As checked in
# this image, reduce_with_quotients(f, G; complete_reduction = true) for one
# polynomial f gives Q of 1 x t and r with f = Q[1, :] G + r, and no term of r
# is divisible by a leading monomial of G (the checker verifies both anyway).
function divide(f, G, o)
    isempty(G) && return (typeof(f)[], f)
    Q, r = reduce_with_quotients(f, G; ordering = o, complete_reduction = true)
    size(Q) == (1, length(G)) || throw(RequestError("the quotients have the wrong shape"))
    return ([Q[1, k] for k in 1:length(G)], r)
end

# ideal.groebner_basis (Issue #66): the reduced Groebner basis G with C
# (G = F C) and N (F = G N) when the request asks for the representation, and
# the S-pairs (Issue #68) when it asks for them. C, N and H are computed from
# G as returned, so they need no adjustment; the products are checked by the
# backend's checker, not here.
function groebner_request(req)
    p = ideal_parts(req)
    value = Dict{String,Any}("basis" => [terms_json(g) for g in p.G])
    p.representation && (value["certificate"] = representation_json(p.R, p.o, p.F, p.G, p.s_pairs))
    return value
end

# ideal.normal_form (Issue #67): the remainder r of f on division by G, the
# normal form of f modulo the ideal, with G and, for the representation, C, N
# and the quotients of f, and the S-pairs when they are asked for. For the
# zero ideal G is empty and r = f.
function normal_form_request(req)
    p = ideal_parts(req)
    q, r = divide(p.f, p.G, p.o)
    value = Dict{String,Any}("basis" => [terms_json(g) for g in p.G], "remainder" => terms_json(r))
    if p.representation
        certificate = representation_json(p.R, p.o, p.F, p.G, p.s_pairs)
        certificate["quotients"] = [terms_json(qk) for qk in q]
        value["certificate"] = certificate
    end
    return value
end

# ideal.membership (Issue #67): f lies in the ideal iff its remainder on
# division by G is 0. For the representation, a member comes with its
# coefficients c, f = sum_i c_i f_i (lifted as C is; all zero for f = 0), and
# a polynomial outside the ideal with G, N, its quotients and its remainder,
# and the S-pairs when they are asked for; no C, since f - r lies in the ideal
# of G, which contains the ideal of F. A member has no S-pairs: its
# coefficients show that f lies in the ideal without G.
function membership_request(req)
    p = ideal_parts(req)
    s = length(p.F)
    q, r = divide(p.f, p.G, p.o)
    member = iszero(r)
    value = Dict{String,Any}("member" => member)
    p.representation || return value
    if member && iszero(p.f)
        c = [zero(p.R) for _ in 1:s]
    elseif member
        L = lift_to_generators(p.R, p.F, [p.f])
        size(L) == (s, 1) || throw(RequestError("the coefficients have the wrong shape"))
        c = [L[i, 1] for i in 1:s]
    end
    if member
        value["certificate"] = Dict("coefficients" => [terms_json(ci) for ci in c])
    else
        N = generators_in_basis(p.R, p.o, p.F, p.G)
        size(N) == (length(p.G), s) || throw(RequestError("the certificate has the wrong shape"))
        certificate = Dict{String,Any}(
            "basis" => [terms_json(g) for g in p.G],
            "generators_in_basis" => poly_matrix_json(N),
            "quotients" => [terms_json(qk) for qk in q],
            "remainder" => terms_json(r),
        )
        p.s_pairs && (certificate["s_pairs"] = s_pairs_json(p.R, p.o, p.G))
        value["certificate"] = certificate
    end
    return value
end

# ------------------------------------------------------------- matrices

function parse_matrix(rows, K)
    (rows isa AbstractVector && !isempty(rows)) || throw(RequestError("the matrix has no rows"))
    n = length(rows[1])
    n > 0 || throw(RequestError("the matrix has no columns"))
    all(r -> length(r) == n, rows) || throw(RequestError("the rows of the matrix differ in length"))
    entries = [parse_rational(rows[i][j]) for i in 1:length(rows) for j in 1:n]
    if K === ZZ
        all(e -> isone(denominator(e)), entries) || throw(RequestError("an entry is not in ZZ"))
        return matrix(ZZ, length(rows), n, [numerator(e) for e in entries])
    end
    return matrix(QQ, length(rows), n, entries)
end

# The columns of a pivot in each nonzero row of a reduced row echelon form.
pivot_columns(R) = Int[findfirst(k -> !iszero(R[i, k]), 1:ncols(R)) for i in 1:nrows(R) if !iszero(R[i:i, :])]

# An invertible minor of the size of the rank (independent rows and columns)
# with its inverse; positions from 0.
function minor(AQ)
    _, R = rref(AQ)
    cols = pivot_columns(R)
    _, RT = rref(transpose(AQ))
    rows = pivot_columns(RT)
    B = AQ[rows, cols]
    return Dict("rows" => rows .- 1, "columns" => cols .- 1, "inverse" => matrix_json(inv(B)))
end

# A basis of the right kernel over QQ in column echelon form read from the top:
# the first nonzero entries of the columns go strictly down.
function echelon_kernel(AQ)
    K = kernel(AQ; side = :right)
    ncols(K) == 0 && return K
    _, R = rref(transpose(K))
    return transpose(R)
end

transform_json(U) = Dict("transform" => matrix_json(U), "inverse" => matrix_json(inv(U)))

# A rational y showing that A x = b has no solution: over QQ, y A = 0 and
# y b != 0 (a row of the left kernel); over ZZ, y A integral and y b not, from
# D = U A V: first a zero d_i with c_i = (U b)_i != 0 (no solution over QQ
# either), then a d_i not dividing c_i.
function no_solution(A, AQ, b, K)
    m, n = nrows(A), ncols(A)
    if K === QQ
        L = kernel(AQ)
        t = L * matrix(QQ, m, 1, b)
        i = findfirst(k -> !iszero(t[k, 1]), 1:nrows(t))
        i === nothing && throw(RequestError("no certificate found for a system without solution"))
        return [qstring(L[i, j]) for j in 1:m]
    end
    D, U, _ = snf_with_transform(A)
    c = U * matrix(ZZ, m, 1, [numerator(x) for x in b])
    d(i) = i <= min(m, n) ? D[i, i] : ZZ(0)
    for i in 1:m
        if iszero(d(i)) && !iszero(c[i, 1])
            return [qstring(QQ(U[i, j]) // (2 * c[i, 1])) for j in 1:m]
        end
    end
    for i in 1:m
        if !iszero(d(i)) && !is_divisible_by(c[i, 1], d(i))
            return [qstring(QQ(U[i, j]) // d(i)) for j in 1:m]
        end
    end
    throw(RequestError("no certificate found for a system without solution"))
end

function matrix_request(req)
    op = req.operation
    K = req.domain == "QQ" ? QQ : req.domain == "ZZ" ? ZZ : throw(RequestError("invalid domain"))
    certificate = get(req, :certificate, true) === true
    A = parse_matrix(req.matrix, K)
    AQ = K === QQ ? A : change_base_ring(QQ, A)
    m, n = nrows(A), ncols(A)
    if op == "matrix.rank"
        value = Dict{String,Any}("type" => "matrix_rank", "rank" => json_integer(rank(AQ)))
        if certificate
            value["certificate"] = Dict("minor" => minor(AQ), "kernel" => matrix_json(echelon_kernel(AQ)))
        end
        return value
    elseif op == "matrix.kernel"
        if K === QQ
            value = Dict{String,Any}("type" => "matrix_kernel", "basis" => matrix_json(echelon_kernel(AQ)))
            certificate && (value["certificate"] = Dict("minor" => minor(AQ)))
            return value
        end
        # U A^T = H in row Hermite form; its zero rows are the last n - r.
        H, U = hnf_with_transform(transpose(A))
        r = rank(A)
        value = Dict{String,Any}("type" => "matrix_kernel", "basis" => matrix_json(transpose(U[r+1:n, :])))
        certificate && (value["certificate"] = transform_json(U))
        return value
    elseif op == "matrix.solve"
        (req.rhs isa AbstractVector && length(req.rhs) == m) || throw(RequestError("the right-hand side has not one entry per row"))
        b = [parse_rational(x) for x in req.rhs]
        if K === ZZ
            all(x -> isone(denominator(x)), b) || throw(RequestError("an entry of the right-hand side is not in ZZ"))
            ok, x = can_solve_with_solution(A, [numerator(v) for v in b]; side = :right)
        else
            ok, x = can_solve_with_solution(AQ, b; side = :right)
        end
        # x means nothing when there is no solution.
        ok && return Dict("type" => "matrix_solution", "solution" => [qstring(v) for v in x])
        value = Dict{String,Any}("type" => "matrix_solution", "solution" => nothing)
        certificate && (value["certificate"] = Dict("y" => no_solution(A, AQ, b, K)))
        return value
    elseif op == "matrix.hermite_normal_form"
        K === ZZ || throw(RequestError("the Hermite normal form is over ZZ only"))
        H, U = hnf_with_transform(A)
        value = Dict{String,Any}("type" => "hermite_normal_form", "form" => matrix_json(H))
        certificate && (value["certificate"] = transform_json(U))
        return value
    elseif op == "matrix.smith_normal_form"
        K === ZZ || throw(RequestError("the Smith normal form is over ZZ only"))
        D, U, V = snf_with_transform(A)
        value = Dict{String,Any}(
            "type" => "smith_normal_form",
            "diagonal" => [qstring(D[i, i]) for i in 1:min(m, n)],
        )
        certificate && (value["certificate"] = Dict("left" => transform_json(U), "right" => transform_json(V)))
        return value
    end
    throw(RequestError("unsupported operation $(repr(op))"))
end

# ------------------------------------------------------------- modules

const MODULE_KEYS = Set([:operation, :domain, :nvars, :order])

# The ring R of a module job (Issues #97, #98) and its number of variables n,
# after checking that the request has exactly the keys MODULE_KEYS and `more`.
# No operation of modules depends on the ordering, but the ring is built as
# for ideals.
function module_ring(req, more)
    allowed = union(MODULE_KEYS, Set(more))
    for k in keys(req)
        k in allowed || throw(RequestError("unknown key $(repr(String(k)))"))
    end
    all(k -> haskey(req, k), allowed) || throw(RequestError("incomplete request"))
    req.domain == "QQ" || throw(RequestError("the module operations are over QQ only"))
    n = req.nvars
    (n isa Integer && n >= 1) || throw(RequestError("invalid nvars"))
    R, _, _ = ordered_ring(n, req.order)
    return R, n
end

# The generators of an ideal request in R, all of them and those not zero.
function module_generators(R, n, req)
    req.generators isa AbstractVector || throw(RequestError("invalid generators"))
    F = [parse_polynomial(R, n, g) for g in req.generators]
    return F, filter(!iszero, F)
end

# module.syzygies (Issue #97): generators S (s x r) of the syzygies of the
# columns of the m x s matrix F, F S = 0. OSCAR's modules are of rows, so the
# column j of F is the image of the j-th basis element of R^s in R^m, and the
# generators of the kernel of that map are the columns of S. As checked in
# this image: kernel takes maps that are not homogeneous, a zero column of F
# gives its unit vector among the generators, and coordinates gives the
# coordinates of a generator in R^s.
function syzygies_request(req)
    R, n = module_ring(req, [:rows, :columns])
    m = req.rows
    (m isa Integer && m >= 0) || throw(RequestError("invalid rows"))
    req.columns isa AbstractVector || throw(RequestError("invalid columns"))
    s = length(req.columns)
    target = free_module(R, m)
    images = map(req.columns) do column
        column isa AbstractVector || throw(RequestError("invalid column"))
        v = zero(target)
        for e in column
            i = e.row
            (i isa Integer && 0 <= i < m) || throw(RequestError("invalid row"))
            v += parse_polynomial(R, n, e.terms) * target[i + 1]
        end
        v
    end
    K, _ = kernel(hom(free_module(R, s), target, images))
    generators = ambient_representatives_generators(K)
    S = zero_matrix(R, s, length(generators))
    for (j, g) in enumerate(generators)
        c = coordinates(g)
        for i in 1:s
            S[i, j] = c[i]
        end
    end
    return Dict("syzygies" => poly_matrix_json(S))
end

# The differentials d_1, ..., d_k of a free resolution of R/(F) for the
# nonzero generators F, each as a matrix of R with one column for each basis
# element of F_i, and for homogeneous generators the degrees of the basis of
# F_0, ..., F_k (nothing otherwise). Homogeneous generators are resolved over
# the standard grading of R with algorithm :mres, which gives the minimal
# resolution, and the unit ideal as R <- R, d_1 = (1), as Macaulay2 does:
# :mres gives for it the zero map from R^0, which does not resolve R/I = 0.
# Other generators are resolved over R itself by free_resolution, whose
# differentials may have constant entries. As checked in this image: map(Fr,
# i) is d_i from F_i to F_(i-1), and matrix gives its transpose (a row for each
# basis element of F_i); degrees(domain(map(Fr, i))) are the degrees of the
# basis of F_i.
function resolution_parts(R, n, nonzero, homogeneous)
    isempty(nonzero) && return Any[], homogeneous ? [[0]] : nothing
    if homogeneous && is_one(ideal(R, nonzero))
        return Any[matrix(R, 1, 1, [one(R)])], [[0], [0]]
    end
    if homogeneous
        S, _ = grade(R)
        Fr = free_resolution(quotient_ring_as_module(ideal(S, [S(f) for f in nonzero])); algorithm = :mres)
    else
        Fr = free_resolution(quotient_ring_as_module(ideal(R, nonzero)))
    end
    differentials = Any[]
    degree_lists = homogeneous ? [[0]] : nothing
    for i in 1:length(Fr)
        d = map(Fr, i)
        M = matrix(d)
        D = zero_matrix(R, ncols(M), nrows(M))
        for r in 1:nrows(M), c in 1:ncols(M)
            D[c, r] = homogeneous ? forget_grading(M[r, c]) : M[r, c]
        end
        push!(differentials, D)
        homogeneous && push!(degree_lists, [Int(g[1]) for g in degrees(domain(d))])
    end
    return differentials, degree_lists
end

# complex.free_resolution (Issue #97): the differentials of a free resolution
# of R/I, the degrees for homogeneous generators, and unless the job asks for
# none the certificate C (s x n_1) with d_1 = F C and N (n_1 x s) with F = d_1
# N, lifted as lift_to_generators lifts. The zero generators are left out of
# the resolution (OSCAR resolves the module of a zero generator by R <- R with
# the zero map, which is not exact) and come back as zero rows of C and zero
# columns of N.
function resolution_request(req)
    R, n = module_ring(req, [:generators, :homogeneous, :certificate])
    homogeneous = req.homogeneous
    homogeneous isa Bool || throw(RequestError("invalid homogeneous"))
    req.certificate in ("include", "none") || throw(RequestError("invalid certificate $(repr(req.certificate))"))
    F, nonzero = module_generators(R, n, req)
    differentials, degree_lists = resolution_parts(R, n, nonzero, homogeneous)
    value = Dict{String,Any}("differentials" => [poly_matrix_json(d) for d in differentials])
    degree_lists === nothing || (value["degrees"] = degree_lists)
    if req.certificate == "include"
        d1 = isempty(differentials) ? elem_type(R)[] : [differentials[1][1, j] for j in 1:ncols(differentials[1])]
        C = isempty(nonzero) ? zero_matrix(R, length(F), 0) : lift_to_generators(R, F, d1)
        N = isempty(d1) ? zero_matrix(R, 0, length(F)) : lift_to_generators(R, d1, F)
        value["certificate"] = Dict(
            "differential_in_generators" => poly_matrix_json(C),
            "generators_in_differential" => poly_matrix_json(N),
        )
    end
    return value
end

# ideal.hilbert_series (Issue #98): the Hilbert series of R/I for homogeneous
# generators, P / (1 - t)^d from hilbert_series_reduced over the standard
# grading of R (the denominator is checked to be (1 - t)^d), the zero series
# as [] over (1 - t)^0; on request the graded Betti numbers of R/I from the
# degrees of resolution_parts. As checked in this image:
# hilbert_series_reduced gives the numerator and the expanded denominator,
# (2t + 1, t^2 - 2t + 1) for the twisted cubic, but for the zero ideal the
# denominator as a product of its factors, which evaluate expands.
function hilbert_series_request(req)
    R, n = module_ring(req, [:generators, :betti_table])
    req.betti_table isa Bool || throw(RequestError("invalid betti_table"))
    F, nonzero = module_generators(R, n, req)
    S, _ = grade(R)
    all(f -> is_homogeneous(S(f)), nonzero) || throw(RequestError("the generators are not homogeneous"))
    I = isempty(nonzero) ? ideal(S, elem_type(S)[]) : ideal(S, [S(f) for f in nonzero])
    P, Q = hilbert_series_reduced(quo(S, I)[1])
    Q isa FacElem && (Q = evaluate(Q))
    value = Dict{String,Any}()
    if iszero(P)
        value["numerator"] = String[]
        value["denominator_exponent"] = 0
    else
        T = gen(parent(Q))
        d = degree(Q)
        Q == (1 - T)^d || throw(RequestError("the denominator of the Hilbert series is not a power of 1 - t"))
        value["numerator"] = [string(coeff(P, k)) for k in 0:degree(P)]
        value["denominator_exponent"] = d
    end
    if req.betti_table
        _, degree_lists = resolution_parts(R, n, nonzero, true)
        betti = Any[]
        for (i, list) in enumerate(degree_lists), j in sort(unique(list))
            push!(betti, [i - 1, j, count(==(j), list)])
        end
        value["betti"] = betti
    end
    return value
end

function handle(req)
    op = get(req, :operation, nothing)
    op == "polynomial.factor" && return factor_request(req)
    op isa AbstractString && startswith(op, "matrix.") && return matrix_request(req)
    op == "system.json_integers" && return Dict("values" => [json_integer(parse(BigInt, v)) for v in req.values])
    op == "system.leading_terms" && return leading_terms_request(req)
    op == "ideal.groebner_basis" && return groebner_request(req)
    op == "ideal.normal_form" && return normal_form_request(req)
    op == "ideal.membership" && return membership_request(req)
    op == "module.syzygies" && return syzygies_request(req)
    op == "complex.free_resolution" && return resolution_request(req)
    op == "ideal.hilbert_series" && return hilbert_series_request(req)
    op == "system.info" && return system_info()
    throw(RequestError("unsupported operation $(repr(op))"))
end

function one_line(s)
    replace(s, r"[\r\n]+" => " | ")
end

# Julia compiles a path on its first use: the first Groebner basis with its
# certificate took about 20 s, longer than a short job's deadline, and the
# first lift of one polynomial (a member's coefficients) about 4 s more.
# Taking the normal form of x^3 with its certificate and the S-pairs (the
# paths of a Groebner basis, C, N, H and the division of one polynomial; the
# basis x^2 - y, xy - 1, y^2 - x has two pairs that are not coprime) and the
# coefficients of the member x^3 - 1 of the same small ideal before the ready
# line, through
# handle and JSON3 as a request is answered, moves that time into the
# start-up, which has a limit of its own (300 s, the backend's
# startup_timeout).
const WARM_UP_IDEAL = """"domain": "QQ", "nvars": 2, "order": [["degrevlex", 1, 2]],
    "certificate": "s_pairs",
    "generators": [[{"coefficient": "1", "exponents": [2, 0]}, {"coefficient": "-1", "exponents": [0, 1]}],
                   [{"coefficient": "1", "exponents": [1, 1]}, {"coefficient": "-1", "exponents": [0, 0]}]]"""
JSON3.write(handle(JSON3.read("""{"operation": "ideal.normal_form", $WARM_UP_IDEAL,
    "polynomial": [{"coefficient": "1", "exponents": [3, 0]}]}""")))
JSON3.write(handle(JSON3.read("""{"operation": "ideal.membership", $WARM_UP_IDEAL,
    "polynomial": [{"coefficient": "1", "exponents": [3, 0]}, {"coefficient": "-1", "exponents": [0, 0]}]}""")))

println("ALGEBRAMESH_READY ", JSON3.write(system_info()))
flush(stdout)
for line in eachline(stdin)
    isempty(strip(line)) && continue
    try
        result = handle(JSON3.read(line))
        println("ALGEBRAMESH_RESULT ", JSON3.write(result))
    catch e
        msg = e isa RequestError ? e.msg : sprint(showerror, e)
        println("ALGEBRAMESH_ERROR ", one_line(msg))
    end
    flush(stdout)
end
