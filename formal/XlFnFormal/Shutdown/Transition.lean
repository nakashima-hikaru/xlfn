import XlFnFormal.Shutdown.Model

set_option autoImplicit false

namespace XlFnFormal.Shutdown

inductive Completion where
  | completed
  | canceled
  | failed
  deriving DecidableEq, Repr

inductive Event where
  | registerFunction
  | unregisterFunction
  | registerEvent
  | unregisterEvent
  | enterExternal
  | leaveExternal
  | enterCall
  | leaveCall
  | createReturnBlock
  | beginReturnFree
  | releaseReturnBlock
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
  | startDiagnostics
  | enqueueDiagnostic
  | flushDiagnostic
  | discardDiagnostic
  | stopDiagnostics
  | recordCleanupIssue
  | beginClose
  | callsDrained
  | returnsDrained
  | asyncDrained
  | subscriptionsDrained
  | closeCallbackGate
  | hostDetached
  | proveStateUnique
  | proveAddinQuiesced
  | stateClosed
  | handlesDrained
  | diagnosticsDrained
  | rtdDrained
  | finishClose
  | failStop (reason : Failure)
  deriving DecidableEq, Repr

namespace Event

def IsExternalAdmission : Event → Prop
  | .enterExternal => True
  | .beginRtdOperation => True
  | _ => False

end Event

