import ExcelXllFormal.Shutdown.Model

set_option autoImplicit false

namespace ExcelXllFormal.Shutdown

inductive Completion where
  | completed
  | canceled
  | failed
  deriving DecidableEq, Repr

/-- Observable labels for the abstract protocol. -/
inductive Event where
  | registerFunction
  | unregisterFunction
  | registerEvent
  | unregisterEvent
  | enterCall
  | leaveCall
  | createReturnBlock
  | beginReturnFree
  | endReturnFree
  | startAsyncExecutor
  | startAsyncTask
  | endAsyncTask (completion : Completion)
  | stopAsyncExecutor
  | beginRtdOperation
  | endRtdOperation
  | addSubscription
  | removeSubscription
  | beginCallback
  | endCallback
  | addRtdClassFactory
  | removeRtdClassFactory
  | addRtdServer
  | removeRtdServer
  | lockRtdServer
  | unlockRtdServer
  | beginHandleOperation
  | endHandleOperation
  | addHandle
  | removeHandle
  | acquireStateLease
  | releaseStateLease
  | startWorker
  | stopWorker
  | submitWorkerJob
  | endWorkerJob (completion : Completion)
  | acquireAddinResource
  | releaseAddinResource
  | startDiagnostics
  | enqueueDiagnostic
  | flushDiagnostic
  | stopDiagnostics
  | beginClose
  | hostDetached
  | callsDrained
  | returnsDrained
  | asyncDrained
  | rtdDrained
  | handlesDrained
  | stateClosed
  | diagnosticsDrained
  | finishClose
  | failStop (reason : Failure)
  deriving DecidableEq, Repr

namespace Event

/-- Work admitted directly from the host (synchronous UDF, async UDF, calculation event callbacks,
COM exports, diagnostics workers, custom exports, and both accepted/rejected ingress paths).
All external entries are guarded by a unified ExportIngress protocol. Internal work spawned by an already
admitted operation is tracked separately and is bounded by the stage gates. -/
def IsExternalAdmission : Event → Prop
  | .enterCall => True
  | .beginRtdOperation => True
  | _ => False

end Event

/-- Certified one-step transition relation.

