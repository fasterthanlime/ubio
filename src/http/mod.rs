use async_trait::async_trait;
use color_eyre::eyre;
use futures::{AsyncRead, TryStreamExt};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Method,
};
use std::{fmt, sync::Arc};
use url::Url;

use crate::errors::make_io_error;
use ara::{
    read_at_wrapper::{GetReaderAt, ReadAtWrapper},
    ReadAt,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("zero-length resource: the content-length header was not present or zero")]
    ZeroLength,
    #[error("trying to get reader at {requested} but resource ends at {resource_end}")]
    ReadAfterEnd { resource_end: u64, requested: u64 },
}

/// A `ReadAt` implementation for HTTP resources, powered by `reqwest`
pub struct Resource {
    client: reqwest::Client,
    opts: Opts,
    url: Url,
    size: u64,
    initial_response: Option<reqwest::Response>,
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "htfs::File({:?})", self.url)
    }
}

#[derive(Debug, Default)]
pub struct Opts {
    headers: Option<HeaderMap<HeaderValue>>,
}

impl Resource {
    #[tracing::instrument]
    pub async fn new(url: Url) -> Result<Self, eyre::Error> {
        Self::with_opts(url, Default::default()).await
    }

    #[tracing::instrument]
    pub async fn with_opts(url: Url, opts: Opts) -> Result<Self, eyre::Error> {
        let client = reqwest::Client::new();

        let mut resource = Resource {
            client,
            opts,
            url,
            size: 0,
            initial_response: None,
        };

        let initial_response = resource.request(0).await?;
        if let Some(size) = initial_response.content_length() {
            resource.size = size;
        }

        if resource.size == 0 {
            return Err(Error::ZeroLength.into());
        }
        resource.initial_response = Some(initial_response);

        Ok(resource)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Converts this Resource to a ReadAt implementation. Note that if you need
    /// any kind of performance, you *will* want to wrap it in a `BufReaderAt`.
    pub fn into_read_at(mut self) -> impl ReadAt {
        let initial_response =
            self.initial_response
                .take()
                .map(|res| -> (u64, Box<dyn AsyncRead + Unpin>) {
                    let reader =
                        Box::new(res.bytes_stream().map_err(make_io_error).into_async_read());
                    (0, reader)
                });
        let size = self.size;
        let source = Arc::new(self);

        ReadAtWrapper::new(source, size, initial_response)
    }

    async fn request(&self, offset: u64) -> Result<reqwest::Response, eyre::Error> {
        let range = format!("bytes={}-", offset);
        let mut req_builder = self.client.request(Method::GET, self.url.clone());
        if let Some(headers) = self.opts.headers.as_ref() {
            for (k, v) in headers.iter() {
                req_builder = req_builder.header(k, v);
            }
        }
        req_builder = req_builder.header("range", range);

        let req = req_builder.build()?;
        let res = self.client.execute(req).await?;
        Ok(res)
    }
}

#[async_trait(?Send)]
impl GetReaderAt for Resource {
    type Reader = Box<dyn AsyncRead + Unpin>;

    async fn get_reader_at(self: &Arc<Self>, offset: u64) -> std::io::Result<Self::Reader> {
        if offset > self.size {
            Err(make_io_error(Error::ReadAfterEnd {
                resource_end: self.size,
                requested: offset,
            }))
        } else {
            let res = self.request(offset).await.map_err(make_io_error)?;
            Ok(Box::new(
                res.bytes_stream().map_err(make_io_error).into_async_read(),
            ))
        }
    }
}
