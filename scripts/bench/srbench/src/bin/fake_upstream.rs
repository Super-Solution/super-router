// Fake OpenAI Responses upstream: DELAY_MS before the first event, then 4 deltas, then response.completed with usage.
use axum::serve::ListenerExt;
use axum::{body::Body, response::Response, routing::post, Router};
use bytes::Bytes;
use std::convert::Infallible;
use std::time::Duration;

async fn responses(delay: u64) -> Response {
    let body = Body::from_stream(async_stream::stream! {
        if delay > 0 { tokio::time::sleep(Duration::from_millis(delay)).await; }
        for i in 0..4 {
            yield Ok::<Bytes, Infallible>(Bytes::from(format!("event: response.output_text.delta\ndata: {{\"type\":\"response.output_text.delta\",\"delta\":\"tok{i} \"}}\n\n")));
        }
        yield Ok(Bytes::from("event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_fake\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\n"));
    });
    let mut r = Response::new(body);
    r.headers_mut().insert("content-type", "text/event-stream".parse().unwrap());
    r
}

#[tokio::main]
async fn main() {
    let delay: u64 = std::env::var("DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let addr = std::env::var("ADDR").unwrap_or("127.0.0.1:3499".into());
    let app = Router::new().route("/v1/responses", post(move || responses(delay)));
    let l = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(l.tap_io(|t| { let _ = t.set_nodelay(true); }), app).await.unwrap();
}
