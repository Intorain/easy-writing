//! 事件汇：宿主发事件只经这里。
//!
//! 窗口在时转给窗口；进程内的订阅者（将来的微信适配器、测试）同样收到同一条事件。
//! 宿主的任何功能都不以窗口存在为前提，窗口只是其中一个订阅者。
//! 没有订阅者也没有窗口时事件静默丢弃；落轨迹与投递队列在调度器与投递步骤接入。

use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::Emitter;

pub type Subscriber = Arc<dyn Fn(&str, &Value) + Send + Sync>;

static SUBSCRIBERS: OnceLock<Mutex<Vec<(u64, Subscriber)>>> = OnceLock::new();
static NEXT_ID: Mutex<u64> = Mutex::new(1);

fn registry() -> &'static Mutex<Vec<(u64, Subscriber)>> {
    SUBSCRIBERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// 订阅全部宿主事件；返回的句柄丢弃时自动退订。
pub fn subscribe(subscriber: Subscriber) -> Subscription {
    let id = {
        let mut next = NEXT_ID.lock().unwrap();
        let id = *next;
        *next += 1;
        id
    };
    registry().lock().unwrap().push((id, subscriber));
    Subscription(id)
}

pub struct Subscription(u64);

impl Drop for Subscription {
    fn drop(&mut self) {
        registry().lock().unwrap().retain(|(id, _)| *id != self.0);
    }
}

/// 发一条事件：先给进程内订阅者，再给窗口（窗口可能不存在，发送失败不算错）。
pub fn emit<T: serde::Serialize>(app: &crate::DesktopAppHandle, name: &str, payload: T) {
    let value = serde_json::to_value(&payload).unwrap_or(Value::Null);
    publish(name, &value);
    let _ = app.emit(name, value);
}

/// 无界面场景下发事件：只到进程内订阅者。
pub fn publish(name: &str, payload: &Value) {
    let subscribers: Vec<Subscriber> = registry()
        .lock()
        .unwrap()
        .iter()
        .map(|(_, s)| s.clone())
        .collect();
    for subscriber in subscribers {
        subscriber(name, payload);
    }
}