Every constructor is a refinement obligation for the Rust implementation.
Notably, `finishClose` requires `Resources.Quiescent`; failures that cannot
establish this condition must use `failStop`, never `finishClose`. -/
inductive Step : State → Event → State → Prop where
  | registerFunction {s : State}
      (hOpen : s.phase = .open) :
      Step s .registerFunction
        { s with resources :=
            { s.resources with registrations := s.resources.registrations + 1 } }

  | unregisterFunction {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.registrations = n + 1) :
      Step s .unregisterFunction
        { s with resources := { s.resources with registrations := n } }

  | registerEvent {s : State}
      (hOpen : s.phase = .open) :
      Step s .registerEvent
        { s with resources :=
            { s.resources with eventRegistrations := s.resources.eventRegistrations + 1 } }

  | unregisterEvent {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.eventRegistrations = n + 1) :
      Step s .unregisterEvent
        { s with resources := { s.resources with eventRegistrations := n } }

  | enterCall {s : State}
      (hOpen : s.phase = .open) :
      Step s .enterCall
        { s with resources :=
            { s.resources with activeCalls := s.resources.activeCalls + 1 } }

  | leaveCall {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.activeCalls = n + 1) :
      Step s .leaveCall
        { s with resources := { s.resources with activeCalls := n } }

  | createReturnBlock {s : State}
      (hAllowed : s.phase.AllowsReturnCreation)
      (hCall : s.resources.activeCalls > 0) :
      Step s .createReturnBlock
        { s with resources :=
            { s.resources with returnBlocks := s.resources.returnBlocks + 1 } }

  | beginReturnFree {s : State}
      (hAllowed : s.phase.AllowsReturnFree)
      (hAvailable :
        s.resources.returnFreeOperations < s.resources.returnBlocks) :
      Step s .beginReturnFree
        { s with resources :=
            { s.resources with
              returnFreeOperations := s.resources.returnFreeOperations + 1 } }

  | endReturnFree {s : State} {blocks freeOperations : Nat}
      (hLive : s.phase.IsLive)
      (hBlocks : s.resources.returnBlocks = blocks + 1)
      (hOperations :
        s.resources.returnFreeOperations = freeOperations + 1) :
      Step s .endReturnFree
        { s with resources :=
            { s.resources with
              returnBlocks := blocks,
              returnFreeOperations := freeOperations } }

  | startAsyncExecutor {s : State}
      (hOpen : s.phase = .open)
      (hStopped : s.resources.asyncExecutorRunning = false) :
      Step s .startAsyncExecutor
        { s with resources := { s.resources with asyncExecutorRunning := true } }

  | startAsyncTask {s : State}
      (hAllowed : s.phase.AllowsAsyncCreation)
      (hExecutor : s.resources.asyncExecutorRunning = true)
      (hProducer : s.resources.ProducerAlive) :
      Step s .startAsyncTask
        { s with resources :=
            { s.resources with asyncTasks := s.resources.asyncTasks + 1 } }

  | endAsyncTask {s : State} {n : Nat} {completion : Completion}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.asyncTasks = n + 1) :
      Step s (.endAsyncTask completion)
        { s with resources := { s.resources with asyncTasks := n } }

  | stopAsyncExecutor {s : State}
      (hLive : s.phase.IsLive)
      (hEmpty : s.resources.asyncTasks = 0)
      (hRunning : s.resources.asyncExecutorRunning = true) :
      Step s .stopAsyncExecutor
        { s with resources := { s.resources with asyncExecutorRunning := false } }

  | beginRtdOperation {s : State}
      (hOpen : s.phase = .open) :
      Step s .beginRtdOperation
        { s with resources :=
            { s.resources with rtdOperations := s.resources.rtdOperations + 1 } }

  | endRtdOperation {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.rtdOperations = n + 1) :
      Step s .endRtdOperation
        { s with resources := { s.resources with rtdOperations := n } }

  | addSubscription {s : State}
      (hAllowed : s.phase.AllowsRtdCreation)
      (hOperation : s.resources.rtdOperations > 0) :
      Step s .addSubscription
        { s with resources :=
            { s.resources with subscriptions := s.resources.subscriptions + 1 } }

  | removeSubscription {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.subscriptions = n + 1) :
      Step s .removeSubscription
        { s with resources := { s.resources with subscriptions := n } }

  | beginCallback {s : State}
      (hAllowed : s.phase.AllowsRtdCreation)
      (hSubscription : s.resources.subscriptions > 0) :
      Step s .beginCallback
        { s with resources :=
            { s.resources with callbacks := s.resources.callbacks + 1 } }

  | endCallback {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.callbacks = n + 1) :
      Step s .endCallback
        { s with resources := { s.resources with callbacks := n } }

  | addRtdClassFactory {s : State}
      (hAllowed : s.phase.AllowsRtdCreation)
      (hOperation : s.resources.rtdOperations > 0) :
      Step s .addRtdClassFactory
        { s with resources :=
            { s.resources with
              rtdClassFactories := s.resources.rtdClassFactories + 1 } }

  | removeRtdClassFactory {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.rtdClassFactories = n + 1) :
      Step s .removeRtdClassFactory
        { s with resources := { s.resources with rtdClassFactories := n } }

  | addRtdServer {s : State}
      (hAllowed : s.phase.AllowsRtdCreation)
      (hOperation : s.resources.rtdOperations > 0) :
      Step s .addRtdServer
        { s with resources :=
            { s.resources with rtdServers := s.resources.rtdServers + 1 } }

  | removeRtdServer {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.rtdServers = n + 1) :
      Step s .removeRtdServer
        { s with resources := { s.resources with rtdServers := n } }

  | lockRtdServer {s : State}
      (hAllowed : s.phase.AllowsRtdCreation)
      (hFactory : s.resources.rtdClassFactories > 0) :
      Step s .lockRtdServer
        { s with resources :=
            { s.resources with rtdServerLocks := s.resources.rtdServerLocks + 1 } }

  | unlockRtdServer {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.rtdServerLocks = n + 1) :
      Step s .unlockRtdServer
        { s with resources := { s.resources with rtdServerLocks := n } }

  | beginHandleOperation {s : State}
      (hAllowed : s.phase.AllowsHandleCreation)
      (hProducer : s.resources.ProducerAlive) :
      Step s .beginHandleOperation
        { s with resources :=
            { s.resources with handleOperations := s.resources.handleOperations + 1 } }

  | endHandleOperation {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.handleOperations = n + 1) :
      Step s .endHandleOperation
        { s with resources := { s.resources with handleOperations := n } }

  | addHandle {s : State}
      (hAllowed : s.phase.AllowsHandleCreation)
      (hOperation : s.resources.handleOperations > 0) :
      Step s .addHandle
        { s with resources :=
            { s.resources with handles := s.resources.handles + 1 } }

  | removeHandle {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.handles = n + 1) :
      Step s .removeHandle
        { s with resources := { s.resources with handles := n } }

  | acquireStateLease {s : State}
      (hAllowed : s.phase.AllowsStateResourceCreation)
      (hState : s.resources.stateOwnedByRuntime = true)
      (hProducer : s.resources.ProducerAlive) :
      Step s .acquireStateLease
        { s with resources :=
            { s.resources with
              externalStateLeases := s.resources.externalStateLeases + 1 } }

  | releaseStateLease {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.externalStateLeases = n + 1) :
      Step s .releaseStateLease
        { s with resources := { s.resources with externalStateLeases := n } }

  | startWorker {s : State}
      (hAllowed : s.phase.AllowsStateResourceCreation)
      (hState : s.resources.stateOwnedByRuntime = true)
      (hProducer : s.resources.ProducerAlive) :
      Step s .startWorker
        { s with resources :=
            { s.resources with workers := s.resources.workers + 1 } }

  | stopWorker {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hNoJobs : s.resources.workerJobs = 0)
      (hCount : s.resources.workers = n + 1) :
      Step s .stopWorker
        { s with resources := { s.resources with workers := n } }

  | submitWorkerJob {s : State}
      (hAllowed : s.phase.AllowsStateResourceCreation)
      (hWorker : s.resources.workers > 0)
      (hProducer : s.resources.ProducerAlive) :
      Step s .submitWorkerJob
        { s with resources :=
            { s.resources with workerJobs := s.resources.workerJobs + 1 } }

  | endWorkerJob {s : State} {n : Nat} {completion : Completion}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.workerJobs = n + 1) :
      Step s (.endWorkerJob completion)
        { s with resources := { s.resources with workerJobs := n } }

  | acquireAddinResource {s : State}
      (hAllowed : s.phase.AllowsStateResourceCreation)
      (hState : s.resources.stateOwnedByRuntime = true)
      (hProducer : s.resources.ProducerAlive) :
      Step s .acquireAddinResource
        { s with resources :=
            { s.resources with addinResources := s.resources.addinResources + 1 } }

  | releaseAddinResource {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.addinResources = n + 1) :
      Step s .releaseAddinResource
        { s with resources := { s.resources with addinResources := n } }

  | startDiagnostics {s : State}
      (hOpen : s.phase = .open)
      (hStopped : s.resources.diagnosticsRunning = false) :
      Step s .startDiagnostics
        { s with resources := { s.resources with diagnosticsRunning := true } }

  | enqueueDiagnostic {s : State}
      (hLive : s.phase.IsLive)
      (hRunning : s.resources.diagnosticsRunning = true) :
      Step s .enqueueDiagnostic
        { s with resources :=
            { s.resources with diagnosticsPending := s.resources.diagnosticsPending + 1 } }

  | flushDiagnostic {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.diagnosticsPending = n + 1) :
      Step s .flushDiagnostic
        { s with resources := { s.resources with diagnosticsPending := n } }

  | stopDiagnostics {s : State}
      (hLive : s.phase.IsLive)
      (hEmpty : s.resources.diagnosticsPending = 0)
      (hRunning : s.resources.diagnosticsRunning = true) :
      Step s .stopDiagnostics
        { s with resources := { s.resources with diagnosticsRunning := false } }

  /- Shutdown protocol.  The stage order mirrors `close_addin_inner` but makes
     every successful stage transition conditional on a checked postcondition. -/

  | beginClose {s : State}
      (hOpen : s.phase = .open) :
      Step s .beginClose { s with phase := .closing .detachHost }

  | hostDetached {s : State}
      (hStage : s.phase = .closing .detachHost)
      (hDetached : s.resources.HostDetached) :
      Step s .hostDetached { s with phase := .closing .drainCalls }

  | callsDrained {s : State}
      (hStage : s.phase = .closing .drainCalls)
      (hDrained : s.resources.CallsDrained) :
      Step s .callsDrained { s with phase := .closing .drainReturns }

  | returnsDrained {s : State}
      (hStage : s.phase = .closing .drainReturns)
      (hDrained : s.resources.ReturnsDrained) :
      Step s .returnsDrained { s with phase := .closing .drainAsync }

  | asyncDrained {s : State}
      (hStage : s.phase = .closing .drainAsync)
      (hDrained : s.resources.AsyncDrained) :
      Step s .asyncDrained { s with phase := .closing .drainRtd }

  | rtdDrained {s : State}
      (hStage : s.phase = .closing .drainRtd)
      (hDrained : s.resources.RtdDrained) :
      Step s .rtdDrained { s with phase := .closing .drainHandles }

  | handlesDrained {s : State}
      (hStage : s.phase = .closing .drainHandles)
      (hDrained : s.resources.HandlesDrained) :
      Step s .handlesDrained { s with phase := .closing .closeState }

  | stateClosed {s : State}
      (hStage : s.phase = .closing .closeState)
      (hNoEscapes : s.resources.externalStateLeases = 0)
      (hNoWorkers : s.resources.workers = 0)
      (hNoWorkerJobs : s.resources.workerJobs = 0)
      (hNoResources : s.resources.addinResources = 0)
      (hOwned : s.resources.stateOwnedByRuntime = true) :
      Step s .stateClosed
        { phase := .closing .stopDiagnostics,
          resources := { s.resources with stateOwnedByRuntime := false } }

  | diagnosticsDrained {s : State}
      (hStage : s.phase = .closing .stopDiagnostics)
      (hDrained : s.resources.DiagnosticsDrained) :
      Step s .diagnosticsDrained { s with phase := .closing .finalize }

  | finishClose {s : State}
      (hStage : s.phase = .closing .finalize)
      (hQuiescent : s.resources.Quiescent) :
      Step s .finishClose { s with phase := .closed }

  | failStop {s : State} {reason : Failure}
      (hLive : s.phase.IsLive) :
      Step s (.failStop reason) { s with phase := .failStopped reason }

end ExcelXllFormal.Shutdown
