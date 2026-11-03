// module: value
use super::input::{ParameterMaterializer, PlainInputMode, PrepareSequence};

type AnyArgument = fn(&str);
type StaticArgument = fn(&'static str);

fn retag_parameter<'call>(
    strategy: ParameterMaterializer<'call, AnyArgument, PlainInputMode>,
) -> ParameterMaterializer<'call, StaticArgument, PlainInputMode> {
    strategy // error: E0308
}

fn retag_sequence<'call>(
    strategy: PrepareSequence<'call, AnyArgument>,
) -> PrepareSequence<'call, StaticArgument> {
    strategy // error: E0308
}
