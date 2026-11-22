//! The catalogue of claims, `docs/claims.md`, read and checked (Issues #61,
//! #62).
//!
//! The catalogue has one section `## <operation>` per operation, a line
//! `契約の確認: ...`, one subsection ``### `<claim>` `` per claim with lines
//! `- <item>: <text>`, and a subsection `### not_claimed` with a list. In the
//! text of an item, names in backquotes are identifiers: claim ids in
//! `depends_on`, methods in `方法`, levels in `水準`, test functions in
//! `変異の試験`, fields in `既存の欄`.
//!
//! [`check`] finds what the rules of ADR 0018 require and the catalogue lacks:
//! a heading or an item that cannot be read, an item missing from a claim, a
//! `checked` claim without a mutation test, a premise that is not in the
//! catalogue, an effective level that does not follow from the premises.
//! [`Catalogue::check_guarantee`] finds what a result claims beyond it.

use std::collections::BTreeMap;

use algebramesh_core::guarantee::{EvidenceLevel, Guarantee};

/// The items every claim has.
pub const ITEMS: [&str; 10] = [
    "命題",
    "範囲",
    "前提",
    "方法",
    "水準",
    "depends_on",
    "既存の欄",
    "水準を上げる方法",
    "支え",
    "変異の試験",
];

/// The methods of ADR 0018, as `Method` serializes them.
pub const METHODS: [&str; 7] = [
    "self_certifying",
    "certificate",
    "recomputation",
    "theorem",
    "independent_implementation",
    "consistency",
    "reference",
];

/// A level of evidence, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Asserted,
    CrossChecked,
    Checked,
}

impl From<EvidenceLevel> for Level {
    fn from(level: EvidenceLevel) -> Self {
        match level {
            EvidenceLevel::Asserted => Self::Asserted,
            EvidenceLevel::CrossChecked => Self::CrossChecked,
            EvidenceLevel::Checked => Self::Checked,
        }
    }
}

impl Level {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "asserted" => Some(Self::Asserted),
            "cross_checked" => Some(Self::CrossChecked),
            "checked" => Some(Self::Checked),
            _ => None,
        }
    }
}

/// One claim of the catalogue.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    pub id: String,
    pub operation: String,
    /// The text of each item, by its name.
    pub items: BTreeMap<String, String>,
}

impl Entry {
    /// The names in backquotes in the text of `item`.
    pub fn names(&self, item: &str) -> Vec<String> {
        self.items.get(item).map_or_else(Vec::new, |t| quoted(t))
    }

    /// The level of the claim itself: `水準` names exactly one level before
    /// `実効水準`, and nothing else in backquotes there.
    pub fn level(&self) -> Option<Level> {
        let text = self.items.get("水準")?;
        let own = text
            .split_once("実効水準")
            .map_or(text.as_str(), |(own, _)| own);
        match quoted(own).as_slice() {
            [level] => Level::parse(level),
            _ => None,
        }
    }

    /// The effective level the catalogue states: the first name after
    /// `実効水準` in `水準`, which must be a level, or the level of the claim
    /// when `実効水準` is not written.
    pub fn stated_effective_level(&self) -> Option<Level> {
        let text = self.items.get("水準")?;
        match text.split_once("実効水準") {
            Some((_, rest)) => quoted(rest).first().and_then(|n| Level::parse(n)),
            None => self.level(),
        }
    }

    /// The methods named in `方法`.
    pub fn methods(&self) -> Vec<String> {
        let mut names = self.names("方法");
        names.retain(|n| METHODS.contains(&n.as_str()));
        names
    }

