//! Native shared-host transport and Cordis lifecycle; not an upstream business acceptance suite.
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::{Activation, PackageInspection, SharedHost, SharedLaunchConfig},
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use std::{net::SocketAddrV4, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

struct Events(mpsc::UnboundedSender<(String, Value)>);

impl Incoming for Events {
    fn request(&self, _: String, _: Value) -> BoxFuture<Result<Value, RpcError>> {
        Box::pin(async {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                "No business services in this lifecycle test",
            ))
        })
    }

    fn notification(&self, method: String, params: Value) {
        let _ = self.0.send((method, params));
    }
}

fn config(root: &std::path::Path, incoming: Arc<dyn Incoming>) -> SharedLaunchConfig {
    SharedLaunchConfig {
        node: std::env::var_os("MAGI_EXTENSION_NODE")
            .map(PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("Node required"),
        runtime_entry: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/dsh/entry.mjs"),
        endpoint_dir: root.join("socket"),
        host_generation: 7,
        incoming,
    }
}

async fn wait_running(events: &mut mpsc::UnboundedReceiver<(String, Value)>, plugin_id: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some((method, value)) = events.recv().await {
            if method == "plugin.status"
                && value["pluginId"] == plugin_id
                && value["state"] == "running"
            {
                return;
            }
        }
        panic!("host events closed before running");
    })
    .await
    .expect("running event");
}

#[tokio::test]
async fn cancelling_web_rpc_aborts_the_provider_and_keeps_the_host_available() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, mut events) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
        .await
        .unwrap();
    host.open_services(&temp.path().join("data"), json!({}))
        .await
        .unwrap();
    let entry = temp.path().join("provider.mjs");
    let receipt = temp.path().join("requests.txt");
    std::fs::write(
        &entry,
        r#"
        import { appendFileSync } from 'node:fs';
        export const inject = ['web'];
        export function apply(ctx, { receipt }) {
            ctx.web.registerFetchProvider({
                id: 'test-provider',
                available: () => true,
                async fetch({ url }, signal) {
                    if (url.endsWith('/next')) return { url };
                    return await new Promise((_, reject) => {
                        signal.addEventListener('abort', () => {
                            appendFileSync(receipt, 'aborted\n');
                            reject(signal.reason);
                        }, { once: true });
                        appendFileSync(receipt, 'started\n');
                    });
                },
            });
        }
        "#,
    )
    .unwrap();
    host.activate(Activation {
        plugin_id: "test-provider".into(),
        instance_id: "test-provider".into(),
        activation_generation: 1,
        version: "1.0.0".into(),
        entry,
        config: json!({ "receipt": receipt }),
    })
    .await
    .unwrap();
    wait_running(&mut events, "test-provider").await;

    let wait_for_receipt = |expected: &'static str| async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while std::fs::read_to_string(&receipt).as_deref().ok() != Some(expected) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    };
    {
        let call = host.call_cancellable_service(
            "host.web.fetch",
            json!({"url": "https://example.com/first"}),
            Duration::from_secs(30),
        );
        tokio::pin!(call);
        tokio::select! {
            result = &mut call => panic!("fetch completed before cancellation: {result:?}"),
            () = wait_for_receipt("started\n") => {}
        }
    }
    wait_for_receipt("started\naborted\n").await;
    let timed_out = host
        .call_cancellable_service(
            "host.web.fetch",
            json!({"url": "https://example.com/timed-out"}),
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
    assert_eq!(timed_out.kind(), ErrorKind::DeadlineExceeded);
    wait_for_receipt("started\naborted\nstarted\naborted\n").await;
    let result = host
        .call_cancellable_service(
            "host.web.fetch",
            json!({"url": "https://example.com/next"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert_eq!(result["url"], "https://example.com/next");
    host.stop().await.unwrap();
}

#[tokio::test]
async fn one_native_process_hosts_services_and_invalidates_only_the_stopped_plugin() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, mut events) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
        .await
        .unwrap();
    let provider = temp.path().join("provider.mjs");
    let consumer = temp.path().join("consumer.mjs");
    let receipt = temp.path().join("receipt.txt");
    std::fs::write(
        &provider,
        "export function apply(ctx) { ctx.provide('sharedExample', { value: 'native-cordis' }); }",
    )
    .unwrap();
    std::fs::write(
        &consumer,
        r#"
        import { writeFileSync } from 'node:fs';
        export const inject = ['sharedExample'];
        export function apply(ctx, config) {
            writeFileSync(config.receipt, ctx.sharedExample.value);
            ctx.effect(() => () => writeFileSync(config.receipt, 'released'));
        }
    "#,
    )
    .unwrap();
    let consumer = Activation {
        plugin_id: "consumer".into(),
        instance_id: "consumer".into(),
        activation_generation: 1,
        version: "1.0.0".into(),
        entry: consumer,
        config: json!({"receipt": receipt}),
    };
    let provider = Activation {
        plugin_id: "provider".into(),
        instance_id: "provider".into(),
        activation_generation: 1,
        version: "1.0.0".into(),
        entry: provider,
        config: json!({}),
    };
    let waiting = host.activate(consumer).await.unwrap();
    assert_eq!(waiting["state"], "waiting");
    assert_eq!(waiting["missingServices"], json!(["sharedExample"]));
    host.activate(provider.clone()).await.unwrap();
    wait_running(&mut events, "consumer").await;
    assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "native-cordis");
    host.deactivate("provider", "provider", 1).await.unwrap();
    assert_eq!(
        host.snapshot("consumer", "consumer", 1).await.unwrap()["state"],
        "waiting"
    );
    assert_eq!(std::fs::read_to_string(&receipt).unwrap(), "released");
    host.activate(Activation {
        activation_generation: 2,
        ..provider
    })
    .await
    .unwrap();
    wait_running(&mut events, "consumer").await;
    assert!(host.deactivate("provider", "provider", 1).await.is_err());
    assert_eq!(
        host.snapshot("provider", "provider", 2).await.unwrap()["state"],
        "running"
    );
    host.stop().await.unwrap();
    assert_eq!(std::fs::read_to_string(receipt).unwrap(), "released");
}

