// module: excel_rtd/windows
use super::server::PublishedServerStart;

fn invalid(published: PublishedServerStart<'_>) {
    published.commit();
    published.commit(); // error: E0382
}
