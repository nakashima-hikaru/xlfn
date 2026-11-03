// module: subscription/data_plane
use super::PublishCore;
use crate::subscription::{host::SubscriptionHost, delivery::{RefreshPlan, RtdUpdate}};
fn invalid<H: SubscriptionHost>(publish: &PublishCore<H>, plan: RefreshPlan, updates: &mut Vec<RtdUpdate>) {
    publish.collect_refresh(&plan, updates); // error: E0624
    publish.abort_refresh_no_unwind(plan.refresh_id, Vec::new()); // error: E0624
}
