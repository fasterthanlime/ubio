use crate::{file::File, http};
use ara::ReadAt;
use color_eyre::eyre;
use mktemp::Temp;
use oorandom::Rand32;
use reqwest::Url;
use scopeguard::defer;
use std::sync::Arc;

fn install_tracing() {
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};

    let fmt_layer = fmt::layer();
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .with(ErrorLayer::default())
        .init();
}

#[tokio::test(threaded_scheduler)]
async fn run_tests() {
    // FIXME: having a single entry point isn't great, but
    // if we have several `#[test]` functions, we can't install
    // a global logger anymore.

    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    install_tracing();
    color_eyre::install().unwrap();

    test_http_resource_inner().await.unwrap();
    test_file_inner().await.unwrap();
}

#[tracing::instrument]
async fn test_http_resource_inner() -> Result<(), eyre::Error> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    defer! {
        tx.send(()).unwrap();
    }

    let mut rand = Rand32::new(0xF00D);
    let data = get_random_data(&mut rand, 16384);
    let data = Arc::new(data);

    let (addr, server) = test_server::start(data.clone(), rx).await?;
    tokio::spawn(async {
        server.await;
    });

    let mut u: Url = "http://localhost/".parse().unwrap();
    u.set_port(Some(addr.port())).unwrap();

    {
        // check that open works
        crate::open(u.as_str()).await.unwrap();
    }

    let ra = http::Resource::new(u).await?.into_read_at();
    test_ra(&mut rand, &data[..], ra).await;

    Ok(())
}

#[tracing::instrument]
async fn test_file_inner() -> Result<(), eyre::Error> {
    let mut rand = Rand32::new(0xFACE);
    let data = get_random_data(&mut rand, 16384);

    let temp_file = Temp::new_file().unwrap();
    std::fs::write(&temp_file, &data[..]).unwrap();

    {
        // check that open works
        crate::open(temp_file.as_path().to_str().unwrap())
            .await
            .unwrap();
    }

    let f = std::fs::File::open(&temp_file).unwrap();
    let ra = File::new(f).unwrap();

    test_ra(&mut rand, &data[..], ra).await;

    Ok(())
}

async fn test_ra<R>(rand: &mut Rand32, v: &[u8], r: R)
where
    R: ReadAt,
{
    let max_read_len: u32 = 1024;
    let mut buf_actual: Vec<u8> = Vec::with_capacity(max_read_len as _);

    let num_reads: usize = 200;
    for _ in 0..num_reads {
        let offset = rand.rand_range(0..v.len() as u32 - max_read_len) as u64;
        let read_len = rand.rand_range(0..max_read_len) as usize;

        unsafe { buf_actual.set_len(read_len as _) };
        r.read_at_exact(offset, &mut buf_actual[..read_len as _])
            .await
            .unwrap();

        let buf_expect = &v[offset as usize..(offset as usize + read_len)];
        assert_eq!(buf_expect, &buf_actual[..]);
    }
}

fn get_random_data(rand: &mut Rand32, len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    for _ in 0..len {
        v.push(rand.rand_range(0..256) as u8);
    }
    v
}

mod test_server {
    use bytes::Bytes;
    use color_eyre::Report;
    use futures::future::BoxFuture;
    use http_serve::Entity;
    use hyper::service::{make_service_fn, service_fn};
    use hyper::{header::HeaderValue, Body, HeaderMap, Request, Response, Server};
    use std::convert::Infallible;
    use std::{error::Error as StdError, net::SocketAddr, sync::Arc};

    async fn hello<T>(req: Request<Body>, data: Arc<T>) -> Result<Response<Body>, Infallible>
    where
        T: Clone + Sync + Send + AsRef<[u8]> + 'static,
    {
        let entity = SliceEntity {
            data,
            phantom: Default::default(),
        };
        let res = http_serve::serve(entity, &req);
        Ok(res)
    }

    pub(crate) async fn start<T>(
        data: Arc<T>,
        cancel_signal: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<(SocketAddr, BoxFuture<'static, ()>), Report>
    where
        T: Clone + Send + Sync + AsRef<[u8]> + 'static,
    {
        let make_svc = make_service_fn(move |_| {
            let data = data.clone();
            async move { Ok::<_, Infallible>(service_fn(move |req| hello(req, data.clone()))) }
        });

        let addr = ([127, 0, 0, 1], 0).into();
        let server = Server::bind(&addr).serve(make_svc);

        let addr = server.local_addr();
        println!("Listening on http://{}", server.local_addr());

        let server = server.with_graceful_shutdown(async {
            cancel_signal.await.ok();
        });

        let fut = async move {
            server.await.unwrap();
        };
        Ok((addr, Box::pin(fut)))
    }

    struct SliceEntity<T, E> {
        data: Arc<T>,
        phantom: std::marker::PhantomData<E>,
    }

    impl<T, E> Entity for SliceEntity<T, E>
    where
        T: Clone + Sync + Send + AsRef<[u8]> + 'static,
        E: 'static
            + Send
            + Sync
            + Into<Box<dyn StdError + Send + Sync>>
            + From<Box<dyn StdError + Send + Sync>>,
    {
        type Error = E;
        type Data = Bytes;

        fn len(&self) -> u64 {
            self.data.as_ref().as_ref().len() as u64
        }

        fn get_range(
            &self,
            range: std::ops::Range<u64>,
        ) -> Box<dyn futures::Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync>
        {
            let buf = Bytes::copy_from_slice(
                &self.data.as_ref().as_ref()[range.start as usize..range.end as usize],
            );
            Box::new(futures::stream::once(async move { Ok(buf) }))
        }
        fn add_headers(&self, headers: &mut HeaderMap) {
            headers.insert("content-type", HeaderValue::from_static("text/plain"));
        }
        fn etag(&self) -> Option<hyper::header::HeaderValue> {
            None
        }
        fn last_modified(&self) -> Option<std::time::SystemTime> {
            None
        }
    }
}