    /// The premises in `depends_on`, each with whether it holds only in some
    /// case, or why the item cannot be read (Issue #78). The item is `なし.`
    /// or sentences, each ending with `.`, of claim ids in backquotes
    /// separated by `, `; a sentence may begin with a case, `<case> のとき `
    /// (for example `K = ZZ のとき`), and its premises hold only then. Nothing
    /// else is read: a premise outside backquotes would otherwise be dropped
    /// without a word.
    pub fn premises(&self) -> Result<Vec<(String, bool)>, String> {
        let Some(text) = self.items.get("depends_on") else {
            return Ok(Vec::new());
        };
        if text == "なし." {
            return Ok(Vec::new());
        }
        let Some(body) = text.strip_suffix('.') else {
            return Err(format!("{text:?} does not end with a period"));
        };
        let mut premises = Vec::new();
        for sentence in body.split(". ") {
            let (conditional, list) = match sentence.split_once("のとき ") {
                Some((case, list)) if !case.trim().is_empty() && !case.contains('`') => {
                    (true, list)
                }
                Some(_) => return Err(format!("{sentence:?} names no case before のとき")),
                None => (false, sentence),
            };
            for name in list.split(", ") {
                let id = name
                    .strip_prefix('`')
                    .and_then(|n| n.strip_suffix('`'))
                    .filter(|id| !id.is_empty() && !id.contains(['`', ' ']));
                match id {
                    Some(id) => premises.push((id.to_owned(), conditional)),
                    None => {
                        return Err(format!(
                            "{name:?} in {sentence:?} is not a claim id in backquotes"
                        ));
                    }
                }
            }
        }
        Ok(premises)
    }
}

/// One operation of the catalogue.
#[derive(Debug, Clone, Default)]
pub struct Operation {
    pub name: String,
    /// The text after `契約の確認:`.
    pub contract: String,
    pub claims: Vec<Entry>,
    pub not_claimed: Vec<String>,
}

impl Operation {
    /// Whether the user has confirmed the contract (`契約の確認: 済 ...`).
    pub fn confirmed(&self) -> bool {
        self.contract.starts_with('済')
    }
}

/// The catalogue.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    pub operations: Vec<Operation>,
    /// What could not be read, one line each.
    pub unread: Vec<String>,
}

impl Catalogue {
    /// The claim `id`, anywhere in the catalogue: the first operation that
    /// lists it.
    pub fn claim(&self, id: &str) -> Option<&Entry> {
        self.claims().find(|c| c.id == id)
    }

    /// The claim `id` as `operation` lists it, or else as the first
    /// operation that lists it does. A claim may be listed under several
    /// operations whose results give it (Issue #67), each with its items
    /// there; a premise is read under the operation of the claim that
    /// names it, so that a result's claims take their premises from the
    /// same result.
    pub fn claim_in(&self, operation: &str, id: &str) -> Option<&Entry> {
        self.operation(operation)
            .and_then(|o| o.claims.iter().find(|c| c.id == id))
            .or_else(|| self.claim(id))
    }

    pub fn claims(&self) -> impl Iterator<Item = &Entry> {
        self.operations.iter().flat_map(|o| o.claims.iter())
    }

    pub fn operation(&self, name: &str) -> Option<&Operation> {
        self.operations.iter().find(|o| o.name == name)
    }

