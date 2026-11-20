use axum::{Router, body::Body, extract::Request};
use magi_daemon::plugin_web_gateway::PluginWebGateway;
use magi_extension_host::{
    peer::{BoxFuture, Incoming},
    protocol::{ErrorKind, RpcError},
    shared::{PackageInspection, SharedHost, SharedLaunchConfig},
};
use magi_plugin_system::{
    PluginManager,
    loading_runtime::{LoadingHostServices, LoadingPreparation, LoadingRuntime},
};
use magi_process::ManagedProcessGroup;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

struct Events(mpsc::UnboundedSender<String>);

impl Incoming for Events {
    fn request(&self, _: String, _: Value) -> BoxFuture<Result<Value, RpcError>> {
        Box::pin(async {
            Err(RpcError::new(
                ErrorKind::MethodNotFound,
                "No other host services",
            ))
        })
    }

    fn notification(&self, method: String, params: Value) {
        if method == "host.diagnostic"
            && let Some(message) = params["message"].as_str()
        {
            let _ = self.0.send(message.to_owned());
        }
    }
}

#[tokio::test]
async fn daemon_gateway_preserves_http_and_upgrades_and_revokes_both_on_durable_disable() {
    let directory = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(directory.path()).unwrap();
    let project = root.join("immutable");
    let package = project.join("node_modules/routes");
    fs::create_dir_all(&package).unwrap();
    let manifest = br#"{"name":"routes","version":"1.0.0","main":"index.mjs"}"#;
    fs::write(package.join("package.json"), manifest).unwrap();
    fs::write(package.join("index.mjs"), r#"
      export const inject = ['webServer'];
      export function apply(ctx) {
        ctx.webServer.register({kind: 'exact', path: '/echo', async handler(req, res) {
          const data = [];
          for await (const chunk of req) data.push(chunk);
          const payload = JSON.stringify({ body: Buffer.concat(data).toString(), target: req.url, host: req.headers.host,
              cookie: req.headers.cookie, authorization: req.headers.authorization, internal: req.headers['x-magi-route-proof'] });
          res.writeHead(200, {'content-type': 'application/json', 'content-length': Buffer.byteLength(payload), 'set-cookie': 'plugin=1',
            'clear-site-data': '"cookies"', 'x-magi-owned': 'value', 'connection': 'close, x-hop', 'x-hop': 'control'});
          res.end(payload);
        }});
        ctx.webServer.register({kind: 'exact', path: '/events', handler(_req, res) {
          res.on('close', () => ctx.logger.info('gateway-stream-ended'));
          res.writeHead(200, {'content-type': 'text/event-stream'});
          res.write('data: accepted\n\n');
        }});
        ctx.webServer.registerUpgrade({path: '/socket', handler(_req, socket, first) {
          socket.on('close', () => ctx.logger.info('gateway-upgrade-ended'));
          socket.write('HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n');
          if (first.length) socket.write(first);
          socket.on('data', data => socket.write(data));
        }});
      }
    "#).unwrap();
    let mut owner = PluginManager::open(root.join("state")).unwrap();
    let sources = owner.loading_source_root();
    fs::create_dir_all(&sources).unwrap();
    let source = sources.join("cordis.yml");
    fs::write(&source, "- id: routes\n  name: routes\n").unwrap();
    let preparation = LoadingPreparation {
        project_directory: project.clone(),
        base: "cordis.yml".into(),
        overlays: Vec::new(),
        packages: vec![PackageInspection {
            project_directory: project,
            directory: package,
            name: "routes".into(),
            version: "1.0.0".into(),
            manifest_sha256: magi_core::hex_lower(Sha256::digest(manifest)),
        }],
    };
    let (log, mut logs) = mpsc::unbounded_channel();
    let services = Arc::new(LoadingHostServices::new(Arc::new(Events(log))));
    let group = ManagedProcessGroup::new();
    let host = Arc::new(
        SharedHost::launch(
            &group,
            SharedLaunchConfig {
                node: std::env::var_os("MAGI_EXTENSION_NODE")
                    .map(Into::into)
                    .or_else(|| magi_process::resolve_executable("node"))
                    .expect("Node required"),
                runtime_entry: Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../magi-extension-host/runtime/dsh/entry.mjs"),
                endpoint_dir: root.join("socket"),
                host_generation: owner.allocate_loading_host_generation().unwrap(),
                incoming: services.clone(),
            },
        )
        .await
        .unwrap(),
    );
    let owner = Arc::new(Mutex::new(owner));
    let runtime = Arc::new(LoadingRuntime::new(owner.clone(), host.clone()));
    services.bind(&runtime).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let gateway = Arc::new(
        PluginWebGateway::open(&runtime, &format!("http://{address}"))
            .await
            .unwrap(),
    );
    runtime.reconcile_sources(0, &preparation).await.unwrap();
    let activation = owner
        .lock()
        .unwrap()
        .loading_instance_binding("include:routes")
        .unwrap()
        .clone();
    let page = Arc::new(gateway.bind_page(&activation).unwrap());
    let app = Router::new().fallback(move |request: Request<Body>| {
        let gateway = gateway.clone();
        let page = page.clone();
        async move { gateway.forward(&page, request).await }
    });
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mut upload = send(address, "/echo?q=1", "POST", concat!(
        "Connection: close\r\nTransfer-Encoding: chunked\r\nCookie: daemon=1\r\nAuthorization: secret\r\n",
        "x-magi-route-proof: client-attempt\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
    )).await;
    let headers = read_until(&mut upload, b"\r\n\r\n")
        .await
        .to_ascii_lowercase();
    assert!(headers.starts_with(b"http/1.1 200"));
    let headers = String::from_utf8(headers).unwrap();
    for forbidden in ["set-cookie", "clear-site-data", "x-magi-owned", "x-hop"] {
        assert!(!headers.contains(forbidden));
    }
    let mut body = String::new();
    tokio::time::timeout(Duration::from_secs(5), upload.read_to_string(&mut body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"body":"abcde","target":"/echo?q=1","host":address.to_string()})
    );

    let mut abandoned = send(address, "/events", "GET", "Connection: close\r\n\r\n").await;
    read_until(&mut abandoned, b"data: accepted\n\n").await;
    drop(abandoned);
    tokio::time::timeout(Duration::from_secs(5), async {
        while logs.recv().await.unwrap() != "gateway-stream-ended" {}
    })
    .await
    .expect("a disconnected browser closes the private streaming response");

    let mut abandoned = send(
        address,
        "/socket",
        "GET",
        "Connection: Upgrade\r\nUpgrade: test\r\n\r\nfirst",
    )
    .await;
    read_until(&mut abandoned, b"\r\n\r\n").await;
    read_until(&mut abandoned, b"first").await;
    drop(abandoned);
    tokio::time::timeout(Duration::from_secs(5), async {
        while logs.recv().await.unwrap() != "gateway-upgrade-ended" {}
    })
    .await
    .expect("a disconnected browser closes the private upgraded connection");

    let mut stream = send(address, "/events", "GET", "Connection: close\r\n\r\n").await;
    read_until(&mut stream, b"data: accepted\n\n").await;
    let mut socket = send(
        address,
        "/socket",
        "GET",
        "Connection: Upgrade\r\nUpgrade: test\r\n\r\nfirst",
    )
    .await;
    assert!(
        read_until(&mut socket, b"\r\n\r\n")
            .await
            .starts_with(b"HTTP/1.1 101")
    );
    assert_eq!(read_until(&mut socket, b"first").await, b"first");
    socket.write_all(b"second").await.unwrap();
    assert_eq!(read_until(&mut socket, b"second").await, b"second");

    fs::write(&source, "- id: routes\n  name: routes\n  disabled: true\n").unwrap();
    // Persist without running configuration.apply. Gateway lease revocation
    // itself must disconnect both already-open private sockets.
    let candidate = owner
        .lock()
        .unwrap()
        .prepare_loading_configuration(1, &["cordis.yml".into()])
        .unwrap();
    let previous = owner
        .lock()
        .unwrap()
        .loading_configuration()
        .unwrap()
        .graph
        .clone();
    let plan = host.prepare_configuration(json!({"projectDirectory":preparation.project_directory,"sourceDirectory":candidate.source_root(),
        "sources":candidate.sources(),"base":preparation.base,"overlays":preparation.overlays,"packages":preparation.packages,"previous":previous})).await.unwrap();
    owner
        .lock()
        .unwrap()
        .accept_loading_configuration(candidate, serde_json::from_value(plan).unwrap())
        .unwrap();
    assert_eq!(
        host.snapshot("routes", "include:routes", activation.activation_generation)
            .await
            .unwrap()["state"],
        "running"
    );
    let (mut stream_ended, mut upgrade_ended) = (false, false);
    let closures = tokio::time::timeout(Duration::from_secs(5), async {
        while !stream_ended || !upgrade_ended {
            let line = logs.recv().await.unwrap();
            stream_ended |= line == "gateway-stream-ended";
            upgrade_ended |= line == "gateway-upgrade-ended";
        }
    })
    .await;
    assert!(
        closures.is_ok(),
        "daemon closes both private connections before graph apply (stream: {stream_ended}, upgrade: {upgrade_ended})"
    );
    let mut tail = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut tail))
        .await
        .unwrap()
        .unwrap();
    assert!(!tail.ends_with(b"0\r\n\r\n"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), socket.read(&mut [0]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let mut stale = send(address, "/echo", "GET", "Connection: close\r\n\r\n").await;
    assert!(
        read_until(&mut stale, b"\r\n\r\n")
            .await
            .starts_with(b"HTTP/1.1 409")
    );
    host.stop().await.unwrap();
    server.abort();
}

async fn send(address: SocketAddr, path: &str, method: &str, rest: &str) -> TcpStream {
    let mut client = TcpStream::connect(address).await.unwrap();
    client
        .write_all(format!("{method} {path} HTTP/1.1\r\nHost: {address}\r\n{rest}").as_bytes())
        .await
        .unwrap();
    client
}

async fn read_until(socket: &mut TcpStream, delimiter: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut data = Vec::new();
        while !data.ends_with(delimiter) {
            data.push(socket.read_u8().await.unwrap());
        }
        data
    })
    .await
    .unwrap()
}
