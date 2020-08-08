use ara::{buf_reader_at::BufReaderAt, ReadAt};
use errors::make_io_error;
use reqwest::Url;
use std::{fs::File as StdFile, io};

pub(crate) mod errors;

pub mod file;
pub mod http;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    http: Option<http::Opts>,
    buffering: Buffering,
}

#[derive(Debug, Clone)]
pub enum Buffering {
    Unbuffered,
    Buffered(ara::buf_reader_at::Opts),
}

impl Default for Buffering {
    fn default() -> Self {
        Self::Buffered(Default::default())
    }
}

impl Buffering {
    fn apply<'a, R: ReadAt + 'a>(self, r: R) -> Box<dyn ReadAt + 'a> {
        match self {
            Buffering::Unbuffered => Box::new(r),
            Buffering::Buffered(opts) => Box::new(BufReaderAt::with_opts(r, opts)),
        }
    }
}

/// Forwards to `OpenOptions::open` with default options
pub async fn open(path: &str) -> io::Result<Box<dyn ReadAt>> {
    OpenOptions::default().open(path).await
}

impl OpenOptions {
    /// Opens a file or HTTP resource as a `ReadAt`, using
    /// default options. Note that this method does not support non-UTF8
    /// file paths.
    async fn open(self, path: &str) -> io::Result<Box<dyn ReadAt>> {
        if path.starts_with("http:") || path.starts_with("https:") {
            let u = Url::parse(path).map_err(make_io_error)?;
            let r = http::Resource::with_opts(u, self.http.unwrap_or_default())
                .await
                .map_err(make_io_error)?;
            let r = r.into_read_at();
            Ok(self.buffering.apply(r))
        } else {
            let f = StdFile::open(path)?;
            let f = file::File::new(f)?;
            Ok(self.buffering.apply(f))
        }
    }
}
