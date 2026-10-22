//! Player-owned source adapter. The bundled worker uses stdin/stdout only and
//! never receives server credentials, media paths or media download links.
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        // Also reap a cancelled/timed-out child; no detached source processes.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

pub fn fetch_local_danmaku(
    module: &Path,
    cache: &Path,
    input: &Value,
    is_current: impl Fn() -> bool,
) -> Result<Value, String> {
    let executable = module.join(if cfg!(windows) { "node.exe" } else { "node" });
    let script = module.join("worker.cjs");
    if !executable.is_file() || !script.is_file() {
        return Err("播放器缺少内置弹幕模块，请重新构建或安装完整播放器".to_string());
    }
    if !is_current() {
        return Err("弹幕任务已取消".to_string());
    }
    let mut output = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut errors = tempfile::tempfile().map_err(|e| e.to_string())?;
    let mut command = Command::new(executable);
    command
        .arg(script)
        .arg(cache)
        .current_dir(module)
        .env_remove("NODE_OPTIONS")
        .env_remove("NODE_PATH")
        .stdin(Stdio::piped())
        .stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(errors.try_clone().map_err(|e| e.to_string())?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut worker = Worker(
        command
            .spawn()
            .map_err(|e| format!("启动本地弹幕模块失败：{e}"))?,
    );
    let mut stdin = worker.0.stdin.take().ok_or("本地弹幕模块没有输入管道")?;
    stdin
        .write_all(input.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    drop(stdin);
    let started = Instant::now();
    let status = loop {
        if !is_current() {
            return Err("弹幕任务已取消".to_string());
        }
        if started.elapsed() > Duration::from_secs(120) {
            return Err("本地弹幕匹配或下载超时".to_string());
        }
        if let Some(status) = worker.0.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        thread::sleep(Duration::from_millis(50));
    };
    if output.metadata().map_err(|e| e.to_string())?.len() > 16 * 1024 * 1024 {
        return Err("本地弹幕响应超过大小限制".to_string());
    }
    output.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut text = String::new();
    output
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let payload: Value = serde_json::from_str(&text).map_err(|e| {
        let detail = (|| -> std::io::Result<String> {
            errors.seek(SeekFrom::Start(0))?;
            let mut detail = String::new();
            errors.take(4096).read_to_string(&mut detail)?;
            Ok(detail)
        })();
        match detail {
            Ok(detail) => format!("本地弹幕模块响应无效（{status}）：{e}；{detail}"),
            Err(error) => {
                format!("本地弹幕模块响应无效（{status}）：{e}；读取错误日志失败：{error}")
            }
        }
    })?;
    if !status.success() || payload["status"] == "error" {
        return Err(payload["message"]
            .as_str()
            .unwrap_or("本地弹幕模块异常退出")
            .to_string());
    }
    if !matches!(
        payload["status"].as_str(),
        Some("matched" | "unmatched" | "candidates")
    ) {
        return Err("本地弹幕模块返回未知状态".to_string());
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn bundled_module() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/danmaku/dist")
    }

    #[test]
    #[ignore = "requires npm run build in resources/danmaku; no network requests"]
    fn bundled_worker_reads_source_cache_and_applies_shared_offset()
    -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let key = r#"["local-v1","iqiyi","comment",{"url":"https://www.iqiyi.com/v_test.html","segment_concurrency":6}]"#;
        let filename = format!("{:x}.json", Sha256::digest(key.as_bytes()));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        std::fs::write(
            cache.path().join(filename),
            json!({ "saved_at": now, "value": {
                "comments": [{"p": "1,1,16777215,u", "m": "本地缓存", "cid": "9007199254740993"}]
            }})
            .to_string(),
        )?;
        let result = fetch_local_danmaku(
            &bundled_module(),
            cache.path(),
            &json!({"association": {
                "source": "iqiyi", "episode_url": "https://www.iqiyi.com/v_test.html", "offset_seconds": 3
            }}),
            || true,
        )?;
        assert_eq!(result["comments"][0]["time"], 4);
        assert_eq!(result["comments"][0]["text"], "本地缓存");
        assert_eq!(result["comments"][0]["cid"], "9007199254740993");
        Ok(())
    }

    #[test]
    #[ignore = "requires npm run build in resources/danmaku; no network requests"]
    fn cancelled_worker_is_reaped_and_invalid_startup_reports_stderr()
    -> Result<(), Box<dyn std::error::Error>> {
        let module = tempfile::tempdir()?;
        let cache = tempfile::tempdir()?;
        let executable = if cfg!(windows) { "node.exe" } else { "node" };
        std::fs::copy(
            bundled_module().join(executable),
            module.path().join(executable),
        )?;
        std::fs::write(
            module.path().join("worker.cjs"),
            "setInterval(() => {}, 1000);",
        )?;
        let checks = std::cell::Cell::new(0);
        let started = Instant::now();
        let result = fetch_local_danmaku(module.path(), cache.path(), &json!({}), || {
            checks.set(checks.get() + 1);
            checks.get() < 2
        });
        assert_eq!(result, Err("弹幕任务已取消".to_string()));
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::write(
            module.path().join("worker.cjs"),
            "throw new Error('synthetic startup failure');",
        )?;
        let error = fetch_local_danmaku(module.path(), cache.path(), &json!({}), || true)
            .err()
            .ok_or("missing startup error")?;
        assert!(error.contains("synthetic startup failure"));
        Ok(())
    }
}
