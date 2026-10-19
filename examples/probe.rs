//! Prints what device discovery finds on this machine: `cargo run --example probe`.
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let links = mm_ofono::device::links().await.expect("rtnetlink");
    for l in links.iter().filter(|l| l.kind.is_some() || l.name.contains("rmnet")) {
        println!("{l:?}");
    }
    println!("{:#?}", mm_ofono::device::discover().await);
}
