#![feature(async_await)]

use futures::sync::mpsc::{Sender, Receiver, channel};
use tokio_fixes::{run_async, spawn_async};
use futures::sink::Sink;
use futures::future::IntoFuture;
use tokio_futures::stream::StreamExt;
use tokio_futures::compat::forward::IntoAwaitable;
use std::future::Future;
use std::mem;

mod tokio_fixes {
    // workaround for https://github.com/tokio-rs/tokio/issues/1094

    use tokio;
    use tokio_futures::compat;

    pub fn run_async<F>(future: F)
        where
            F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::run(compat::infallible_into_01(future));
    }

    pub fn spawn_async<F>(future: F)
        where
            F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(compat::infallible_into_01(future));
    }
}

type FlyingBlock<Value> = Box<FnOnce(&mut Value) + Send + 'static>;

struct StateService<Value> {
    val: Value,
    ch: Receiver<FlyingBlock<Value>>,
}

impl<Value> StateService<Value> {
    async fn run(mut self) {
        loop {
            let block = match self.ch.next().await {
                Some(Ok(v)) => {
                    v
                },
                _ => {
                    break
                }
            };
            block(&mut self.val);
        }
    }
}

pub struct State<Value> {
    ch: Sender<FlyingBlock<Value>>,
}

impl<Value> Clone for State<Value> {
    fn clone(&self) -> Self {
        State {
            ch: self.ch.clone(),
        }
    }
}

trait Cap<'a> {}
impl<'a, T> Cap<'a> for T {}


impl<Value: Send + 'static> State<Value> {
    pub fn new(val: Value) -> State<Value> {
        let (tx, rx) = channel(1);
        let state = State {
            ch: tx,
        };
        spawn_async(async {
            let service = StateService { val, ch: rx };
            service.run().await;
        });
        state
    }

    pub fn apply<'a, Ret, Blk>(&self, block: Blk)
                               -> impl Future<Output=Ret> + Cap<'a>
        where
            Ret: Unpin + Send + 'static,
            Blk: FnOnce(&mut Value) -> Ret,
            Blk: Send + 'a
    {
        async move {
            let (mut tx, mut rx) = channel(0);

            fn translife<'a, Value, Ret>(blk: impl FnOnce(&mut Value) -> Ret + Send + 'a)
                -> impl FnOnce(&mut Value) -> Ret + Send + 'static {
                unsafe { mem::transmute(blk) }
            }

            let block = translife(block);

            // TODO: 优化去掉Box
            let block = Box::new(move |v: &mut Value| {
                tx.try_send(block(v)).expect("send response failed");
            });

            self.ch.clone()
                .send(block)
                .into_future()
                .into_awaitable()
                .await
                .expect("send request failed");

            rx.next()
                .await
                .expect("response some expected")
                .expect("response ok expected")

        }
    }
}

fn main() {
    run_async(async {
        let state = State::new(1);

        state.apply(|v|{
            *v += 1;
        }).await;

        let value = state.apply(|x|{
            *x
        }).await;

        assert_eq!(value, 2);

        let mut a = 0;
        state.apply(|x|{
            a = *x;
        }).await;
    });
}

