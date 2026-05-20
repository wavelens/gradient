use connector::ConnectorError;
use reqwest::StatusCode;

#[test]
fn api_error_displays_status_and_message() {
    let e = ConnectorError::Api {
        status: StatusCode::BAD_REQUEST,
        message: "bad input".into(),
    };
    let s = e.to_string();
    assert!(s.contains("400"));
    assert!(s.contains("bad input"));
}

#[test]
fn unauthorized_displays_token_hint() {
    let e = ConnectorError::Unauthorized;
    assert!(e.to_string().to_lowercase().contains("unauthor"));
}
