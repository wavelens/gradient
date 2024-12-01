/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use super::consts::*;
use std::net::{SocketAddr, ToSocketAddrs};

pub fn url_to_addr(host: &str, port: i32) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let port = port as usize;

    if !PORT_RANGE.contains(&port) {
        return Err(format!(
            "port out of range {}-{}",
            PORT_RANGE.start(),
            PORT_RANGE.end()
        )
        .into());
    }

    let uri = format!("{}:{}", host, port);
    let url = uri
        .to_socket_addrs()?
        .next()
        .ok_or(format!("{} is not a valid address", uri))?;
    Ok(url)
}

pub fn port_in_range(s: &str) -> Result<u16, String> {
    let port: usize = s
        .parse()
        .map_err(|_| format!("`{s}` is not a port number"))?;

    if PORT_RANGE.contains(&port) {
        Ok(port as u16)
    } else {
        Err(format!(
            "port not in range {}-{}",
            PORT_RANGE.start(),
            PORT_RANGE.end()
        ))
    }
}

pub fn greater_than_zero<
    T: std::str::FromStr + std::cmp::PartialOrd + std::fmt::Display + Default,
>(
    s: &str,
) -> Result<T, String> {
    let num: T = s
        .parse()
        .map_err(|_| format!("`{}` is not a valid number", s))?;

    if num > T::default() {
        Ok(num)
    } else {
        Err(format!("`{}` is not larger than 0", s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_to_addr() {
        let addr = url_to_addr("127.0.0.1", 8080).unwrap();
        assert_eq!(addr.to_string(), "127.0.0.1:8080");

        let addr = url_to_addr("localhost", 8080).unwrap();
        assert_eq!(addr.to_string(), "[::1]:8080");

        let addr = url_to_addr("127.0.0.1", 65536).unwrap_err();
        assert_eq!(addr.to_string(), "port out of range 1-65535");

        let addr = url_to_addr("127.0.0.1", 0).unwrap_err();
        assert_eq!(addr.to_string(), "port out of range 1-65535");

        let addr = url_to_addr("127.0.0.1", -1).unwrap_err();
        assert_eq!(addr.to_string(), "port out of range 1-65535");

        let addr = url_to_addr("::1", 8080).unwrap();
        assert_eq!(addr.to_string(), "[::1]:8080");

        let addr = url_to_addr(":::1", 8080).unwrap_err();
        assert_eq!(
            addr.to_string(),
            "failed to lookup address information: Name or service not known"
        );
    }

    #[test]
    fn test_port_in_range() {
        let port = port_in_range("8080").unwrap();
        assert_eq!(port, 8080);

        let port = port_in_range("65535").unwrap();
        assert_eq!(port, 65535);

        let port = port_in_range("65536").unwrap_err();
        assert_eq!(port, "port not in range 1-65535");

        let port = port_in_range("0").unwrap_err();
        assert_eq!(port, "port not in range 1-65535");
    }

    #[test]
    fn test_greater_than_zero() {
        let num = greater_than_zero::<u32>("1").unwrap();
        assert_eq!(num, 1);

        let num = greater_than_zero::<usize>("0").unwrap_err();
        assert_eq!(num, "`0` is not larger than 0");

        let num = greater_than_zero::<u32>("-1").unwrap_err();
        assert_eq!(num, "`-1` is not a valid number");

        let num = greater_than_zero::<i32>("-1").unwrap_err();
        assert_eq!(num, "`-1` is not larger than 0");

        let num = greater_than_zero::<u32>("a").unwrap_err();
        assert_eq!(num, "`a` is not a valid number");

        let num = greater_than_zero::<f32>("1.0").unwrap();
        assert_eq!(num, 1.0);
    }
}