    /// What the guarantee of a result of `operation` says beyond the
    /// catalogue, one line each: a claim the catalogue does not list for the
    /// operation, a level above the one the catalogue gives the claim, a
    /// method the catalogue does not name, a premise the catalogue does not
    /// list, or one it lists (and not only for some case) that is missing.
    /// A claim of a family, such as `gkz.membership[0]`, is the entry of the
    /// family, `gkz.membership[j]` ([`family`]).
    pub fn check_guarantee(&self, operation: &str, guarantee: &Guarantee) -> Vec<String> {
        let mut problems = Vec::new();
        let Some(listed) = self.operation(operation) else {
            return vec![format!("{operation}: not in the catalogue")];
        };
        for claim in &guarantee.claims {
            let id = &claim.id;
            let Some(entry) = listed.claims.iter().find(|c| c.id == family(id)) else {
                problems.push(format!(
                    "{operation}: the claim {id} is not in the catalogue"
                ));
                continue;
            };
            if entry.level().is_none_or(|l| Level::from(claim.level) > l) {
                problems.push(format!(
                    "{id}: given as {:?}, above the level of the catalogue",
                    claim.level
                ));
            }
            if let Some(method) = claim.method {
                let name = serde_json::to_value(method)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                if !entry.methods().contains(&name) {
                    problems.push(format!("{id}: decided by {name}, which 方法 does not name"));
                }
            }
            let premises = match entry.premises() {
                Ok(premises) => premises,
                Err(e) => {
                    problems.push(format!("{id}: depends_on cannot be read: {e}"));
                    continue;
                }
            };
            for premise in &claim.depends_on {
                if !premises.iter().any(|(p, _)| p == premise) {
                    problems.push(format!("{id}: depends on {premise}, unlike the catalogue"));
                }
            }
            for (premise, conditional) in &premises {
                if !conditional && !claim.depends_on.contains(premise) {
                    problems.push(format!(
                        "{id}: does not depend on {premise}, unlike the catalogue"
                    ));
                }
            }
        }
        problems
    }

    /// The effective level of `id`, as the first operation that lists it
    /// does: the weakest of its own level and of the effective levels of its
    /// premises; `None` for an unknown claim, a claim without a level, or
    /// premises that depend on each other.
    pub fn effective_level(&self, id: &str) -> Option<Level> {
        let entry = self.claim(id)?;
        self.effective_level_in(&entry.operation, id)
    }

    /// [`Self::effective_level`] of `id` as `operation` lists it, its
    /// premises read as in [`Self::claim_in`].
    pub fn effective_level_in(&self, operation: &str, id: &str) -> Option<Level> {
        self.effective(operation, id, &mut Vec::new())
    }

    fn effective(
        &self,
        operation: &str,
        id: &str,
        path: &mut Vec<(String, String)>,
    ) -> Option<Level> {
        let entry = self.claim_in(operation, id)?;
        let key = (entry.operation.clone(), id.to_owned());
        if path.contains(&key) {
            return None;
        }
        let mut level = entry.level()?;
        path.push(key);
        for (premise, _) in entry.premises().ok()? {
            level = level.min(self.effective(&entry.operation, &premise, path)?);
        }
        path.pop();
        Some(level)
    }
}

/// The entry of the catalogue that the claim `id` of a result belongs to:
/// `name[j]` for a claim `name[k]` of a family indexed by a number k (one
/// claim for each operator of `gkz.generate`, for example), and `id` itself
/// otherwise.
pub fn family(id: &str) -> String {
    if let Some((name, index)) = id.strip_suffix(']').and_then(|i| i.rsplit_once('['))
        && !index.is_empty()
        && index.bytes().all(|b| b.is_ascii_digit())
    {
        return format!("{name}[j]");
    }
    id.to_owned()
}

/// The names in backquotes in `text`.
pub fn quoted(text: &str) -> Vec<String> {
    text.split('`')
        .enumerate()
        .filter(|(i, s)| i % 2 == 1 && !s.is_empty())
        .map(|(_, s)| s.to_owned())
        .collect()
}

/// Where a line of the catalogue goes.
enum Place {
    /// Outside the operations, or after a heading that names no claim.
    Nowhere,
    Claim,
    NotClaimed,
}

