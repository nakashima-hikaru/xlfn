// module: return_abi
use super::PreparedPublication;
fn invalid(publication: PreparedPublication) {
    let _first = publication.commit();
    let _second = publication.commit(); // error: E0382
}
