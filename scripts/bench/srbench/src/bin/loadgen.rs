// loadgen MODE=router|direct|mock TARGET=... N=... C=... STREAM=0|1
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn env(k: &str, d: &str) -> String { std::env::var(k).unwrap_or(d.into()) }

#[tokio::main]
async fn main() {
    let mode = env("MODE", "router");
    let target = env("TARGET", "http://127.0.0.1:3410");
    let n: u64 = env("N", "20000").parse().unwrap();
    let c: usize = env("C", "64").parse().unwrap();
    let stream = env("STREAM", "0") == "1";
    let client = reqwest::Client::builder().pool_max_idle_per_host(c).build().unwrap();
    let key = format!("Bearer sr_{}", "a".repeat(64));
    let provider = if mode == "mock" { "mock" } else { "openai" };
    if mode != "direct" {
        let svc = json!({"id":"chat","version":1,"mode":"tier","tier":"chat/low","regions":["test"],"required_labels":[],
          "price":{"asset":"credit:demo","input":"2","cached":"1","cache_write":"2","output":"3","per":1},
          "max_attempts":2,"max_provider_cost":"1000000000","max_output_tokens":128,
          "weights":{"cost":1,"latency":0,"reliability":0,"quality":0},"cost_scale":"10000","latency_scale_ms":1000,
          "offerings":[{"id":"a","provider":provider,"model":"m","upstream_model":"gpt-x","region":"test","labels":[],"enabled":true,"source":"byok",
            "max_input_tokens":1024,"max_output_tokens":128,"price":{"asset":"USD","input":"1","cached":"1","cache_write":"1","output":"1","per":1}}]});
        let r = client.post(format!("{target}/v1/admin/service-versions")).header("authorization",&key).header("idempotency-key","pub").json(&svc).send().await.unwrap();
        eprintln!("publish {}", r.status());
        let r = client.post(format!("{target}/v1/admin/credits/grants")).header("authorization",&key).header("idempotency-key","grant").json(&json!({"kind":"customer_sale","asset":"credit:demo","amount":"900000000000000"})).send().await.unwrap();
        eprintln!("grant {}", r.status());
    }
    let next = Arc::new(AtomicU64::new(0));
    let fails = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    let mut hs = vec![];
    for _ in 0..c {
        let (client, next, fails, target, key, mode) = (client.clone(), next.clone(), fails.clone(), target.clone(), key.clone(), mode.clone());
        hs.push(tokio::spawn(async move {
            let mut lat = Vec::new();
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n { break; }
                let t = Instant::now();
                let res = if mode == "direct" {
                    client.post(format!("{target}/v1/responses")).json(&json!({"model":"x","input":[],"stream":true})).send().await
                } else {
                    client.post(format!("{target}/v1/inferences")).header("authorization",&key).header("idempotency-key",format!("{}-{i}", std::env::var("RUN").unwrap_or("r".into())))
                      .json(&json!({"service":"chat","customer_id":"c1","payer_mode":"developer","input":{"messages":[{"role":"user","content":"Hello"}]},"limits":{"max_output_tokens":32,"timeout_ms":30000},"stream":stream})).send().await
                };
                match res {
                    Ok(r) if r.status().is_success() => { let _ = r.bytes().await; }
                    Ok(r) => { if fails.fetch_add(1, Ordering::Relaxed) < 3 { eprintln!("status {} {:?}", r.status(), r.text().await); } }
                    Err(e) => { if fails.fetch_add(1, Ordering::Relaxed) < 3 { eprintln!("err {e}"); } }
                }
                lat.push(t.elapsed().as_micros() as u64);
            }
            lat
        }));
    }
    let mut all = vec![];
    for h in hs { all.extend(h.await.unwrap()); }
    let el = start.elapsed().as_secs_f64();
    all.sort();
    let p = |q: f64| all[((all.len() as f64 * q) as usize).min(all.len()-1)] as f64 / 1000.0;
    println!("mode={mode} stream={stream} n={} c={c} fails={} rps={:.0} p50={:.2}ms p90={:.2}ms p99={:.2}ms max={:.2}ms", all.len(), fails.load(Ordering::Relaxed), all.len() as f64/el, p(0.5), p(0.9), p(0.99), *all.last().unwrap() as f64/1000.0);
}
