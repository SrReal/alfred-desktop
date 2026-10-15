use super::*;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

#[tokio::test]
async fn protocol_refused_stream_default_control_retries_but_shared_client_never_replays() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let counter = count.clone();
    let server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted=listener.accept()=>{
                    let Ok((stream,_))=accepted else {break};
                    let counter=counter.clone();
                    children.spawn(async move {
                        let mut connection=h2::server::handshake(stream).await.unwrap();
                        while let Some(Ok((request,mut respond)))=connection.accept().await {
                            assert_eq!(request.uri().path(),"/owned/action");
                            counter.fetch_add(1,Ordering::SeqCst);
                            respond.send_reset(h2::Reason::REFUSED_STREAM);
                        }
                    });
                },
                _=children.join_next(),if !children.is_empty()=>(),
            }
        }
    });
    let url = format!("http://{address}/owned/action");
    // This control deliberately retains the library default, solely to prove
    // that the owned protocol fault would trigger a cloneable POST replay.
    let control = reqwest::Client::builder()
        .no_proxy()
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    assert!(control
        .post(&url)
        .body("owned control")
        .send()
        .await
        .is_err());
    let default_count = count.load(Ordering::SeqCst);
    assert!(
        default_count >= 2,
        "default control did not retry: {default_count}"
    );
    drop(control);
    let client = builder()
        .no_proxy()
        .http2_prior_knowledge()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    for method in [
        reqwest::Method::POST,
        reqwest::Method::PUT,
        reqwest::Method::PATCH,
        reqwest::Method::DELETE,
        reqwest::Method::GET,
    ] {
        let before = count.load(Ordering::SeqCst);
        assert!(client
            .request(method.clone(), &url)
            .body("owned payload")
            .send()
            .await
            .is_err());
        assert_eq!(
            count.load(Ordering::SeqCst) - before,
            1,
            "unexpected replay for {method}"
        );
    }
    drop(client);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn http_status_failures_are_returned_once_and_only_explicit_calls_send_again() {
    use axum::{body::Body, extract::State, http::Response, Router};
    async fn handler(
        State(count): State<Arc<AtomicUsize>>,
        request: axum::extract::Request,
    ) -> Response<Body> {
        let status = request
            .uri()
            .path()
            .trim_start_matches('/')
            .parse::<u16>()
            .unwrap();
        assert_eq!(request.method(), "POST");
        let body = axum::body::to_bytes(request.into_body(), 128)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), b"owned body");
        count.fetch_add(1, Ordering::SeqCst);
        Response::builder()
            .status(status)
            .header("retry-after", "60")
            .body(Body::from("owned error"))
            .unwrap()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let router = Router::new().fallback(handler).with_state(count.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = builder()
        .no_proxy()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap();
    for status in [429, 503] {
        let before = count.load(Ordering::SeqCst);
        let response = client
            .post(format!("http://{address}/{status}"))
            .body("owned body")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.text().await.unwrap(), "owned error");
        assert_eq!(count.load(Ordering::SeqCst) - before, 1);
    }
    drop(client);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}