#[tokio::test]
async fn original_module_error_does_not_destroy_the_shared_connection() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, _) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
        .await
        .unwrap();
    let entry = temp.path().join("broken.mjs");
    std::fs::write(
        &entry,
        "export function apply() { throw new Error('upstream initialization error'); }",
    )
    .unwrap();
    let activation = Activation {
        plugin_id: "broken".into(),
        instance_id: "broken".into(),
        activation_generation: 1,
        version: "1.0.0".into(),
        entry,
        config: json!({}),
    };
    let error = host.activate(activation).await.unwrap_err();
    assert!(error.to_string().contains("upstream initialization error"));
    assert!(host.snapshot("broken", "broken", 1).await.is_err());
    host.stop().await.unwrap();
}

#[tokio::test]
async fn authenticated_owner_opens_web_routes_and_releases_them_with_the_shared_host() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, _) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
        .await
        .unwrap();

    let origin = "http://127.0.0.1:38123";
    assert!(host.open_web_routes("file:///index.html").await.is_err());
    let endpoint = host.open_web_routes(origin).await.unwrap();
    let repeated = host.open_web_routes(origin).await.unwrap();
    assert_eq!(endpoint.address(), repeated.address());
    assert!(
        host.open_web_routes("http://127.0.0.1:38124")
            .await
            .is_err()
    );
    assert!(
        endpoint
            .headers(host.generation() + 1, "include:配置", 1, 1)
            .is_err()
    );

    let entry = temp.path().join("routes.mjs");
    std::fs::write(&entry, r#"
        export const inject = ['webServer'];
        export function apply(ctx) {
            ctx.webServer.registerFallback((req, res) => res.end(`fallback:${req.url}`));
            ctx.webServer.register({ kind: 'exact', path: '/resource', resource: async (_req, peer) => {
                return new Response(peer ? peer.name : 'http');
            }});
            ctx.webServer.tapIndex(html => html.replace('placeholder', 'tap'));
            ctx.on('webserver/index-inject', (table, peer) => {
                table.push({ kind: 'global', name: 'testPeer', value: peer.name });
            });
            ctx.webServer.register({ kind: 'exact', path: '/api', async handler(req, res) {
                if (req.headers['x-magi-route-proof']) throw new Error('transport headers leaked');
                const origin = ctx.webServer.getOrigin();
                const peer = { name: 'admitted' };
                const reply = await ctx.webServer.readResource(new Request(`${origin}/resource`), peer);
                res.end(`${origin};${await reply.text()};${ctx.webServer.renderIndex('<html><head></head><body>placeholder</body></html>', peer)}`);
            }});
        }
    "#).unwrap();
    let instance = "include:配置";
    let running = host
        .activate(Activation {
            plugin_id: "routes".into(),
            instance_id: instance.into(),
            activation_generation: 1,
            version: "1.0.0".into(),
            entry,
            config: json!({}),
        })
        .await
        .unwrap();
    assert_eq!(running["state"], "running");

    let epoch = running["callEpoch"].as_u64().unwrap();
    let headers = endpoint
        .headers(host.generation(), instance, 1, epoch)
        .unwrap();
    let response = get(endpoint.address(), "/api", &[]).await;
    assert!(response.starts_with("HTTP/1.1 403"));
    let response = get(endpoint.address(), "/api", &headers).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("http://127.0.0.1:38123;admitted;"),
        "{response}"
    );
    assert!(
        response.contains("globalThis[\"testPeer\"] = \"admitted\""),
        "{response}"
    );
    assert!(response.contains("tap</body>"), "{response}");
    let response = get(endpoint.address(), "/other", &headers).await;
    assert!(response.ends_with("fallback:/other"));
    assert!(
        get(endpoint.address(), "/resource", &headers)
            .await
            .contains("http")
    );
    host.deactivate("routes", instance, 1).await.unwrap();
    assert!(
        get(endpoint.address(), "/api", &headers)
            .await
            .starts_with("HTTP/1.1 404")
    );
    host.stop().await.unwrap();
    endpoint.closed().await;
    assert!(endpoint.headers(7, instance, 1, epoch).is_err());
    assert!(
        tokio::net::TcpStream::connect(endpoint.address())
            .await
            .is_err()
    );

    async fn get(address: SocketAddrV4, path: &str, headers: &[(&str, String)]) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let mut request =
                format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:38123\r\nConnection: close\r\n");
            for (name, value) in headers {
                request.push_str(&format!("{name}: {value}\r\n"));
            }
            request.push_str("\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        })
        .await
        .expect("private HTTP request deadline")
    }
}

