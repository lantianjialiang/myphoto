//! 本地回环 HTTP 服务：多线程接收请求，处理文件流与 Range 请求。

use crate::api::{self, Shared};
use std::sync::Arc;
use std::thread;
use tiny_http::Server;

pub struct ServeHandle {
    pub port: u16,
    _server: Arc<Server>,
}

pub fn start(app: Shared) -> Result<ServeHandle, String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| format!("无法启动本地服务：{e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .ok_or_else(|| "无法获取监听端口".to_string())?;
    let server = Arc::new(server);
    let workers = 12;
    for _ in 0..workers {
        let srv = server.clone();
        let app = app.clone();
        thread::spawn(move || loop {
            match srv.recv() {
                Ok(req) => {
                    let resp = api::handle(&app, req);
                    let _ = resp;
                }
                Err(_) => break,
            }
        });
    }
    Ok(ServeHandle {
        port,
        _server: server,
    })
}
