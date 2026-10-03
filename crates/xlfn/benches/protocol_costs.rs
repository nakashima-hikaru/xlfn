use std::time::Duration;

fn main() {
    if std::env::args().any(|arg| arg == "--token-cache") {
        println!(
            "token_cache_probe {}",
            xlfn::benchmark_support::token_cache_associativity_probe()
        );
        return;
    }

    if std::env::args().any(|arg| arg == "--pipeline") {
        for producers in [1, 4] {
            for capacity in [64, 1024] {
                println!(
                    "rtd_pipeline_probe {}",
                    xlfn::benchmark_support::rtd_pipeline_probe(producers, capacity, 10_000)
                );
            }
        }
        return;
    }

    println!(
        "handle_retirement_debt_probe {}",
        xlfn::benchmark_support::handle_retirement_debt_probe()
    );
    for hold in [Duration::ZERO, Duration::from_millis(1)] {
        println!(
            "handle_removal_probe {}",
            xlfn::benchmark_support::handle_removal_probe(100, hold)
        );
    }
    for producers in [1, 4] {
        for capacity in [1, 64, 1024] {
            println!(
                "rtd_channel_probe {}",
                xlfn::benchmark_support::channel_protocol_probe(producers, capacity, 10_000)
            );
        }
    }
}
