use crate::errors::make_io_error;
use ara::ReadAt;
use async_trait::async_trait;
use futures::lock::Mutex;
use positioned_io::ReadAt as _;
use std::{fs::File as StdFile, io, sync::Arc};
use tokio::task::spawn_blocking;

pub struct File {
    file: Arc<StdFile>,
    len: u64,
    internal_buf: Mutex<Option<Vec<u8>>>,
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("dead file - a previous read went horribly wrong and it can no longer be read from")]
    DeadFile,
}

impl File {
    pub fn new(file: StdFile) -> io::Result<Self> {
        let len = file.metadata()?.len();
        let file = Arc::new(file);
        let internal_buf = Mutex::new(Some(Vec::new()));
        tracing::trace!("opened file, len = {}", len);
        Ok(Self {
            file,
            len,
            internal_buf,
        })
    }
}

#[async_trait(?Send)]
impl ReadAt for File {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let file = self.file.clone();

        let mut buf_slot = self.internal_buf.lock().await;
        let mut internal_buf = match buf_slot.take() {
            Some(buf) => buf,
            None => return Err(make_io_error(Error::DeadFile)),
        };

        internal_buf.clear();
        internal_buf.reserve(buf.len());
        unsafe {
            internal_buf.set_len(buf.len());
        }

        match spawn_blocking(move || {
            let res = file.read_at(offset, &mut internal_buf);
            (internal_buf, res)
        })
        .await
        {
            Ok((internal_buf, res)) => {
                if let Ok(n) = &res {
                    let n = *n;
                    let dst = &mut buf[..n];
                    let src = &internal_buf[..n];
                    dst.copy_from_slice(src);
                }
                *buf_slot = Some(internal_buf);
                res
            }
            Err(e) => Err(make_io_error(e)),
        }
    }

    fn len(&self) -> u64 {
        self.len
    }
}
