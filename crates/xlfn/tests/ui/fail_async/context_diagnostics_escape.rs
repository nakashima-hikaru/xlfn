use xlfn::AsyncContext;
use xlfn::diagnostics::DiagnosticHistory;

fn escape(context: AsyncContext<'_, ()>) -> &'static DiagnosticHistory {
    context.diagnostics()
}

fn main() {}