/// Reads the catalogue from the text of `docs/claims.md`. A section whose
/// heading has no dot is not an operation, and is not read.
pub fn parse(text: &str) -> Catalogue {
    let mut catalogue = Catalogue::default();
    let mut place = Place::Nowhere;
    let mut in_operation = false;
    // Whether the line before was an item of a claim or of `not_claimed`.
    let mut after_item = false;
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let continues = after_item;
        after_item = false;
        if let Some(heading) = line.strip_prefix("## ") {
            let mut words = heading.split_whitespace();
            let name = words.next().unwrap_or_default();
            in_operation = name.contains('.');
            place = Place::Nowhere;
            if in_operation {
                if words.next().is_some() {
                    catalogue.unread.push(format!(
                        "line {n}: the heading of {name} has more than its name"
                    ));
                }
                catalogue.operations.push(Operation {
                    name: name.to_owned(),
                    ..Default::default()
                });
            }
            continue;
        }
        let Some(operation) = catalogue.operations.last_mut().filter(|_| in_operation) else {
            continue;
        };
        if let Some(contract) = line.strip_prefix("契約の確認:") {
            operation.contract = contract.trim().to_owned();
        } else if let Some(heading) = line.strip_prefix("### ") {
            place = Place::Nowhere;
            if heading.trim() == "not_claimed" {
                place = Place::NotClaimed;
            } else if let [id] = quoted(heading).as_slice()
                && heading.trim() == format!("`{id}`")
            {
                place = Place::Claim;
                operation.claims.push(Entry {
                    id: id.clone(),
                    operation: operation.name.clone(),
                    items: BTreeMap::new(),
                });
            } else {
                catalogue
                    .unread
                    .push(format!("line {n}: the heading {heading:?} names no claim"));
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            after_item = !matches!(place, Place::Nowhere);
            match place {
                Place::NotClaimed => operation.not_claimed.push(item.trim().to_owned()),
                Place::Claim => {
                    let claim = operation.claims.last_mut().expect("a claim was begun");
                    match item.split_once(": ") {
                        Some((name, text)) if ITEMS.contains(&name) => {
                            let text = text.trim().to_owned();
                            if claim.items.insert(name.to_owned(), text).is_some() {
                                catalogue.unread.push(format!(
                                    "line {n}: {} has the item {name} twice",
                                    claim.id
                                ));
                            }
                        }
                        _ => catalogue.unread.push(format!(
                            "line {n}: {} has an item that is not one of the ten",
                            claim.id
                        )),
                    }
                }
                Place::Nowhere => {
                    if item
                        .split_once(": ")
                        .is_some_and(|(name, _)| ITEMS.contains(&name))
                    {
                        catalogue
                            .unread
                            .push(format!("line {n}: an item outside a claim"));
                    }
                }
            }
        } else if !line.trim().is_empty()
            && !matches!(place, Place::Nowhere)
            && (continues || line.starts_with(char::is_whitespace))
        {
            // In Markdown the line goes on the item above it, which is read
            // from its first line alone (Issue #78).
            catalogue.unread.push(format!(
                "line {n}: an item goes on to this line, which is not read;                  write each item on one line"
            ));
            after_item = true;
        }
    }
    catalogue
}

