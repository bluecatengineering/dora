//! Functions/types for reading incoming message from UDP
use dhcproto::{Decodable, Encodable};
use futures::ready;
use pin_project::pin_project;
// use tokio::net::UdpSocket;
use tokio_stream::Stream;
use tokio_util::codec::BytesCodec; // , udp::UdpFramed};
use unix_udp_sock::{framed::UdpFramed, UdpSocket};

use std::{
    borrow::Borrow,
    io,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{self, Poll},
};

use crate::{
    handler::{MsgContext, State},
    server::msg::SerialMsg,
};

/// Abstracts reading buffers off of a tokio `net::UdpStream` and converting
/// that raw data into a stream of [`MsgContext`]
///
/// [`MsgContext`]: crate::MsgContext
#[pin_project]
#[derive(Debug)]
pub(crate) struct UdpStream<T, S> {
    #[pin]
    stream: UdpFramed<BytesCodec, S>,
    state: Arc<State>,
    _marker: PhantomData<T>,
}

impl<T, S> UdpStream<T, S>
where
    T: Decodable + Encodable,
    S: Borrow<UdpSocket>,
{
    /// Create a new stream from a `UdpRecv`r and `State`
    pub(crate) fn new(stream: S, state: Arc<State>) -> Self {
        // we just want a stream of bytes, messages will be decoded later
        UdpStream {
            stream: UdpFramed::new(stream, BytesCodec::new()),
            state,
            _marker: PhantomData,
        }
    }
}

impl<T, S> Stream for UdpStream<T, S>
where
    T: Decodable + Encodable,
    S: Borrow<UdpSocket>,
{
    type Item = io::Result<MsgContext<T>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let pin = self.project();
        match ready!(pin.stream.poll_next(cx)) {
            Some(res) => {
                let (buf, meta) = res?;
                let msg = SerialMsg::new(buf.freeze(), meta.addr);
                Poll::Ready(Some(Ok(MsgContext::with_state(
                    msg,
                    meta,
                    Arc::clone(pin.state),
                )?)))
            }
            None => Poll::Ready(None),
        }
    }
}
