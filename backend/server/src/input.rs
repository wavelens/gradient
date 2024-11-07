use std::net::SocketAddr;

pub fn url_to_addr(host: &str, port: i32) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    //TODO: validate server url
    let url = SocketAddr::new(host.parse()?, port as u16);
    Ok(url)
}
