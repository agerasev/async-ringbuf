use crate::ring_buffer::{AsyncRbBase, AsyncRbRead};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll, Waker},
};
#[cfg(feature = "std")]
use futures::io::AsyncRead;
use futures::{future::FusedFuture, stream::Stream};
use ringbuf::{ring_buffer::RbRef, Consumer};
#[cfg(feature = "std")]
use std::io;
#[cfg(feature = "impl-tokio")]
use tokio::io::AsyncRead as TokioRead;

pub struct AsyncConsumer<T, R: RbRef>
where
    R::Rb: AsyncRbRead<T>,
{
    base: Consumer<T, R>,
}

impl<T, R: RbRef> AsyncConsumer<T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    pub fn from_base(base: Consumer<T, R>) -> Self {
        Self { base }
    }
    pub fn as_base(&self) -> &Consumer<T, R> {
        &self.base
    }
    pub fn as_mut_base(&mut self) -> &mut Consumer<T, R> {
        &mut self.base
    }

    pub fn capacity(&self) -> usize {
        self.base.capacity()
    }
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }
    pub fn is_full(&self) -> bool {
        self.base.is_full()
    }
    pub fn len(&self) -> usize {
        self.base.len()
    }
    pub fn free_len(&self) -> usize {
        self.base.free_len()
    }

    pub(crate) fn register_waker(&self, waker: &Waker) {
        unsafe { self.base.rb().register_tail_waker(waker) };
    }

    /// Check if the corresponding consumer is dropped.
    pub fn is_closed(&self) -> bool {
        self.base.rb().is_closed()
    }

    /// Pop item from the ring buffer waiting asynchronously if the buffer is empty.
    ///
    /// Future returns:
    /// + `Some(item)` - an item is taken.
    /// + `None` - the buffer is empty and the corresponding producer was dropped.
    pub fn pop(&mut self) -> PopFuture<'_, T, R> {
        PopFuture {
            owner: self,
            done: false,
        }
    }

    /// Wait for the buffer to contain at least `len` items or to close.
    ///
    /// Panics if `len` is greater than buffer capacity.
    pub fn wait(&self, len: usize) -> WaitFuture<'_, T, R> {
        assert!(len <= self.capacity());
        WaitFuture {
            owner: self,
            len,
            done: false,
        }
    }
}

impl<T: Copy, R: RbRef> AsyncConsumer<T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    /// Pop item from the ring buffer waiting asynchronously if the buffer is empty.
    ///
    /// Future returns:
    /// + `Ok` - the whole slice is filled with the items from the buffer.
    /// + `Err(count)` - the buffer is empty and the corresponding producer was dropped, number items copied to slice is returned.
    pub fn pop_slice<'a: 'b, 'b>(&'a mut self, slice: &'b mut [T]) -> PopSliceFuture<'a, 'b, T, R> {
        PopSliceFuture {
            owner: self,
            slice: Some(slice),
            count: 0,
        }
    }
}

impl<T, R: RbRef> Drop for AsyncConsumer<T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    fn drop(&mut self) {
        unsafe { self.base.rb().close_head() };
    }
}

impl<T, R: RbRef> Unpin for AsyncConsumer<T, R> where R::Rb: AsyncRbRead<T> {}

pub struct PopFuture<'a, T, R: RbRef>
where
    R::Rb: AsyncRbRead<T>,
{
    owner: &'a mut AsyncConsumer<T, R>,
    done: bool,
}
impl<'a, T, R: RbRef> Unpin for PopFuture<'a, T, R> where R::Rb: AsyncRbRead<T> {}
impl<'a, T, R: RbRef> FusedFuture for PopFuture<'a, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}
impl<'a, T, R: RbRef> Future for PopFuture<'a, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done);
        self.owner.register_waker(cx.waker());
        let closed = self.owner.is_closed();
        match self.owner.base.pop() {
            Some(item) => {
                self.done = true;
                Poll::Ready(Some(item))
            }
            None => {
                if closed {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

pub struct PopSliceFuture<'a, 'b, T: Copy, R: RbRef>
where
    R::Rb: AsyncRbRead<T>,
{
    owner: &'a mut AsyncConsumer<T, R>,
    slice: Option<&'b mut [T]>,
    count: usize,
}
impl<'a, 'b, T: Copy, R: RbRef> Unpin for PopSliceFuture<'a, 'b, T, R> where R::Rb: AsyncRbRead<T> {}
impl<'a, 'b, T: Copy, R: RbRef> FusedFuture for PopSliceFuture<'a, 'b, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    fn is_terminated(&self) -> bool {
        self.slice.is_none()
    }
}
impl<'a, 'b, T: Copy, R: RbRef> Future for PopSliceFuture<'a, 'b, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    type Output = Result<(), usize>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.owner.register_waker(cx.waker());
        let closed = self.owner.is_closed();
        let mut slice = self.slice.take().unwrap();
        let len = self.owner.base.pop_slice(slice);
        slice = &mut slice[len..];
        self.count += len;
        if slice.is_empty() {
            Poll::Ready(Ok(()))
        } else if closed {
            Poll::Ready(Err(self.count))
        } else {
            self.slice.replace(slice);
            Poll::Pending
        }
    }
}

pub struct WaitFuture<'a, T, R: RbRef>
where
    R::Rb: AsyncRbRead<T>,
{
    owner: &'a AsyncConsumer<T, R>,
    len: usize,
    done: bool,
}
impl<'a, T, R: RbRef> Unpin for WaitFuture<'a, T, R> where R::Rb: AsyncRbRead<T> {}
impl<'a, T, R: RbRef> FusedFuture for WaitFuture<'a, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    fn is_terminated(&self) -> bool {
        self.done
    }
}
impl<'a, T, R: RbRef> Future for WaitFuture<'a, T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(!self.done);
        self.owner.register_waker(cx.waker());
        let closed = self.owner.is_closed();
        if self.len <= self.owner.len() || closed {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl<T, R: RbRef> Stream for AsyncConsumer<T, R>
where
    R::Rb: AsyncRbRead<T>,
{
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.register_waker(cx.waker());
        let closed = self.is_closed();
        match self.base.pop() {
            Some(item) => Poll::Ready(Some(item)),
            None => {
                if closed {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

#[cfg(feature = "std")]
impl<R: RbRef> AsyncRead for AsyncConsumer<u8, R>
where
    R::Rb: AsyncRbRead<u8>,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        self.register_waker(cx.waker());
        let closed = self.is_closed();
        let len = self.base.pop_slice(buf);
        if len != 0 || closed {
            Poll::Ready(Ok(len))
        } else {
            Poll::Pending
        }
    }
}

#[cfg(feature = "impl-tokio")]
impl<R: RbRef> TokioRead for AsyncConsumer<u8, R>
where
    R::Rb: AsyncRbRead<u8>,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let len = futures::ready!(AsyncRead::poll_read(self, cx, buf.initialize_unfilled())?);
        buf.advance(len);
        Poll::Ready(Ok(()))
    }
}