inductive Step : State → Event → State → Prop where
  | registerFunction {s : State}
      (hOpen : s.phase = .open)
      (hIngress : s.resources.ingressOpen = true) :
      Step s .registerFunction
        { s with resources :=
            { s.resources with registrations := s.resources.registrations + 1 } }

  | unregisterFunction {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.registrations = n + 1) :
      Step s .unregisterFunction
        { s with resources := { s.resources with registrations := n } }

  | registerEvent {s : State}
      (hOpen : s.phase = .open)
      (hIngress : s.resources.ingressOpen = true) :
      Step s .registerEvent
        { s with resources :=
            { s.resources with eventRegistrations := s.resources.eventRegistrations + 1 } }

  | unregisterEvent {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.eventRegistrations = n + 1) :
      Step s .unregisterEvent
        { s with resources := { s.resources with eventRegistrations := n } }

  | enterExternal {s : State}
      (hOpen : s.phase = .open)
      (hIngress : s.resources.ingressOpen = true) :
      Step s .enterExternal
        { s with resources :=
            { s.resources with externalEntries := s.resources.externalEntries + 1 } }

  | leaveExternal {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.externalEntries = n + 1) :
      Step s .leaveExternal
        { s with resources := { s.resources with externalEntries := n } }

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
      (hAvailable : s.resources.returnBlocksInFree < s.resources.returnBlocks) :
      Step s .beginReturnFree
        { s with resources :=
            { s.resources with
              returnBlocksInFree := s.resources.returnBlocksInFree + 1
              returnFreeOperations := s.resources.returnFreeOperations + 1 } }

  | releaseReturnBlock {s : State} {blocks blocksInFree : Nat}
      (hLive : s.phase.IsLive)
      (hBlocks : s.resources.returnBlocks = blocks + 1)
      (hInFree : s.resources.returnBlocksInFree = blocksInFree + 1) :
      Step s .releaseReturnBlock
        { s with resources :=
            { s.resources with
              returnBlocks := blocks
              returnBlocksInFree := blocksInFree } }

  | endReturnFree {s : State} {freeOperations : Nat}
      (hLive : s.phase.IsLive)
      (hOperations : s.resources.returnFreeOperations = freeOperations + 1)
      (hReleased : s.resources.returnBlocksInFree ≤ freeOperations) :
      Step s .endReturnFree
        { s with resources :=
            { s.resources with
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
      (hOpen : s.phase = .open)
      (hIngress : s.resources.ingressOpen = true) :
      Step s .beginRtdOperation
        { s with resources :=
            { s.resources with rtdOperations := s.resources.rtdOperations + 1 } }

  | endRtdOperation {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.rtdOperations = n + 1) :
      Step s .endRtdOperation
        { s with resources := { s.resources with rtdOperations := n } }

  | addSubscription {s : State}
      (hAllowed : s.phase.AllowsSubscriptionCreation)
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
      (hAllowed : s.phase.AllowsSubscriptionCreation)
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
      (hAllowed : s.phase.AllowsHandleCreation) :
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
        { s with resources := { s.resources with handles := s.resources.handles + 1 } }

  | removeHandle {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.handles = n + 1) :
      Step s .removeHandle
        { s with resources := { s.resources with handles := n } }

  | startDiagnostics {s : State}
      (hOpen : s.phase = .open)
      (hStopped : s.resources.diagnosticsRunning = false) :
      Step s .startDiagnostics
        { s with resources := { s.resources with diagnosticsRunning := true } }

  | enqueueDiagnostic {s : State}
      (hAllowed : s.phase.AllowsDiagnosticCreation)
      (hRunning : s.resources.diagnosticsRunning = true) :
      Step s .enqueueDiagnostic
        { s with resources :=
            { s.resources with diagnosticsPending := s.resources.diagnosticsPending + 1 } }

  | flushDiagnostic {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.diagnosticsPending = n + 1) :
      Step s .flushDiagnostic
        { s with resources := { s.resources with diagnosticsPending := n } }

  | discardDiagnostic {s : State} {n : Nat}
      (hLive : s.phase.IsLive)
      (hCount : s.resources.diagnosticsPending = n + 1) :
      Step s .discardDiagnostic
        { s with resources := { s.resources with diagnosticsPending := n } }

  | stopDiagnostics {s : State}
      (hLive : s.phase.IsLive)
      (hEmpty : s.resources.diagnosticsPending = 0)
      (hRunning : s.resources.diagnosticsRunning = true) :
      Step s .stopDiagnostics
        { s with resources := { s.resources with diagnosticsRunning := false } }

  | recordCleanupIssue {s : State}
      (hLive : s.phase.IsLive) :
      Step s .recordCleanupIssue
        { s with resources :=
            { s.resources with cleanupIssues := s.resources.cleanupIssues + 1 } }

  | beginClose {s : State}
      (hOpen : s.phase = .open)
      (hIngress : s.resources.ingressOpen = true) :
      Step s .beginClose
        { phase := .closing .drainCalls,
          resources := { s.resources with ingressOpen := false } }

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
      Step s .asyncDrained { s with phase := .closing .stopSubscriptions }

  | subscriptionsDrained {s : State}
      (hStage : s.phase = .closing .stopSubscriptions)
      (hDrained : s.resources.SubscriptionsDrained) :
      Step s .subscriptionsDrained { s with phase := .closing .detachHost }

  | closeCallbackGate {s : State}
      (hStage : s.phase = .closing .detachHost)
      (hOpen : s.resources.callbackGateOpen = true) :
      Step s .closeCallbackGate
        { s with resources := { s.resources with callbackGateOpen := false } }

  | hostDetached {s : State}
      (hStage : s.phase = .closing .detachHost)
      (hDetached : s.resources.HostDetached) :
      Step s .hostDetached { s with phase := .closing .closeState }

  | proveStateUnique {s : State}
      (hStage : s.phase = .closing .closeState)
      (hNotProven : s.resources.stateUnique = false) :
      Step s .proveStateUnique
        { s with resources := { s.resources with stateUnique := true } }

  | proveAddinQuiesced {s : State}
      (hStage : s.phase = .closing .closeState)
      (hNotProven : s.resources.addinQuiesced = false) :
      Step s .proveAddinQuiesced
        { s with resources := { s.resources with addinQuiesced := true } }

  | stateClosed {s : State}
      (hStage : s.phase = .closing .closeState)
      (hUnique : s.resources.stateUnique = true)
      (hQuiesced : s.resources.addinQuiesced = true)
      (hOwned : s.resources.stateOwnedByRuntime = true) :
      Step s .stateClosed
        { phase := .closing .drainHandles,
          resources := { s.resources with stateOwnedByRuntime := false } }

  | handlesDrained {s : State}
      (hStage : s.phase = .closing .drainHandles)
      (hDrained : s.resources.HandlesDrained) :
      Step s .handlesDrained { s with phase := .closing .stopDiagnostics }

  | diagnosticsDrained {s : State}
      (hStage : s.phase = .closing .stopDiagnostics)
      (hDrained : s.resources.DiagnosticsDrained) :
      Step s .diagnosticsDrained { s with phase := .closing .drainRtd }

  | rtdDrained {s : State}
      (hStage : s.phase = .closing .drainRtd)
      (hDrained : s.resources.RtdDrained) :
      Step s .rtdDrained { s with phase := .closing .finalize }

  | finishClose {s : State}
      (hStage : s.phase = .closing .finalize)
      (hQuiescent : s.resources.Quiescent) :
      Step s .finishClose { s with phase := .closed }

  | failStop {s : State} {reason : Failure}
      (hLive : s.phase.IsLive) :
      Step s (.failStop reason) { s with phase := .failStopped reason }

end XlFnFormal.Shutdown
