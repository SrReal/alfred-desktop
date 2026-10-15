//! Explicit transport retry policy shared by the application's HTTP clients.
//! One approved action must not acquire an implicit protocol replay. Callers
//! retain their own host, redirect, proxy, timeout and outcome contracts.
pub(crate) fn builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().retry(reqwest::retry::never())
}

#[cfg(test)]
#[path = "http_client_tests.rs"]
mod tests;
