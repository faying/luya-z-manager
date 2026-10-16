//! 8d-1c ⑥: the exit lookups, u60d's with writes on.

use super::exits::{Exits, pass, route_key};
use super::*;
use std::fs;

#[test]
fn the_default_route_is_the_key() {
    let table = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
        br-lan\t0042000A\t00000000\t0001\t0\t0\t0\t00FFFFFF\n\
        rmnet0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\n";
    assert_eq!(route_key(table, |i| (i == "rmnet0").then(|| "10.1.2.3".into())), "rmnet0 10.1.2.3");
    assert_eq!(route_key(table, |_| None), "rmnet0 ");
    assert_eq!(route_key("Iface\tDestination\n", |_| None), "");
}

#[tokio::test]
async fn a_pass_looks_the_direct_exit_up_once_and_writes_the_file() {
    let root = std::env::temp_dir().join(format!("u60d-exits-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("data")).unwrap();
    fs::create_dir_all(root.join("tmp")).unwrap();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/geo",
        get(move || {
            h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Json(json!({"status": "success", "query": "1.2.3.4", "country": "中国",
                    "regionName": "广东", "city": "深圳", "isp": "China Mobile"}))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await });
    // the second service never answers: the first one's answer is enough
    fs::write(
        root.join("data/netinfo.conf"),
        format!("direct=http://127.0.0.1:{port}/geo,http://127.0.0.1:9/never\n"),
    )
    .unwrap();
    let c = Compat::new(root.to_str().unwrap(), Sources::none()).with_writes(true);
    let table = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\n\
        lo\t00000000\t00000000\t0001\t0\t0\t0\t00000000\n";
    let file = root.join(luyaz_api::netinfo::EXITS_FILE.trim_start_matches('/'));
    let (c2, f2) = (c.clone(), file.clone());
    let v = tokio::task::spawn_blocking(move || {
        let mut x = Exits::default();
        pass(&c2, &mut x, 1000, table);
        // not due again 10 s later on the same route
        pass(&c2, &mut x, 1010, table);
        serde_json::from_str::<Value>(&fs::read_to_string(&f2).unwrap()).unwrap()
    })
    .await
    .unwrap();
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(v["direct"]["ip"], "1.2.3.4", "{v}");
    assert_eq!(v["direct"]["source"], format!("127.0.0.1:{port}"), "{v}");
    assert_eq!(v["direct"]["fetched_at"], 1000);
    let _ = fs::remove_dir_all(&root);
}
