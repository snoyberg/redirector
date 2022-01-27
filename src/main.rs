use std::{collections::HashMap, convert::Infallible, net::SocketAddr, str::FromStr, sync::Arc};

use anyhow::Context;
use clap::StructOpt;
use hyper::{
    header::{HeaderName, HeaderValue, HOST, LOCATION},
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server, StatusCode,
};

#[derive(clap::Parser)]
struct Opt {
    /// Redirect to insecure HTTP instead of HTTPS
    #[clap(long)]
    insecure: bool,
    /// Source<->dest pairs of domain names, e.g. example.com=www.example.com
    pairs: Vec<DomainPair>,
    /// Optional default domain destination when no other domain provided
    #[clap(long)]
    fallback: Option<String>,
    /// Host/port to bind to
    #[clap(long, default_value = "0.0.0.0:3000")]
    bind: SocketAddr,
}

struct App {
    domain_map: HashMap<Vec<u8>, String>,
    fallback: Option<String>,
    insecure: bool,
}

struct DomainPair {
    source: String,
    dest: String,
}

impl FromStr for DomainPair {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match DomainPair::parse_option(s) {
            None => Err(anyhow::anyhow!("Invalid domain pair: {}", s)),
            Some(pair) => Ok(pair),
        }
    }
}
impl DomainPair {
    fn parse_option(s: &str) -> Option<Self> {
        let mut pieces = s.split('=');
        let source = pieces.next()?;
        let dest = pieces.next()?;
        if pieces.next().is_none() {
            Some(DomainPair {
                source: source.to_owned(),
                dest: dest.to_owned(),
            })
        } else {
            None
        }
    }
}

impl App {
    async fn handle(self: Arc<Self>, req: Request<Body>) -> Result<Response<Body>, Infallible> {
        Ok(self.handle_inner(req).await)
    }

    async fn handle_inner(self: Arc<Self>, req: Request<Body>) -> Response<Body> {
        let host = match req.headers().get(HOST) {
            None => {
                eprintln!("Received request without hostname");
                return make_response(StatusCode::BAD_REQUEST, "Missing host header", []);
            }
            Some(host) => host,
        };
        match host.to_str() {
            Ok(host) => eprintln!("Received request for http://{}{}", host, req.uri()),
            Err(_) => eprintln!(
                "Received request for non-UTF8 host {:?} with URI {}",
                host,
                req.uri()
            ),
        }
        let dest = match self.domain_map.get(host.as_bytes()) {
            Some(dest) => dest,
            None => match &self.fallback {
                Some(fallback) => fallback,
                None => return make_response(StatusCode::BAD_REQUEST, "Unsupported hostname", []),
            },
        };
        let location = format!(
            "{scheme}://{dest}{uri}",
            scheme = if self.insecure { "http" } else { "https" },
            dest = dest,
            uri = req.uri(),
        );
        match HeaderValue::from_str(&location) {
            Ok(location) => make_response(
                StatusCode::PERMANENT_REDIRECT,
                "Redirecting",
                [(LOCATION, location)],
            ),
            Err(e) => make_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "Unable to convert location {:?} to HTTP header value: {:?}",
                    location, e
                ),
                [],
            ),
        }
    }
}

fn make_response(
    code: StatusCode,
    body: impl Into<Body>,
    headers: impl IntoIterator<Item = (HeaderName, HeaderValue)>,
) -> Response<Body> {
    let mut res = Response::new(body.into());
    *res.status_mut() = code;
    for (name, value) in headers {
        res.headers_mut().insert(name, value);
    }
    res
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    let mut domain_map = HashMap::new();
    for DomainPair { source, dest } in opt.pairs {
        if domain_map.contains_key(source.as_bytes()) {
            anyhow::bail!("Duplicate destination for domain name {}", source);
        }
        domain_map.insert(source.into_bytes(), dest);
    }
    let app = Arc::new(App {
        domain_map,
        fallback: opt.fallback,
        insecure: opt.insecure,
    });

    let make_svc = make_service_fn(move |_conn| {
        let app = app.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let app = app.clone();
                app.handle(req)
            }))
        }
    });

    Server::bind(&opt.bind)
        .serve(make_svc)
        .await
        .context("Hyper server exited unexpectedly")
}
