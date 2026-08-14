use crate::ws::{ConnectState, ConnectStateNotify};
use client_websocket::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast::Sender;
use tokio::sync::mpsc::Receiver;
use tokio::sync::Mutex;

pub(crate) struct ServerFixIntervalPing {
  duration: Duration,
  ping_sender: Option<Sender<Message>>,
  pong_recv: Option<Receiver<()>>,
  #[allow(dead_code)]
  stop_tx: tokio::sync::mpsc::Sender<()>,
  stop_rx: Option<Receiver<()>>,
  state: Arc<parking_lot::Mutex<ConnectStateNotify>>,
  ping_count: Arc<Mutex<u32>>,
  maximum_ping_count: u32,
}

impl ServerFixIntervalPing {
  pub(crate) fn new(
    duration: Duration,
    state: Arc<parking_lot::Mutex<ConnectStateNotify>>,
    ping_sender: Sender<Message>,
    pong_recv: Receiver<()>,
    maximum_ping_count: u32,
  ) -> Self {
    let (tx, rx) = tokio::sync::mpsc::channel(1000);
    Self {
      duration,
      stop_tx: tx,
      stop_rx: Some(rx),
      state,
      ping_sender: Some(ping_sender),
      pong_recv: Some(pong_recv),
      ping_count: Arc::new(Mutex::new(0)),
      maximum_ping_count,
    }
  }

  pub(crate) async fn stop(&self) {
    let _ = self.stop_tx.send(()).await;
  }

  pub(crate) fn run(&mut self) {
    let mut stop_rx = self.stop_rx.take().expect("Only take once");
    let mut interval = tokio::time::interval(self.duration);
    let ping_sender = self.ping_sender.take().expect("Only take once");
    let mut pong_recv = self.pong_recv.take().expect("Only take once");
    let weak_ping_count = Arc::downgrade(&self.ping_count);
    let weak_state = Arc::downgrade(&self.state);
    let reconnect_per_ping = self.maximum_ping_count;
    tokio::spawn(async move {
      loop {
        tokio::select! {
          _ = interval.tick() => {
            // send ping to server
            // when the ping_sender return error which means the ping_receiver was dropped
            if  ping_sender.send(Message::Ping(vec![])).is_err() {
               if let Some(state) =weak_state.upgrade() {
                 state.lock().set_state(ConnectState::PingTimeout);
               }
              break;
            }
            if let Some(ping_count) = weak_ping_count.upgrade() {
              let mut lock = ping_count.lock().await;
              if *lock >= reconnect_per_ping {
                if let Some(state) =weak_state.upgrade() {
                  state.lock().set_state(ConnectState::PingTimeout);
                }
              } else {
                if *lock > 1 {
                 tracing::trace!("ping count: {}", *lock);
                }
                *lock +=1;
              }
            }
          },
          // pong from server
          result = pong_recv.recv() => {
            // recv() 返回 None 表示所有 pong 发送端都已释放 —— 也就是
            // spawn_recv_server_message 的接收循环已经退出，连接实际已经断了。
            //
            // 【2026-08-11 修复：断线后永不重连】
            // 原实现这里是 `continue`。已关闭的 mpsc channel 上 recv() 会立即
            // 返回 None，于是这条分支被反复选中；而同一个 select! 里的
            // interval.tick() 要等 ping_per_secs（10 秒）才就绪，几乎永远抢不到
            // 执行机会。后果是：
            //   1. ping 不再发送，ping_count 不再累加；
            //   2. ConnectState::PingTimeout 永远不会被设置；
            //   3. flowy-server 里 `PingTimeout | Lost => attempt_reconnect`
            //      这条重连路径永不触发，客户端却仍认为自己 Connected；
            //   4. 该 loop 空转吃满一个 CPU 核，表现为界面卡顿。
            //
            // 实测（runwen-logs/log.2026-08-11）：15:08 连接静默死亡后 38 分钟内
            // 没有任何 `[websocket] state:` 日志、没有重连，folder / collab 同步
            // 全部静默失效（中间栏加载不出内容、看起来像断网），直到 15:46:52
            // token 到期刷新，才靠 TokenState::Refresh 这条独立路径恢复。
            //
            // 注：5ea2242f 已在 handle_error 中按 WSError 类型上报 Lost，覆盖了
            // 传输层报错的场景；但 stream 正常结束（next() 返回 None，例如对端
            // 静默关闭）不产生 WSError，仍只能由这里兜住，故本修复不可省略。
            if result.is_none() {
              if let Some(state) = weak_state.upgrade() {
                state.lock().set_state(ConnectState::PingTimeout);
              }
              break;
            }
            if let Some(ping_count) = weak_ping_count.upgrade() {
              let mut lock = ping_count.lock().await;
              *lock = 0;

              if let Some(state) =weak_state.upgrade() {
                state.lock().set_state(ConnectState::Connected);
              }
            }
          },
          _ = stop_rx.recv() => {
            break;
          }
        }
      }
    });
  }
}