#[tokio::test]
async fn cancelling_an_unfinished_activation_reclaims_the_host_without_late_registration() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, mut events) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = Arc::new(
        SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
            .await
            .unwrap(),
    );
    let entry = temp.path().join("unfinished.mjs");
    std::fs::write(
        &entry,
        "await new Promise(() => {}); export function apply() {}",
    )
    .unwrap();
    let running_host = Arc::clone(&host);
    let request = tokio::spawn(async move {
        running_host
            .activate(Activation {
                plugin_id: "unfinished".into(),
                instance_id: "unfinished".into(),
                activation_generation: 1,
                version: "1".into(),
                entry,
                config: json!({}),
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some((method, value)) = events.recv().await {
            if method == "plugin.status"
                && value["pluginId"] == "unfinished"
                && value["state"] == "starting"
            {
                return;
            }
        }
        panic!("activation did not start");
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert_eq!(
        host.snapshot("unfinished", "unfinished", 1)
            .await
            .unwrap_err()
            .kind(),
        ErrorKind::HostGone
    );
    // stop waits for managed process-tree reclamation even after the control connection failed.
    assert!(host.stop().await.is_err());
}

#[tokio::test]
async fn source_parsing_and_package_admission_cross_the_real_host_without_executing_the_package() {
    let temp = tempfile::tempdir().unwrap();
    let (sender, _) = mpsc::unbounded_channel();
    let group = ManagedProcessGroup::new();
    let host = SharedHost::launch(&group, config(temp.path(), Arc::new(Events(sender))))
        .await
        .unwrap();
    let source = host
        .parse_configuration("cordis.patch.yml", "- id: business\n  disabled: false\n")
        .await
        .unwrap();
    assert_eq!(
        source["entries"],
        json!([{ "id": "business", "disabled": false }])
    );
    assert_eq!(source["sha256"].as_str().unwrap().len(), 64);
    assert!(
        host.parse_configuration("cordis.yml", "- [broken")
            .await
            .is_err()
    );

    let project = std::fs::canonicalize(temp.path()).unwrap();
    let directory = project.join("node_modules/original");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(project.join("package.json"), "{\"private\":true}").unwrap();
    let manifest = br#"{"name":"original","version":"1.0.0","main":"index.mjs"}"#;
    std::fs::write(directory.join("package.json"), manifest).unwrap();
    std::fs::write(
        directory.join("index.mjs"),
        "throw new Error('must not import')",
    )
    .unwrap();
    let identity = PackageInspection {
        project_directory: project,
        directory: directory.clone(),
        name: "original".into(),
        version: "1.0.0".into(),
        manifest_sha256: <sha2::Sha256 as sha2::Digest>::digest(manifest)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    };
    let result = host.inspect_package(identity.clone()).await.unwrap();
    assert_eq!(result["entry"], json!(directory.join("index.mjs")));
    std::fs::write(directory.join("package.json"), "{}").unwrap();
    assert!(
        host.inspect_package(identity)
            .await
            .unwrap_err()
            .to_string()
            .contains("manifest changed")
    );
    host.stop().await.unwrap();
}