/// What the catalogue lacks under the rules of ADR 0018, one line each;
/// empty when it follows them. `test_exists` says whether a test function of
/// that name exists.
pub fn check(catalogue: &Catalogue, test_exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut problems = catalogue.unread.clone();
    for operation in &catalogue.operations {
        let name = &operation.name;
        if operation.contract.is_empty() {
            problems.push(format!("{name}: no line 契約の確認"));
        }
        if operation.claims.is_empty() {
            problems.push(format!("{name}: no claims"));
        }
        if operation.not_claimed.is_empty() {
            problems.push(format!("{name}: no list not_claimed"));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for claim in catalogue.claims() {
        let id = &claim.id;
        // A claim may be listed under several operations, once under each.
        if !seen.insert((claim.operation.clone(), id.clone())) {
            problems.push(format!("{id}: listed twice under {}", claim.operation));
        }
        for item in ITEMS {
            if claim.items.get(item).is_none_or(|t| t.is_empty()) {
                problems.push(format!("{id}: no item {item}"));
            }
        }
        let Some(level) = claim.level() else {
            problems.push(format!(
                "{id}: 水準 does not name one level before 実効水準"
            ));
            continue;
        };
        if level != Level::Asserted && claim.methods().is_empty() {
            problems.push(format!("{id}: 方法 names no method"));
        }
        let premises = match claim.premises() {
            Ok(premises) => premises,
            Err(e) => {
                problems.push(format!("{id}: depends_on cannot be read: {e}"));
                continue;
            }
        };
        let unknown: Vec<String> = premises
            .into_iter()
            .map(|(p, _)| p)
            .filter(|p| catalogue.claim_in(&claim.operation, p).is_none())
            .collect();
        for premise in &unknown {
            problems.push(format!(
                "{id}: depends on {premise}, which is not in the catalogue"
            ));
        }
        match catalogue.effective_level_in(&claim.operation, id) {
            None if !unknown.is_empty() => {}
            None => problems.push(format!("{id}: its premises depend on each other")),
            Some(effective) if Some(effective) != claim.stated_effective_level() => {
                problems.push(format!(
                    "{id}: the effective level is {effective:?}, which 水準 does not state"
                ));
            }
            Some(_) => {}
        }
        if level == Level::Checked {
            let tests = claim.names("変異の試験");
            if tests.is_empty() {
                problems.push(format!("{id}: checked, but no mutation test is named"));
            }
            for test in tests {
                if Level::parse(&test).is_some() || METHODS.contains(&test.as_str()) {
                    problems.push(format!("{id}: names {test} as a mutation test"));
                } else if !test_exists(&test) {
                    problems.push(format!("{id}: the mutation test {test} does not exist"));
                }
            }
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use algebramesh_core::guarantee::{Claim, Method, Scope, Summary, VerifierInfo};

    const SAMPLE: &str = "# 主張の契約

## 序

- 命題: not read

## op.one

契約の確認: 未.

### `one.a`

- 命題: P.
- 範囲: 厳密.
- 前提: なし.
- 方法: `self_certifying`.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: `x`.
- 水準を上げる方法: 最上位である.
- 支え: 試験.
- 変異の試験: `a_test`.

### `one.b`

- 命題: Q.
- 範囲: 厳密.
- 前提: なし.
- 方法: backend の答え.
- 水準: `asserted`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 照合.
- 支え: なし.
- 変異の試験: なし (水準が `asserted` のため).

### `one.c`

- 命題: R.
- 範囲: 厳密.
- 前提: なし.
- 方法: `certificate`.
- 水準: `checked`. 実効水準は `asserted` (`one.b` による).
- depends_on: `one.b`. K = ZZ のとき `one.a`.
- 既存の欄: なし.
- 水準を上げる方法: `one.b` を上げる.
- 支え: 試験.
- 変異の試験: `c_test`.

### not_claimed

- 収束.
";

    fn problems(text: &str) -> Vec<String> {
        check(&parse(text), |_| true)
    }

    #[test]
    fn a_catalogue_that_follows_the_rules_has_no_problems() {
        let catalogue = parse(SAMPLE);
        assert_eq!(catalogue.operations.len(), 1);
        let op = &catalogue.operations[0];
        assert_eq!((op.name.as_str(), op.confirmed()), ("op.one", false));
        assert_eq!(op.claims.len(), 3);
        assert_eq!(op.not_claimed, ["収束."]);
        assert_eq!(catalogue.effective_level("one.c"), Some(Level::Asserted));
        let c = catalogue.claim("one.c").unwrap();
        assert_eq!(
            c.premises().unwrap(),
            [("one.b".into(), false), ("one.a".into(), true)]
        );
        assert_eq!(c.methods(), ["certificate"]);
        assert_eq!(catalogue.claim("one.a").unwrap().names("既存の欄"), ["x"]);
        assert!(problems(SAMPLE).is_empty());
    }

    /// Issue #78: a premise outside backquotes, or on a second line of the
    /// item, would be dropped, and a claim at the level `asserted` loses
    /// nothing a level shows. Both are refused, and so is every other way of
    /// writing `depends_on` but `なし.` and ids in backquotes.
    #[test]
    fn premises_that_cannot_be_read_are_refused() {
        let premises = |text: &str| {
            let catalogue =
                parse(&SAMPLE.replace("- depends_on: `one.b`. K = ZZ のとき `one.a`.", text));
            let c = catalogue.claim("one.c").unwrap().clone();
            (
                c.premises(),
                problems(&SAMPLE.replace("- depends_on: `one.b`. K = ZZ のとき `one.a`.", text)),
            )
        };
        let (read, found) = premises("- depends_on: `one.b`, `one.a`.");
        assert_eq!(
            read.unwrap(),
            [("one.b".into(), false), ("one.a".into(), false)]
        );
        assert!(found.is_empty(), "{found:?}");
        for text in [
            "- depends_on: one.b, one.a.",
            "- depends_on: `one.b`, one.a.",
            "- depends_on: `one.b` and `one.a`.",
            "- depends_on: `one.b`",
            "- depends_on: のとき `one.a`.",
            "- depends_on: なし",
        ] {
            let (read, found) = premises(text);
            assert!(read.is_err(), "{text}: {read:?}");
            assert!(
                found
                    .iter()
                    .any(|p| p.contains("depends_on cannot be read")),
                "{text}: {found:?}"
            );
        }
        // Wrapped, the second premise is on a line of its own; so is a
        // lazy continuation without the indent.
        for wrapped in [
            "- depends_on: `one.b`,
  `one.a`.",
            "- depends_on: `one.b`,
`one.a`.",
        ] {
            let (_, found) = premises(wrapped);
            assert!(
                found.iter().any(|p| p.contains("not read")),
                "{wrapped}: {found:?}"
            );
        }
        // A paragraph after a blank line is not part of an item.
        let paragraph = SAMPLE.replace(
            "- 変異の試験: `a_test`.",
            "- 変異の試験: `a_test`.

The reason.",
        );
        assert!(problems(&paragraph).is_empty());
    }

    /// Issue #62: a `checked` claim without a mutation test, or with one that
    /// does not exist, is a problem; so is a level read as a test name.
    #[test]
    fn a_checked_claim_needs_an_existing_mutation_test() {
        let without = SAMPLE.replace("- 変異の試験: `a_test`.", "- 変異の試験: なし.");
        assert_eq!(
            problems(&without),
            ["one.a: checked, but no mutation test is named"]
        );
        let missing = check(&parse(SAMPLE), |t| t != "c_test");
        assert_eq!(missing, ["one.c: the mutation test c_test does not exist"]);
        // A claim raised to checked whose item still says why it needs none.
        let raised = SAMPLE.replace(
            "- 方法: backend の答え.
- 水準: `asserted`.",
            "- 方法: `self_certifying`.
- 水準: `checked`.",
        );
        assert_eq!(
            problems(&raised),
            [
                "one.b: names asserted as a mutation test",
                "one.c: the effective level is Checked, which 水準 does not state"
            ]
        );
    }

    #[test]
    fn missing_items_and_wrong_levels_are_problems() {
        let no_scope = SAMPLE.replacen("- 範囲: 厳密.\n", "", 1);
        assert_eq!(problems(&no_scope), ["one.a: no item 範囲"]);
        let unstated = SAMPLE.replace(" 実効水準は `asserted` (`one.b` による).", "");
        assert_eq!(
            problems(&unstated),
            ["one.c: the effective level is Asserted, which 水準 does not state"]
        );
        let unknown = SAMPLE.replace("- depends_on: `one.b`.", "- depends_on: `one.z`.");
        assert_eq!(
            problems(&unknown),
            ["one.c: depends on one.z, which is not in the catalogue"]
        );
        let circular = SAMPLE.replace(
            "- depends_on: なし.\n- 既存の欄: `x`.",
            "- depends_on: `one.c`.\n- 既存の欄: `x`.",
        );
        assert!(problems(&circular).contains(&"one.c: its premises depend on each other".into()));
        // A misspelt level is no level, even with an effective level after it.
        let misspelt = SAMPLE.replace("- 水準: `checked`. 実効水準", "- 水準: `checekd`. 実効水準");
        assert_eq!(
            problems(&misspelt),
            ["one.c: 水準 does not name one level before 実効水準"]
        );
        let unquoted = SAMPLE.replace("- 水準: `checked`. 実効水準", "- 水準: checked. 実効水準");
        assert_eq!(
            problems(&unquoted),
            ["one.c: 水準 does not name one level before 実効水準"]
        );
    }

    /// Issue #67: a claim the results of several operations give is listed
    /// under each, with its own items there, and its premises are read
    /// under the same operation; twice under one operation is a problem.
    #[test]
    fn a_claim_may_be_listed_under_several_operations() {
        let two = format!(
            "{SAMPLE}\n## op.two\n\n契約の確認: 未.\n\n{CHECKED_B}\n### not_claimed\n\n- 収束.\n"
        );
        let catalogue = parse(&two);
        assert!(problems(&two).is_empty(), "{:?}", problems(&two));
        // Under op.one, one.c takes the level of one.b asserted there; under
        // op.two one.b is checked.
        assert_eq!(
            catalogue.effective_level_in("op.one", "one.c"),
            Some(Level::Asserted)
        );
        assert_eq!(
            catalogue.effective_level_in("op.two", "one.c"),
            Some(Level::Checked)
        );
        assert_eq!(catalogue.effective_level("one.c"), Some(Level::Asserted));
        assert_eq!(
            catalogue.claim_in("op.two", "one.b").unwrap().operation,
            "op.two"
        );
        // A premise the operation does not list is read where it is listed.
        assert_eq!(
            catalogue.claim_in("op.two", "one.a").unwrap().operation,
            "op.one"
        );
        // A guarantee of op.two is read against op.two.
        let right = vec![
            claim(
                "one.b",
                EvidenceLevel::Checked,
                Some(Method::SelfCertifying),
                &[],
            ),
            claim(
                "one.c",
                EvidenceLevel::Checked,
                Some(Method::Certificate),
                &["one.b"],
            ),
        ];
        assert!(
            catalogue
                .check_guarantee("op.two", &guarantee(right))
                .is_empty()
        );
        // The effective level stated under op.two follows from op.two.
        let stated = two.replace(
            "- 水準: `checked`.\n- depends_on: `one.b`.",
            "- 水準: `checked`. 実効水準は `asserted` (`one.b` による).\n- depends_on: `one.b`.",
        );
        assert_eq!(
            problems(&stated),
            ["one.c: the effective level is Checked, which 水準 does not state"]
        );
        // Twice under one operation.
        let twice = SAMPLE.replace("### not_claimed", &format!("{ONE_A}\n### not_claimed"));
        assert_eq!(problems(&twice), ["one.a: listed twice under op.one"]);
    }

    /// `one.b` at the level `checked`, and `one.c` after it, as op.two lists
    /// them.
    const CHECKED_B: &str = "### `one.b`

- 命題: Q.
- 範囲: 厳密.
- 前提: なし.
- 方法: `self_certifying`.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験.
- 変異の試験: `b_test`.

### `one.c`

- 命題: R.
- 範囲: 厳密.
- 前提: なし.
- 方法: `certificate`.
- 水準: `checked`.
- depends_on: `one.b`.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験.
- 変異の試験: `c_test`.
";

    /// `one.a` once more.
    const ONE_A: &str = "### `one.a`

- 命題: P.
- 範囲: 厳密.
- 前提: なし.
- 方法: `self_certifying`.
- 水準: `checked`.
- depends_on: なし.
- 既存の欄: なし.
- 水準を上げる方法: 最上位である.
- 支え: 試験.
- 変異の試験: `a_test`.
";

    /// A heading or an item that cannot be read is a problem, and the items
    /// after a heading that names no claim go to no claim.
    #[test]
    fn what_cannot_be_read_is_a_problem() {
        let unquoted = SAMPLE.replace("### `one.b`", "### one.b");
        let catalogue = parse(&unquoted);
        assert_eq!(catalogue.operations[0].claims.len(), 2);
        assert_eq!(catalogue.claim("one.a").unwrap().items["命題"], "P.");
        let found = problems(&unquoted);
        assert_eq!(found[0], "line 24: the heading \"one.b\" names no claim");
        assert_eq!(found[1], "line 26: an item outside a claim");
        assert!(found.contains(&"one.c: depends on one.b, which is not in the catalogue".into()));
        let typo = SAMPLE.replacen("- 範囲: 厳密.", "- 範図: 厳密.", 1);
        assert_eq!(
            problems(&typo),
            [
                "line 14: one.a has an item that is not one of the ten",
                "one.a: no item 範囲"
            ]
        );
        let twice = SAMPLE.replacen("- 範囲: 厳密.", "- 範囲: 厳密.\n- 範囲: 打切り.", 1);
        assert_eq!(problems(&twice), ["line 15: one.a has the item 範囲 twice"]);
        let longer = SAMPLE.replace("## op.one", "## op.one (V2)");
        assert_eq!(
            problems(&longer),
            ["line 7: the heading of op.one has more than its name"]
        );
    }

    fn guarantee(claims: Vec<Claim>) -> Guarantee {
        Guarantee {
            schema: 1,
            catalog: None,
            verifier: VerifierInfo {
                name: "v".into(),
                version: "1".into(),
                commit: None,
            },
            summary: Summary::Unverified,
            claims,
            not_claimed: vec![],
        }
    }

    fn claim(id: &str, level: EvidenceLevel, method: Option<Method>, depends_on: &[&str]) -> Claim {
        Claim {
            id: id.into(),
            level,
            method,
            by: "v".into(),
            scope: Scope::default(),
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            reason: None,
        }
    }

    /// Issue #61: a result claims only what the catalogue lists, at no higher
    /// level, by a method it names, with its premises.
    #[test]
    fn a_guarantee_claims_only_what_the_catalogue_lists() {
        let catalogue = parse(SAMPLE);
        let check = |claims| catalogue.check_guarantee("op.one", &guarantee(claims));
        use EvidenceLevel::{Asserted, Checked};
        let right = vec![
            claim("one.a", Checked, Some(Method::SelfCertifying), &[]),
            claim("one.b", Asserted, None, &[]),
            claim(
                "one.c",
                Checked,
                Some(Method::Certificate),
                &["one.b", "one.a"],
            ),
        ];
        assert!(check(right).is_empty());
        // The premise for some case may be left out.
        assert!(
            check(vec![claim(
                "one.c",
                Checked,
                Some(Method::Certificate),
                &["one.b"]
            )])
            .is_empty()
        );
        assert_eq!(
            check(vec![claim("one.z", Asserted, None, &[])]),
            ["op.one: the claim one.z is not in the catalogue"]
        );
        assert_eq!(
            check(vec![claim(
                "one.b",
                Checked,
                Some(Method::Consistency),
                &[]
            )]),
            [
                "one.b: given as Checked, above the level of the catalogue",
                "one.b: decided by consistency, which 方法 does not name"
            ]
        );
        assert_eq!(
            check(vec![claim(
                "one.c",
                Checked,
                Some(Method::Certificate),
                &[]
            )]),
            ["one.c: does not depend on one.b, unlike the catalogue"]
        );
        assert_eq!(
            check(vec![claim(
                "one.a",
                Checked,
                Some(Method::SelfCertifying),
                &["one.b"]
            )]),
            ["one.a: depends on one.b, unlike the catalogue"]
        );
        assert_eq!(
            catalogue.check_guarantee("op.two", &guarantee(vec![])),
            ["op.two: not in the catalogue"]
        );
    }
}
