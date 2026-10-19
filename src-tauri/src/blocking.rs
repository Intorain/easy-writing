//! 在任何线程上同步等一个 future。
//!
//! 调度器、投递队列、网页桥这些同步函数既在自己的线程里跑，也会被异步的 tauri 命令直接调用
//! （比如微信通道处理完一条消息后让宿主补发通知）。在 tokio 的工作线程里调 `block_on` 会 panic，
//! 命令的 future 中断、永远不回应，网页那头的 `await` 就一直挂着，表现为"没有任何反应"。
//! 这里先问当前线程是不是在运行时里：是就先把这条工作线程让出去（`block_in_place`）再等；不是就用全局运行时。
use std::future::Future;

pub fn run<F: Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(fut)),
        Err(_) => tauri::async_runtime::block_on(fut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_from_plain_threads_inside_block_on_and_inside_runtime_workers() {
        // 普通线程：没有运行时
        assert_eq!(run(async { 1 }), 1);
        // 测试常用的 block_on 里面再等一次
        assert_eq!(tauri::async_runtime::block_on(async { run(async { 2 }) }), 2);
        // 运行时的工作线程（异步命令就跑在这里）：旧写法 panic，新写法拿到结果
        let (old, new) = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn(async {
                let old = std::panic::catch_unwind(|| tauri::async_runtime::block_on(async { 3 })).is_err();
                let new = run(async { 3 });
                (old, new)
            })
            .await
            .unwrap()
        });
        assert!(old, "tokio 工作线程里直接 block_on 会 panic，这正是命令永不回应的原因");
        assert_eq!(new, 3);
        // 阻塞线程池（spawn_blocking）里也能等
        let value = tauri::async_runtime::block_on(async {
            tauri::async_runtime::spawn_blocking(|| run(async { 4 })).await.unwrap()
        });
        assert_eq!(value, 4);
    }
}
