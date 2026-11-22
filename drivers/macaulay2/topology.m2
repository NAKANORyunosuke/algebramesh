-- AlgebraMesh driver for Macaulay2: the topology package (Issue #103).
--
-- algebramesh.m2 loads this file when it is next to it, before it reads the
-- request; the macaulay2 backend offers simplicial.homology only when it is
-- built with the feature pkg-topology, and then hashes this file with the
-- driver. It uses the helpers of algebramesh.m2 (fail, parseMatrix,
-- canonicalSNF, transformJSON, jsonString, systemInfo), which are defined
-- after it is loaded but before a request is answered, and registers its
-- operation in packageOperations. The request:
--   {"operation": "simplicial.homology", "matrices": [M_1, ..., M_d],
--    "certificate": true|false}
-- with M_k the boundary matrix of dimension k as rows of decimal strings,
-- built by the backend (algebramesh-topology). The answer is
--   {"value": {"smith_forms": [S_1, ..., S_d]}, "info": ...}
-- with S_k the Smith normal form of M_k in the form of matrix.smith_normal_form
-- (the same procedure, canonicalSNF), with its certificate when the request
-- asks for it. algebramesh-topology builds the homology from them and checks
-- them (docs/claims.md).

homologyRequest = req -> (
    certificate := req#?"certificate" and req#"certificate" === true;
    matrices := req#"matrices";
    if not instance(matrices, List) then fail "invalid matrices";
    forms := apply(matrices, rows -> (
        (d, P, Q) := canonicalSNF parseMatrix(rows, ZZ);
        "{\"type\":\"smith_normal_form\",\"diagonal\":[" | demark(",", apply(d, jsonString)) | "]"
        | (if certificate then ",\"certificate\":{\"left\":" | transformJSON P
            | ",\"right\":" | transformJSON Q | "}" else "")
        | "}"));
    "{\"value\":{\"smith_forms\":[" | demark(",", forms) | "]},\"info\":" | systemInfo() | "}"
);

packageOperations#"simplicial.homology" = homologyRequest;
