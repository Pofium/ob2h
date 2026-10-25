//! Локальный защищенный IPC для синхронизации состояния разблокировки
//! между CLI и запущенным процессом ob2h serve (NFR-1, ADR-18).
//! Ключ передается исключительно через loopback TCP в оперативной памяти.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::info;
use zeroize::Zeroizing;

use super::service::SecretsService;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ServerIpcInfo {
    pub port: u16,
    pub pid: u32,
    pub token: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct IpcRequest {
    pub token: String,
    pub action: String,
    pub key_hex: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    pub message: String,
}

pub struct IpcServer {
    run_file: PathBuf,
}

impl IpcServer {
    pub async fn start(
        data_dir: &Path,
        service: Arc<SecretsService>,
    ) -> anyhow::Result<Self> {
        let run_dir = data_dir.join("run");
        fs::create_dir_all(&run_dir)?;
        let run_file = run_dir.join("server.json");

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let pid = std::process::id();

        let mut token_bytes = [0u8; 16];
        getrandom::getrandom(&mut token_bytes).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
        let token = hex::encode(token_bytes);

        let info = ServerIpcInfo {
            port,
            pid,
            token: token.clone(),
        };
        fs::write(&run_file, serde_json::to_string(&info)?)?;

        let expected_token = token;
        let srv = service.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let token_match = expected_token.clone();
                let srv_clone = srv.clone();

                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }

                    let resp = match serde_json::from_slice::<IpcRequest>(&buf[..n]) {
                        Ok(req) => {
                            if req.token != token_match {
                                IpcResponse {
                                    ok: false,
                                    message: "Invalid IPC token".to_string(),
                                }
                            } else {
                                match req.action.as_str() {
                                    "unlock" => {
                                        if let Some(kh) = req.key_hex {
                                            match hex::decode(kh) {
                                                Ok(kb) if kb.len() == 32 => {
                                                    let mut arr = [0u8; 32];
                                                    arr.copy_from_slice(&kb);
                                                    srv_clone.unlock_with_key(Zeroizing::new(arr));
                                                    IpcResponse {
                                                        ok: true,
                                                        message: "Vault unlocked".to_string(),
                                                    }
                                                }
                                                _ => IpcResponse {
                                                    ok: false,
                                                    message: "Invalid key size".to_string(),
                                                },
                                            }
                                        } else {
                                            IpcResponse {
                                                ok: false,
                                                message: "Missing key".to_string(),
                                            }
                                        }
                                    }
                                    "lock" => {
                                        srv_clone.lock();
                                        IpcResponse {
                                            ok: true,
                                            message: "Vault locked".to_string(),
                                        }
                                    }
                                    _ => IpcResponse {
                                        ok: false,
                                        message: "Unknown action".to_string(),
                                    },
                                }
                            }
                        }
                        Err(e) => IpcResponse {
                            ok: false,
                            message: format!("Parse error: {e}"),
                        },
                    };

                    if let Ok(bytes) = serde_json::to_vec(&resp) {
                        let _ = socket.write_all(&bytes).await;
                    }
                });
            }
        });

        info!("Secrets IPC сервер запущен на 127.0.0.1:{} (PID {})", port, pid);
        Ok(Self { run_file })
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.run_file);
    }
}

pub struct IpcClient;

impl IpcClient {
    pub fn read_server_info(data_dir: &Path) -> Option<ServerIpcInfo> {
        let run_file = data_dir.join("run").join("server.json");
        let content = fs::read_to_string(run_file).ok()?;
        serde_json::from_str(&content).ok()
    }

    pub async fn send_action(
        data_dir: &Path,
        action: &str,
        key: Option<&[u8; 32]>,
    ) -> anyhow::Result<bool> {
        let Some(info) = Self::read_server_info(data_dir) else {
            return Ok(false); // Сервер не запущен
        };

        let req = IpcRequest {
            token: info.token,
            action: action.to_string(),
            key_hex: key.map(hex::encode),
        };

        let mut stream = match TcpStream::connect(("127.0.0.1", info.port)).await {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };

        let payload = serde_json::to_vec(&req)?;
        stream.write_all(&payload).await?;

        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await?;
        if n > 0 {
            let resp: IpcResponse = serde_json::from_slice(&buf[..n])?;
            Ok(resp.ok)
        } else {
            Ok(false)
        }
    }
}
