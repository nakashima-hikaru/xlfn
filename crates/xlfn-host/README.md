# xlfn-host

Excel and Windows host adapter primitives for xlfn. Add-ins use the
`xlfn` facade.

Windows drain orchestration starts a worker with no STA-bound references,
dispatches COM calls on the waiting STA, and joins the worker before exposing
its result. The facade converts errors into its diagnostics and owns the
progress of the lifecycle transaction.

The FP12 process ledger retains one named section anchor across cooperating XLL
images. Individual views outlive published charges. A charge is released only
after its corresponding block is physically freed. Old images using a
different header version cannot reattach safely; restart Excel when upgrading.

Type checking a Windows target does not exercise Office callbacks. Native
qualification needs Windows desktop Excel at the XLL's bitness.
