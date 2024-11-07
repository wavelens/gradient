use std::net::SocketAddr;

pub fn url_to_addr(url: &str) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    //TODO: validate server url
    let url = url.parse::<SocketAddr>()?;
    Ok(url)
}
