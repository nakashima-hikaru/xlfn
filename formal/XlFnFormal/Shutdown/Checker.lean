import XlFnFormal.Shutdown.Safety

set_option autoImplicit false

namespace XlFnFormal.Shutdown

/-! A computable transition function for the finite event labels.  The target
    state is determined by the event; only the proposition-valued precondition
    is checked at runtime.  The two theorems below tie this executable view to
    the inductive `Step` relation. -/

private def live (phase : Phase) : Bool :=
  match phase with
  | .open | .closing _ => true
  | .closed | .failStopped _ => false

private def decrement (value : Nat) : Nat :=
  value - 1

private def allowsReturnCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls => true
  | _ => false

private def allowsReturnFree (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns => true
  | _ => false

private def allowsAsyncCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns | .closing .drainAsync => true
  | _ => false

private def allowsSubscriptionCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns | .closing .drainAsync |
      .closing .stopSubscriptions => true
  | _ => false

private def allowsRtdCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns | .closing .drainAsync |
      .closing .stopSubscriptions | .closing .detachHost | .closing .closeState |
      .closing .drainHandles | .closing .stopDiagnostics | .closing .drainRtd => true
  | _ => false

private def allowsHandleCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns | .closing .drainAsync |
      .closing .stopSubscriptions | .closing .detachHost | .closing .closeState |
      .closing .drainHandles => true
  | _ => false

private def allowsDiagnosticCreation (phase : Phase) : Bool :=
  match phase with
  | .open | .closing .drainCalls | .closing .drainReturns | .closing .drainAsync |
      .closing .stopSubscriptions | .closing .detachHost | .closing .closeState |
      .closing .drainHandles | .closing .stopDiagnostics => true
  | _ => false

private def hostDetached (r : Resources) : Bool :=
  r.ingressOpen == false && r.registrations == 0 && r.eventRegistrations == 0 &&
    r.registrationStateKnown == true && r.callbackGateOpen == false

private def callsDrained (r : Resources) : Bool :=
  r.externalEntries == 0 && r.activeCalls == 0

private def returnsDrained (r : Resources) : Bool :=
  r.returnBlocks == 0 && r.returnFreeOperations == 0

private def asyncDrained (r : Resources) : Bool :=
  r.asyncTasks == 0 && r.asyncExecutorRunning == false

private def subscriptionsDrained (r : Resources) : Bool :=
  r.subscriptions == 0 && r.callbacks == 0

private def rtdDrained (r : Resources) : Bool :=
  r.rtdOperations == 0 && r.rtdClassFactories == 0 && r.rtdServers == 0 &&
    r.rtdServerLocks == 0

private def handlesDrained (r : Resources) : Bool :=
  r.handleOperations == 0 && r.handles == 0

private def stateClosed (r : Resources) : Bool :=
  r.stateUnique == true && r.addinQuiesced == true && r.stateOwnedByRuntime == false

private def diagnosticsDrained (r : Resources) : Bool :=
  r.diagnosticsPending == 0 && r.diagnosticsRunning == false

private def producerAlive (r : Resources) : Bool :=
  !(r.externalEntries == 0) || !(r.activeCalls == 0) ||
  !(r.returnFreeOperations == 0) || !(r.asyncTasks == 0) ||
  !(r.rtdOperations == 0) || !(r.subscriptions == 0) ||
  !(r.callbacks == 0) || !(r.handleOperations == 0) ||
  !(r.diagnosticsPending == 0)

private def quiescent (r : Resources) : Bool :=
  hostDetached r && callsDrained r && returnsDrained r && asyncDrained r &&
    subscriptionsDrained r && rtdDrained r && handlesDrained r &&
    stateClosed r && diagnosticsDrained r

def isQuiescent (r : Resources) : Bool :=
  quiescent r

private theorem decrement_add_one {n : Nat} (h : 0 < n) :
    decrement n + 1 = n := by
  unfold decrement
  exact Nat.sub_add_cancel (by omega)

private theorem decrement_succ (n : Nat) :
    decrement (n + 1) = n := by
  simp [decrement]

private theorem producer_alive_bool_iff (r : Resources) :
    producerAlive r = true ↔ Resources.ProducerAlive r := by
  simp [producerAlive, Resources.ProducerAlive]
  omega

private theorem live_iff (phase : Phase) :
    live phase = true ↔ phase.IsLive := by
  cases phase <;> simp [live, Phase.IsLive]

private theorem allows_return_creation_iff (phase : Phase) :
    allowsReturnCreation phase = true ↔ Phase.AllowsReturnCreation phase := by
  cases phase with
  | «open» => simp [allowsReturnCreation, Phase.AllowsReturnCreation]
  | closing stage =>
      cases stage <;> simp [allowsReturnCreation, Phase.AllowsReturnCreation]
  | closed => simp [allowsReturnCreation, Phase.AllowsReturnCreation]
  | failStopped reason => simp [allowsReturnCreation, Phase.AllowsReturnCreation]

private theorem allows_return_free_iff (phase : Phase) :
    allowsReturnFree phase = true ↔ Phase.AllowsReturnFree phase := by
  cases phase with
  | «open» => simp [allowsReturnFree, Phase.AllowsReturnFree]
  | closing stage =>
      cases stage <;> simp [allowsReturnFree, Phase.AllowsReturnFree]
  | closed => simp [allowsReturnFree, Phase.AllowsReturnFree]
  | failStopped reason => simp [allowsReturnFree, Phase.AllowsReturnFree]

private theorem allows_async_creation_iff (phase : Phase) :
    allowsAsyncCreation phase = true ↔ Phase.AllowsAsyncCreation phase := by
  cases phase with
  | «open» => simp [allowsAsyncCreation, Phase.AllowsAsyncCreation]
  | closing stage =>
      cases stage <;> simp [allowsAsyncCreation, Phase.AllowsAsyncCreation]
  | closed => simp [allowsAsyncCreation, Phase.AllowsAsyncCreation]
  | failStopped reason => simp [allowsAsyncCreation, Phase.AllowsAsyncCreation]

private theorem allows_subscription_creation_iff (phase : Phase) :
    allowsSubscriptionCreation phase = true ↔ Phase.AllowsSubscriptionCreation phase := by
  cases phase with
  | «open» => simp [allowsSubscriptionCreation, Phase.AllowsSubscriptionCreation]
  | closing stage =>
      cases stage <;> simp [allowsSubscriptionCreation, Phase.AllowsSubscriptionCreation]
  | closed => simp [allowsSubscriptionCreation, Phase.AllowsSubscriptionCreation]
  | failStopped reason => simp [allowsSubscriptionCreation, Phase.AllowsSubscriptionCreation]

private theorem allows_rtd_creation_iff (phase : Phase) :
    allowsRtdCreation phase = true ↔ Phase.AllowsRtdCreation phase := by
  cases phase with
  | «open» => simp [allowsRtdCreation, Phase.AllowsRtdCreation]
  | closing stage =>
      cases stage <;> simp [allowsRtdCreation, Phase.AllowsRtdCreation]
  | closed => simp [allowsRtdCreation, Phase.AllowsRtdCreation]
  | failStopped reason => simp [allowsRtdCreation, Phase.AllowsRtdCreation]

private theorem allows_handle_creation_iff (phase : Phase) :
    allowsHandleCreation phase = true ↔ Phase.AllowsHandleCreation phase := by
  cases phase with
  | «open» => simp [allowsHandleCreation, Phase.AllowsHandleCreation]
  | closing stage =>
      cases stage <;> simp [allowsHandleCreation, Phase.AllowsHandleCreation]
  | closed => simp [allowsHandleCreation, Phase.AllowsHandleCreation]
  | failStopped reason => simp [allowsHandleCreation, Phase.AllowsHandleCreation]

private theorem allows_diagnostic_creation_iff (phase : Phase) :
    allowsDiagnosticCreation phase = true ↔ Phase.AllowsDiagnosticCreation phase := by
  cases phase with
  | «open» => simp [allowsDiagnosticCreation, Phase.AllowsDiagnosticCreation]
  | closing stage =>
      cases stage <;> simp [allowsDiagnosticCreation, Phase.AllowsDiagnosticCreation]
  | closed => simp [allowsDiagnosticCreation, Phase.AllowsDiagnosticCreation]
  | failStopped reason => simp [allowsDiagnosticCreation, Phase.AllowsDiagnosticCreation]

def apply? (s : State) (event : Event) : Option State :=
  match event with
  | .registerFunction =>
      if s.phase = .open ∧ s.resources.ingressOpen = true then
        some { s with resources :=
          { s.resources with registrations := s.resources.registrations + 1 } }
      else none
  | .unregisterFunction =>
      if live s.phase && s.resources.registrations > 0 then
        some { s with resources :=
          { s.resources with registrations := decrement s.resources.registrations } }
      else none
  | .registerEvent =>
      if s.phase = .open ∧ s.resources.ingressOpen = true then
        some { s with resources :=
          { s.resources with eventRegistrations := s.resources.eventRegistrations + 1 } }
      else none
  | .unregisterEvent =>
      if live s.phase && s.resources.eventRegistrations > 0 then
        some { s with resources :=
          { s.resources with eventRegistrations := decrement s.resources.eventRegistrations } }
      else none
  | .enterExternal =>
      if s.phase = .open ∧ s.resources.ingressOpen = true then
        some { s with resources :=
          { s.resources with externalEntries := s.resources.externalEntries + 1 } }
      else none
  | .leaveExternal =>
      if live s.phase && s.resources.externalEntries > 0 then
        some { s with resources :=
          { s.resources with externalEntries := decrement s.resources.externalEntries } }
      else none
  | .enterCall =>
      if s.phase = .open then
        some { s with resources :=
          { s.resources with activeCalls := s.resources.activeCalls + 1 } }
      else none
  | .leaveCall =>
      if live s.phase && s.resources.activeCalls > 0 then
        some { s with resources :=
          { s.resources with activeCalls := decrement s.resources.activeCalls } }
      else none
  | .createReturnBlock =>
      if allowsReturnCreation s.phase = true ∧ s.resources.activeCalls > 0 then
        some { s with resources :=
          { s.resources with returnBlocks := s.resources.returnBlocks + 1 } }
      else none
  | .beginReturnFree =>
      if allowsReturnFree s.phase = true ∧
          s.resources.returnFreeOperations < s.resources.returnBlocks then
        some { s with resources :=
          { s.resources with
            returnFreeOperations := s.resources.returnFreeOperations + 1 } }
      else none
  | .releaseReturnBlock =>
      if live s.phase && s.resources.returnBlocks > 0 then
        some { s with resources :=
          { s.resources with returnBlocks := decrement s.resources.returnBlocks } }
      else none
  | .endReturnFree =>
      if live s.phase && s.resources.returnFreeOperations > 0 then
        some { s with resources :=
          { s.resources with
            returnFreeOperations := decrement s.resources.returnFreeOperations } }
      else none
  | .startAsyncExecutor =>
      if s.phase = .open ∧ s.resources.asyncExecutorRunning = false then
        some { s with resources :=
          { s.resources with asyncExecutorRunning := true } }
      else none
  | .startAsyncTask =>
      if allowsAsyncCreation s.phase = true ∧ s.resources.asyncExecutorRunning = true ∧
          producerAlive s.resources = true then
        some { s with resources :=
          { s.resources with asyncTasks := s.resources.asyncTasks + 1 } }
      else none
  | .endAsyncTask _ =>
      if live s.phase ∧ s.resources.asyncTasks > 0 then
        some { s with resources :=
          { s.resources with asyncTasks := decrement s.resources.asyncTasks } }
      else none
  | .stopAsyncExecutor =>
      if live s.phase ∧ s.resources.asyncTasks = 0 ∧
          s.resources.asyncExecutorRunning = true then
        some { s with resources :=
          { s.resources with asyncExecutorRunning := false } }
      else none
  | .beginRtdOperation =>
      if s.phase = .open ∧ s.resources.ingressOpen = true then
        some { s with resources :=
          { s.resources with rtdOperations := s.resources.rtdOperations + 1 } }
      else none
  | .endRtdOperation =>
      if live s.phase ∧ s.resources.rtdOperations > 0 then
        some { s with resources :=
          { s.resources with rtdOperations := decrement s.resources.rtdOperations } }
      else none
  | .addSubscription =>
      if allowsSubscriptionCreation s.phase = true ∧ s.resources.rtdOperations > 0 then
        some { s with resources :=
          { s.resources with subscriptions := s.resources.subscriptions + 1 } }
      else none
  | .removeSubscription =>
      if live s.phase ∧ s.resources.subscriptions > 0 then
        some { s with resources :=
          { s.resources with subscriptions := decrement s.resources.subscriptions } }
      else none
  | .beginCallback =>
      if allowsSubscriptionCreation s.phase = true ∧ s.resources.subscriptions > 0 then
        some { s with resources :=
          { s.resources with callbacks := s.resources.callbacks + 1 } }
      else none
  | .endCallback =>
      if live s.phase ∧ s.resources.callbacks > 0 then
        some { s with resources :=
          { s.resources with callbacks := decrement s.resources.callbacks } }
      else none
  | .addRtdClassFactory =>
      if allowsRtdCreation s.phase = true ∧ s.resources.rtdOperations > 0 then
        some { s with resources :=
          { s.resources with rtdClassFactories := s.resources.rtdClassFactories + 1 } }
      else none
  | .removeRtdClassFactory =>
      if live s.phase ∧ s.resources.rtdClassFactories > 0 then
        some { s with resources :=
          { s.resources with
            rtdClassFactories := decrement s.resources.rtdClassFactories } }
      else none
  | .addRtdServer =>
      if allowsRtdCreation s.phase = true ∧ s.resources.rtdOperations > 0 then
        some { s with resources :=
          { s.resources with rtdServers := s.resources.rtdServers + 1 } }
      else none
  | .removeRtdServer =>
      if live s.phase ∧ s.resources.rtdServers > 0 then
        some { s with resources :=
          { s.resources with rtdServers := decrement s.resources.rtdServers } }
      else none
  | .lockRtdServer =>
      if allowsRtdCreation s.phase = true ∧ s.resources.rtdClassFactories > 0 then
        some { s with resources :=
          { s.resources with rtdServerLocks := s.resources.rtdServerLocks + 1 } }
      else none
  | .unlockRtdServer =>
      if live s.phase ∧ s.resources.rtdServerLocks > 0 then
        some { s with resources :=
          { s.resources with
            rtdServerLocks := decrement s.resources.rtdServerLocks } }
      else none
  | .beginHandleOperation =>
      if allowsHandleCreation s.phase = true then
        some { s with resources :=
          { s.resources with handleOperations := s.resources.handleOperations + 1 } }
      else none
  | .endHandleOperation =>
      if live s.phase ∧ s.resources.handleOperations > 0 then
        some { s with resources :=
          { s.resources with
            handleOperations := decrement s.resources.handleOperations } }
      else none
  | .addHandle =>
      if allowsHandleCreation s.phase = true ∧ s.resources.handleOperations > 0 then
        some { s with resources :=
          { s.resources with handles := s.resources.handles + 1 } }
      else none
  | .removeHandle =>
      if live s.phase ∧ s.resources.handles > 0 then
        some { s with resources :=
          { s.resources with handles := decrement s.resources.handles } }
      else none
  | .startDiagnostics =>
      if s.phase = .open ∧ s.resources.diagnosticsRunning = false then
        some { s with resources :=
          { s.resources with diagnosticsRunning := true } }
      else none
  | .enqueueDiagnostic =>
      if allowsDiagnosticCreation s.phase = true ∧ s.resources.diagnosticsRunning = true then
        some { s with resources :=
          { s.resources with diagnosticsPending := s.resources.diagnosticsPending + 1 } }
      else none
  | .flushDiagnostic =>
      if live s.phase ∧ s.resources.diagnosticsPending > 0 then
        some { s with resources :=
          { s.resources with
            diagnosticsPending := decrement s.resources.diagnosticsPending } }
      else none
  | .discardDiagnostic =>
      if live s.phase ∧ s.resources.diagnosticsPending > 0 then
        some { s with resources :=
          { s.resources with
            diagnosticsPending := decrement s.resources.diagnosticsPending } }
      else none
  | .stopDiagnostics =>
      if live s.phase ∧ s.resources.diagnosticsPending = 0 ∧
          s.resources.diagnosticsRunning = true then
        some { s with resources :=
          { s.resources with diagnosticsRunning := false } }
      else none
  | .recordCleanupIssue =>
      if live s.phase then
        some { s with resources :=
          { s.resources with cleanupIssues := s.resources.cleanupIssues + 1 } }
      else none
  | .beginClose =>
      if s.phase = .open ∧ s.resources.ingressOpen = true then
        some { s with
          phase := .closing .drainCalls,
          resources := { s.resources with ingressOpen := false } }
      else none
  | .callsDrained =>
      if s.phase = .closing .drainCalls ∧ callsDrained s.resources = true then
        some { s with phase := .closing .drainReturns }
      else none
  | .returnsDrained =>
      if s.phase = .closing .drainReturns ∧ returnsDrained s.resources = true then
        some { s with phase := .closing .drainAsync }
      else none
  | .asyncDrained =>
      if s.phase = .closing .drainAsync ∧ asyncDrained s.resources = true then
        some { s with phase := .closing .stopSubscriptions }
      else none
  | .subscriptionsDrained =>
      if s.phase = .closing .stopSubscriptions ∧ subscriptionsDrained s.resources = true then
        some { s with phase := .closing .detachHost }
      else none
  | .closeCallbackGate =>
      if s.phase = .closing .detachHost ∧ s.resources.callbackGateOpen = true then
        some { s with resources := { s.resources with callbackGateOpen := false } }
      else none
  | .hostDetached =>
      if s.phase = .closing .detachHost ∧ hostDetached s.resources = true then
        some { s with phase := .closing .closeState }
      else none
  | .proveStateUnique =>
      if s.phase = .closing .closeState ∧ s.resources.stateUnique = false then
        some { s with resources := { s.resources with stateUnique := true } }
      else none
  | .proveAddinQuiesced =>
      if s.phase = .closing .closeState ∧ s.resources.addinQuiesced = false then
        some { s with resources := { s.resources with addinQuiesced := true } }
      else none
  | .stateClosed =>
      if s.phase = .closing .closeState ∧
          s.resources.stateUnique = true ∧
          s.resources.addinQuiesced = true ∧
          s.resources.stateOwnedByRuntime = true then
        some { s with
          phase := .closing .drainHandles,
          resources := { s.resources with stateOwnedByRuntime := false } }
      else none
  | .handlesDrained =>
      if s.phase = .closing .drainHandles ∧ handlesDrained s.resources = true then
        some { s with phase := .closing .stopDiagnostics }
      else none
  | .diagnosticsDrained =>
      if s.phase = .closing .stopDiagnostics ∧ diagnosticsDrained s.resources = true then
        some { s with phase := .closing .drainRtd }
      else none
  | .rtdDrained =>
      if s.phase = .closing .drainRtd ∧ rtdDrained s.resources = true then
        some { s with phase := .closing .finalize }
      else none
  | .finishClose =>
      if s.phase = .closing .finalize ∧ quiescent s.resources = true then
        some { s with phase := .closed }
      else none
  | .failStop reason =>
      if live s.phase then some { s with phase := .failStopped reason } else none

theorem apply?_sound
    {s t : State} {event : Event}
    (h : apply? s event = some t) :
    Step s event t := by
  cases s with
  | mk phase resources =>
    cases phase <;>
    try { cases ‹CloseStage› } <;>
    cases event <;>
    simp only [apply?] at h <;>
    split at h <;>
    try simp at h <;>
    try cases h <;>
    first
    | apply Step.registerFunction
    | apply Step.unregisterFunction
    | apply Step.registerEvent
    | apply Step.unregisterEvent
    | apply Step.enterExternal
    | apply Step.leaveExternal
    | apply Step.enterCall
    | apply Step.leaveCall
    | apply Step.createReturnBlock
    | apply Step.beginReturnFree
    | apply Step.releaseReturnBlock
    | apply Step.endReturnFree
    | apply Step.startAsyncExecutor
    | apply Step.startAsyncTask
    | apply Step.endAsyncTask
    | apply Step.stopAsyncExecutor
    | apply Step.beginRtdOperation
    | apply Step.endRtdOperation
    | apply Step.addSubscription
    | apply Step.removeSubscription
    | apply Step.beginCallback
    | apply Step.endCallback
    | apply Step.addRtdClassFactory
    | apply Step.removeRtdClassFactory
    | apply Step.addRtdServer
    | apply Step.removeRtdServer
    | apply Step.lockRtdServer
    | apply Step.unlockRtdServer
    | apply Step.beginHandleOperation
    | apply Step.endHandleOperation
    | apply Step.addHandle
    | apply Step.removeHandle
    | apply Step.startDiagnostics
    | apply Step.enqueueDiagnostic
    | apply Step.flushDiagnostic
    | apply Step.discardDiagnostic
    | apply Step.stopDiagnostics
    | apply Step.recordCleanupIssue
    | apply Step.beginClose
    | apply Step.callsDrained
    | apply Step.returnsDrained
    | apply Step.asyncDrained
    | apply Step.subscriptionsDrained
    | apply Step.closeCallbackGate
    | apply Step.hostDetached
    | apply Step.proveStateUnique
    | apply Step.proveAddinQuiesced
    | apply Step.stateClosed
    | apply Step.handlesDrained
    | apply Step.diagnosticsDrained
    | apply Step.rtdDrained
    | apply Step.finishClose
    | apply Step.failStop
    all_goals
      simp_all [live_iff, decrement_add_one, producer_alive_bool_iff,
      allows_return_creation_iff, allows_return_free_iff,
      allows_async_creation_iff, allows_subscription_creation_iff,
      allows_rtd_creation_iff, allows_handle_creation_iff,
      allows_diagnostic_creation_iff,
      hostDetached,
      callsDrained, returnsDrained, asyncDrained, subscriptionsDrained,
      rtdDrained, handlesDrained, stateClosed, diagnosticsDrained,
      quiescent, Phase.IsLive,
      Phase.AllowsReturnCreation, Phase.AllowsReturnFree,
      Phase.AllowsAsyncCreation, Phase.AllowsSubscriptionCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsDiagnosticCreation, Resources.HostDetached,
      Resources.CallsDrained, Resources.ReturnsDrained,
      Resources.AsyncDrained, Resources.SubscriptionsDrained,
        Resources.RtdDrained, Resources.HandlesDrained,
        Resources.DiagnosticsDrained, Resources.ProducerAlive,
        Resources.StateClosed, Resources.Quiescent] <;>
      omega

theorem apply?_complete
    {s t : State} {event : Event}
    (h : Step s event t) :
    apply? s event = some t := by
  cases h <;>
    simp_all [apply?, live_iff, decrement_succ,
      producer_alive_bool_iff,
      allows_return_creation_iff, allows_return_free_iff,
      allows_async_creation_iff, allows_subscription_creation_iff,
      allows_rtd_creation_iff, allows_handle_creation_iff,
      allows_diagnostic_creation_iff, hostDetached, callsDrained, returnsDrained,
      asyncDrained, subscriptionsDrained, rtdDrained, handlesDrained,
      stateClosed, diagnosticsDrained, quiescent,
      Phase.IsLive, Phase.AllowsReturnCreation, Phase.AllowsReturnFree,
      Phase.AllowsAsyncCreation, Phase.AllowsSubscriptionCreation,
      Phase.AllowsRtdCreation, Phase.AllowsHandleCreation,
      Phase.AllowsDiagnosticCreation, Resources.HostDetached,
      Resources.CallsDrained, Resources.ReturnsDrained,
      Resources.AsyncDrained, Resources.SubscriptionsDrained,
      Resources.RtdDrained, Resources.HandlesDrained,
      Resources.DiagnosticsDrained, Resources.ProducerAlive,
      Resources.StateClosed, Resources.Quiescent] <;>
    omega

end XlFnFormal.Shutdown
