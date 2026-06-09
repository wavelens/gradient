use std::time::Duration;

pub const MAX_BUILD_REQUEST_SIZE: usize = 20 * 1024 * 1024;
pub const UPLOAD_SESSION_TTL: Duration = Duration::from_secs(3600);
