use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::test]
async fn enterprise_routes_exist() {
    let bus = Arc::new(igris_protocol::eco::events::EventBus::new());
    let disc = igris_protocol::eco::discovery::DeviceDiscovery::new(bus);
    let addr: SocketAddr = "127.0.0.1:55327".parse().unwrap();
    disc.start_server(&addr).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();

    for path in [
        "/api/ecosystem/v1/info",
        "/api/ecosystem/v1/notification/list",
        "/api/ecosystem/v1/call/list",
        "/api/ecosystem/v1/sms/list",
    ] {
        let url = format!("http://127.0.0.1:55327{}", path);
        let resp = client.get(&url).send().await.expect(path);
        assert!(resp.status().is_success(), "{} -> {}", path, resp.status());
    }

    // FastSwap native v1 info should 404 here (eco port), but route table
    // check is covered by server router unit test; enterprise call/sms
    // unauthenticated posts must be rejected as untrusted, not 404.
    let call_url = "http://127.0.0.1:55327/api/ecosystem/v1/call/sync";
    let resp = client
        .post(call_url)
        .json(&serde_json::json!({
            "call_id": "c1", "event": "ringing",
            "source_device_id": "unknown", "source_device_name": "t",
            "timestamp": 1
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.get("status").and_then(|v| v.as_str()), Some("error"));
}
