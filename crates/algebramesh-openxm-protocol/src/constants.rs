//! Numeric constants of OpenXM-RFC 100 (`OpenXM/include/ox.h`).

/// OX message tags (the first 32-bit word of every OX message).
pub mod ox_tag {
    pub const COMMAND: u32 = 513;
    pub const DATA: u32 = 514;
    pub const SYNC_BALL: u32 = 515;
    pub const DATA_WITH_SIZE: u32 = 521;
    pub const DATA_ASIR_BINARY_EXPRESSION: u32 = 522;
    pub const LOCAL_OBJECT_ASIR: u32 = 0x7fcd_ef30;
}

/// CMO type tags.
pub mod cmo_tag {
    pub const ERROR2: u32 = 0x7f00_0002;
    pub const NULL: u32 = 1;
    pub const INT32: u32 = 2;
    pub const DATUM: u32 = 3;
    pub const STRING: u32 = 4;
    pub const MATHCAP: u32 = 5;
    pub const ARRAY: u32 = 16;
    pub const LIST: u32 = 17;
    pub const ATOM: u32 = 18;
    pub const MONOMIAL32: u32 = 19;
    pub const ZZ: u32 = 20;
    pub const QQ: u32 = 21;
    pub const ZERO: u32 = 22;
    pub const DMS_GENERIC: u32 = 24;
    pub const DMS_OF_N_VARIABLES: u32 = 25;
    pub const RING_BY_NAME: u32 = 26;
    pub const RECURSIVE_POLYNOMIAL: u32 = 27;
    pub const DISTRIBUTED_POLYNOMIAL: u32 = 31;
    pub const UNIVARIATE_POLYNOMIAL: u32 = 33;
    pub const RATIONAL: u32 = 34;
    pub const COMPLEX: u32 = 35;
    pub const IEEE_DOUBLE_FLOAT: u32 = 51;
    pub const BIGFLOAT32: u32 = 52;
    pub const INDETERMINATE: u32 = 60;
    pub const TREE: u32 = 61;
    pub const LAMBDA: u32 = 62;
}

macro_rules! sm_commands {
    ($($(#[$doc:meta])* $name:ident = $code:expr,)*) => {
        /// Stack Machine commands carried in the body of an `OX_COMMAND` message.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u32)]
        pub enum SmCommand {
            $($(#[$doc])* $name = $code,)*
        }

        impl SmCommand {
            pub const fn code(self) -> u32 {
                self as u32
            }

            pub const fn from_code(code: u32) -> Option<Self> {
                match code {
                    $($code => Some(Self::$name),)*
                    _ => None,
                }
            }
        }
    };
}

sm_commands! {
    PopSerializedLocalObject = 258,
    PopCmo = 262,
    PopString = 263,
    Mathcap = 264,
    Pops = 265,
    SetName = 266,
    EvalName = 267,
    /// Executes a source string with the server's own parser.
    /// Not exposed through the job API (design §5.4).
    ExecuteStringByLocalParser = 268,
    ExecuteFunction = 269,
    BeginBlock = 270,
    EndBlock = 271,
    Shutdown = 272,
    SetMathcap = 273,
    ExecuteStringByLocalParserInBatchMode = 274,
    Getsp = 275,
    DupErrors = 276,
    PushCmoTag = 277,
    Nop = 300,
    ExecuteFunctionSync = 400,
    ControlKill = 1024,
    ControlIntr = 1025,
    ControlResetConnection = 1030,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn design_document_command_codes() {
        assert_eq!(ox_tag::COMMAND, 513);
        assert_eq!(ox_tag::DATA, 514);
        assert_eq!(SmCommand::PopCmo.code(), 262);
        assert_eq!(SmCommand::Mathcap.code(), 264);
        assert_eq!(SmCommand::ExecuteFunction.code(), 269);
        assert_eq!(SmCommand::ExecuteStringByLocalParser.code(), 268);
        assert_eq!(SmCommand::from_code(272), Some(SmCommand::Shutdown));
        assert_eq!(SmCommand::from_code(9999), None);
    }
}
