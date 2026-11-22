use algebramesh_openxm_protocol::{Cmo, SmCommand, ox_tag};

use crate::error::OxError;

/// The capability description returned by `SM_mathcap`.
///
/// Layout (OpenXM-RFC 100): `CMO_MATHCAP(CMO_LIST[info, sm_commands, formats])` where
/// `info = [version, system, ...]` (system as `"Ox_system=<name>"` or as a bare name),
/// `sm_commands` is a list of `CMO_INT32` and `formats` is a list of
/// `[ox_message_tag, [cmo tags ...]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mathcap {
    pub version: Option<u32>,
    /// The server name: the value of an `Ox_system=` entry, or else the first string.
    pub system: Option<String>,
    /// All string entries of the first list, verbatim.
    pub info: Vec<String>,
    pub sm_commands: Vec<u32>,
    /// `(OX message tag, CMO tags accepted with it)`.
    pub formats: Vec<(u32, Vec<u32>)>,
    pub raw: Cmo,
}

impl Mathcap {
    pub fn from_cmo(cmo: Cmo) -> Result<Self, OxError> {
        let Cmo::Mathcap(inner) = &cmo else {
            return Err(OxError::Mathcap(format!(
                "expected CMO_MATHCAP, got CMO tag {}",
                cmo.tag()
            )));
        };
        let parts = inner
            .as_list()
            .ok_or_else(|| OxError::Mathcap("body is not a list".into()))?;
        let [info, commands, rest @ ..] = parts else {
            return Err(OxError::Mathcap(format!(
                "expected at least 2 entries, got {}",
                parts.len()
            )));
        };

        let info = info
            .as_list()
            .ok_or_else(|| OxError::Mathcap("first entry is not a list".into()))?;
        let version = info.first().and_then(Cmo::as_u32);
        let strings: Vec<String> = info
            .iter()
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect();
        // RFC 100 examples use "Ox_system=<name>"; ox_asir sends the bare name.
        let system = strings
            .iter()
            .find_map(|s| s.strip_prefix("Ox_system=").map(str::to_owned))
            .or_else(|| strings.first().cloned());

        let sm_commands = commands
            .as_list()
            .ok_or_else(|| OxError::Mathcap("second entry is not a list".into()))?
            .iter()
            .map(|c| {
                c.as_u32()
                    .ok_or_else(|| OxError::Mathcap("non-integer SM command".into()))
            })
            .collect::<Result<_, _>>()?;

        let mut formats = Vec::new();
        if let Some(entries) = rest.first() {
            let entries = entries
                .as_list()
                .ok_or_else(|| OxError::Mathcap("third entry is not a list".into()))?;
            for entry in entries {
                let pair = entry
                    .as_list()
                    .ok_or_else(|| OxError::Mathcap("format entry is not a list".into()))?;
                let tag = pair
                    .first()
                    .and_then(Cmo::as_u32)
                    .ok_or_else(|| OxError::Mathcap("format entry without tag".into()))?;
                let tags = pair
                    .get(1)
                    .and_then(Cmo::as_list)
                    .map(|l| l.iter().filter_map(Cmo::as_u32).collect())
                    .unwrap_or_default();
                formats.push((tag, tags));
            }
        }

        Ok(Self {
            version,
            system,
            info: strings,
            sm_commands,
            formats,
            raw: cmo,
        })
    }

    pub fn supports_command(&self, command: SmCommand) -> bool {
        self.sm_commands.contains(&command.code())
    }

    /// CMO tags accepted inside `OX_DATA` messages.
    pub fn data_cmo_tags(&self) -> &[u32] {
        self.formats
            .iter()
            .find(|(tag, _)| *tag == ox_tag::DATA)
            .map(|(_, tags)| tags.as_slice())
            .unwrap_or(&[])
    }

    pub fn supports_cmo(&self, cmo_tag: u32) -> bool {
        self.data_cmo_tags().contains(&cmo_tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use algebramesh_openxm_protocol::cmo_tag;

    fn sample() -> Cmo {
        Cmo::Mathcap(Box::new(Cmo::List(vec![
            Cmo::List(vec![
                Cmo::Int32(20_231_026),
                Cmo::string("Ox_system=ox_asir"),
                Cmo::string("Version=20231026"),
                Cmo::string("HOSTTYPE=x86_64"),
            ]),
            Cmo::List(vec![Cmo::Int32(262), Cmo::Int32(264), Cmo::Int32(269)]),
            Cmo::List(vec![
                Cmo::List(vec![
                    Cmo::Int32(514),
                    Cmo::List(vec![Cmo::Int32(2), Cmo::Int32(20), Cmo::Int32(27)]),
                ]),
                Cmo::List(vec![
                    Cmo::Int32(0x7fcd_ef30),
                    Cmo::List(vec![Cmo::Int32(1), Cmo::Int32(0)]),
                ]),
            ]),
        ])))
    }

    #[test]
    fn parses_asir_style_mathcap() {
        let mc = Mathcap::from_cmo(sample()).unwrap();
        assert_eq!(mc.version, Some(20_231_026));
        assert_eq!(mc.system.as_deref(), Some("ox_asir"));
        assert!(mc.supports_command(SmCommand::ExecuteFunction));
        assert!(!mc.supports_command(SmCommand::ExecuteStringByLocalParser));
        assert!(mc.supports_cmo(cmo_tag::ZZ));
        assert!(!mc.supports_cmo(cmo_tag::QQ));
    }

    #[test]
    fn parses_mathcap_observed_from_ox_asir_20260331() {
        let ints = |v: &[u32]| Cmo::List(v.iter().map(|x| Cmo::Int32(*x as i32)).collect());
        let cmo = Cmo::Mathcap(Box::new(Cmo::List(vec![
            Cmo::List(vec![Cmo::Int32(199_901_160), Cmo::string("ox_asir")]),
            ints(&[
                276, 275, 258, 262, 263, 277, 266, 267, 268, 274, 269, 272, 265, 264, 273, 300,
                270, 271, 400, 278, 279, 280, 281, 282, 283,
            ]),
            Cmo::List(vec![
                Cmo::List(vec![
                    Cmo::Int32(514),
                    ints(&[
                        1,
                        2,
                        3,
                        4,
                        5,
                        2_130_706_433,
                        2_130_706_434,
                        17,
                        19,
                        20,
                        21,
                        22,
                        24,
                        25,
                        26,
                        31,
                        34,
                        27,
                        33,
                        60,
                        61,
                        52,
                        35,
                    ]),
                ]),
                Cmo::List(vec![Cmo::Int32(2_144_202_544), ints(&[0, 1])]),
            ]),
        ])));
        let mc = Mathcap::from_cmo(cmo).unwrap();
        assert_eq!(mc.version, Some(199_901_160));
        assert_eq!(mc.system.as_deref(), Some("ox_asir"));
        for tag in [
            cmo_tag::ZZ,
            cmo_tag::QQ,
            cmo_tag::LIST,
            cmo_tag::RECURSIVE_POLYNOMIAL,
            cmo_tag::UNIVARIATE_POLYNOMIAL,
            cmo_tag::INDETERMINATE,
            cmo_tag::ERROR2,
        ] {
            assert!(mc.supports_cmo(tag), "{tag}");
        }
    }

    #[test]
    fn rejects_non_mathcap() {
        assert!(Mathcap::from_cmo(Cmo::Null).is_err());
        assert!(Mathcap::from_cmo(Cmo::Mathcap(Box::new(Cmo::Null))).is_err());
    }
}
