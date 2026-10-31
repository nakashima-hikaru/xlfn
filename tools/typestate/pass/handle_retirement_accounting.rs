// module: handle/domain
use super::retirement::RetirementAccounting;

fn ordered(accounting: &RetirementAccounting) {
    let _observations = (
        accounting.queued(),
        accounting.debt(),
        accounting.is_reclaiming_here(),
        accounting.current_thread_may_be_reading(),
    );
    if let Some(batch) = accounting.poll_reclaim() {
        batch.reclaim();
    }
    accounting.quiesce_reclaim().reclaim();
    accounting.close_reclaim().reclaim();
    accounting.wait_for_completion();
}

fn construct() {
    ordered(&RetirementAccounting::new());
}
