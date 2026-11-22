//! Requests `system.leading_terms` for the OSCAR driver (Issue #64), for
//! `tests/integration/check-driver-orders.sh`.
//!
//! Reads on stdin the cases the `order_cases` example of
//! algebramesh-math-types writes: one JSON line for each order, with a ring,
//! polynomials over it and the exponents of their leading terms by
//! `MonomialOrder::compare`. For each case it writes one line to the file
//! REQUESTS, the request with the order as this backend sends it
//! (`order_items`), and one line to the file REPLIES, the answer the driver
//! must give:
//!
//! ```text
//! {"operation":"system.leading_terms","nvars":4,"order":[["degrevlex",1,4]],
//!  "polynomials":[[],[{"coefficient":"1","exponents":[2,3,1,2]},...],...]}
//! {"leading_exponents":[null,[2,3,1,2],...]}
//! ```
//!
//! A ring without variables keeps `"nvars":0`, with the item of the
//! driver's own variable (`[["lex",1,1]]`); the driver answers with the empty
//! exponent vector `[]` for each nonzero polynomial.
//!
//! Usage (the name carries the backend, since the examples of all the packages
//! are built into one directory):
//!
//! ```text
//! cargo run -p algebramesh-math-types --example order_cases > cases
//! cargo run -p algebramesh-backend-oscar --example oscar_leading_terms -- REQUESTS REPLIES < cases
//! ```

use std::fs::File;
use std::io::{BufRead, BufWriter, Write};

use algebramesh_backend_oscar::{OrderItem, order_items};
use algebramesh_math_types::{Polynomial, PolynomialData, RingSpec, TermData};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    ring: RingSpec,
    polynomials: Vec<PolynomialData>,
    leading_exponents: Vec<Option<Vec<u32>>>,
}

#[derive(Serialize)]
struct Request {
    operation: &'static str,
    nvars: usize,
    order: Vec<OrderItem>,
    polynomials: Vec<Vec<TermData>>,
}

#[derive(Serialize)]
struct Reply {
    leading_exponents: Vec<Option<Vec<u32>>>,
}

fn create(path: &str) -> Result<BufWriter<File>, String> {
    File::create(path)
        .map(BufWriter::new)
        .map_err(|e| format!("{path}: {e}"))
}

fn write_line(file: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *file, value).map_err(|e| e.to_string())?;
    writeln!(file).map_err(|e| e.to_string())
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [requests, replies] = args.as_slice() else {
        return Err("usage: oscar_leading_terms REQUESTS REPLIES < cases".into());
    };
    let (mut requests, mut replies) = (create(requests)?, create(replies)?);
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        let case: Case = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if case.leading_exponents.len() != case.polynomials.len() {
            return Err("a case has not one leading exponent for each polynomial".into());
        }
        let nvars = case.ring.nvars();
        let polynomials = case
            .polynomials
            .iter()
            .map(|p| Polynomial::from_data(case.ring.clone(), p).map(|p| p.to_data().terms))
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        let request = Request {
            operation: "system.leading_terms",
            nvars,
            order: order_items(&case.ring.order, nvars).map_err(|e| e.to_string())?,
            polynomials,
        };
        write_line(&mut requests, &request)?;
        write_line(
            &mut replies,
            &Reply {
                leading_exponents: case.leading_exponents,
            },
        )?;
    }
    requests.flush().map_err(|e| e.to_string())?;
    replies.flush().map_err(|e| e.to_string())
}
