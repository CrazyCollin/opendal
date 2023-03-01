// Copyright 2022 Datafuse Labs
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt::Display;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::{ready, AsyncWrite};

use crate::ops::OpWrite;
use crate::raw::*;
use crate::*;

/// ObjectWriter is the public API for users to write data.
///
/// # Notes
///
/// ObjectWriter is designed for appending multiple blocks which could
/// lead to much requests. If only want to send all data in single chunk,
/// please use [`Object::write`] instead.
pub struct ObjectWriter {
    state: State,
}

impl ObjectWriter {
    /// Create a new object writer.
    ///
    /// Create will use internal information to decide the most suitable
    /// implementation for users.
    ///
    /// We don't want to expose those details to users so keep this function
    /// in crate only.
    pub(crate) async fn create(acc: FusedAccessor, path: &str, op: OpWrite) -> Result<Self> {
        let (_, w) = acc.write(path, op).await?;

        Ok(ObjectWriter {
            state: State::Idle(Some(w)),
        })
    }

    /// Append data into writer.
    ///
    /// It is highly recommended to align the length of the input bytes
    /// into blocks of 4MiB (except the last block) for better performance
    /// and compatibility.
    pub async fn append(&mut self, bs: impl Into<Bytes>) -> Result<()> {
        if let State::Idle(Some(w)) = &mut self.state {
            w.append(bs.into()).await
        } else {
            unreachable!(
                "writer state invalid while append, expect Idle, actual {}",
                self.state
            );
        }
    }

    /// Close the writer and make sure all data have been stored.
    pub async fn close(&mut self) -> Result<()> {
        if let State::Idle(Some(w)) = &mut self.state {
            w.close().await
        } else {
            unreachable!(
                "writer state invalid while close, expect Idle, actual {}",
                self.state
            );
        }
    }
}

enum State {
    Idle(Option<output::Writer>),
    Write(BoxFuture<'static, Result<(usize, output::Writer)>>),
    Close(BoxFuture<'static, Result<output::Writer>>),
}

impl Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::Idle(_) => write!(f, "Idle"),
            State::Write(_) => write!(f, "Write"),
            State::Close(_) => write!(f, "Close"),
        }
    }
}

impl AsyncWrite for ObjectWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            match &mut self.state {
                State::Idle(w) => {
                    let mut w = w
                        .take()
                        .expect("invalid state of writer: Idle state with empty write");
                    let bs = Bytes::from(buf.to_vec());
                    let size = bs.len();
                    let fut = async move {
                        w.append(bs).await?;
                        Ok((size, w))
                    };
                    self.state = State::Write(Box::pin(fut));
                }
                State::Write(fut) => match ready!(fut.poll_unpin(cx)) {
                    Ok((size, w)) => {
                        self.state = State::Idle(Some(w));
                        return Poll::Ready(Ok(size));
                    }
                    Err(err) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err))),
                },
                State::Close(_) => {
                    unreachable!("invalid state of writer: poll_write with State::Close")
                }
            };
        }
    }

    /// ObjectWriter makes sure that every write is flushed.
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            match &mut self.state {
                State::Idle(w) => {
                    let mut w = w
                        .take()
                        .expect("invalid state of writer: Idle state with empty write");
                    let fut = async move {
                        w.close().await?;
                        Ok(w)
                    };
                    self.state = State::Close(Box::pin(fut));
                }
                State::Write(_) => {
                    unreachable!("invlia state of writer: poll_close with State::Write")
                }
                State::Close(fut) => match ready!(fut.poll_unpin(cx)) {
                    Ok(w) => {
                        self.state = State::Idle(Some(w));
                        return Poll::Ready(Ok(()));
                    }
                    Err(err) => return Poll::Ready(Err(io::Error::new(io::ErrorKind::Other, err))),
                },
            }
        }
    }
}

/// BlockingObjectWriter is the public API for users.
///
/// It works nearly the same with [`ObjectWriter`] but in blocking way.
pub struct BlockingObjectWriter {
    pub(crate) inner: output::BlockingWriter,
}

impl BlockingObjectWriter {
    /// Create a new object writer.
    ///
    /// Create will use internal information to decide the most suitable
    /// implementation for users.
    ///
    /// We don't want to expose those details to users so keep this function
    /// in crate only.
    pub(crate) fn create(acc: FusedAccessor, path: &str, op: OpWrite) -> Result<Self> {
        let (_, w) = acc.blocking_write(path, op)?;

        Ok(BlockingObjectWriter { inner: w })
    }

    /// Append data into writer.
    ///
    /// It is highly recommended to align the length of the input bytes
    /// into blocks of 4MiB (except the last block) for better performance
    /// and compatibility.
    pub fn append(&mut self, bs: impl Into<Bytes>) -> Result<()> {
        self.inner.append(bs.into())
    }

    /// Close the writer and make sure all data have been stored.
    pub fn close(&mut self) -> Result<()> {
        self.inner.close()
    }
}

impl io::Write for BlockingObjectWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let size = buf.len();
        self.append(Bytes::from(buf.to_vec()))
            .map(|_| size)
            .map_err(|err| io::Error::new(io::ErrorKind::Other, err))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}