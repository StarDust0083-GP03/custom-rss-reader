use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

/// 调试日志管理器
#[derive(Clone)]
pub struct DebugLogger {
    enabled: bool,
    debug_dir: PathBuf,
}

impl DebugLogger {
    /// 从环境变量或默认路径创建调试日志器
    pub fn new(app_handle: &AppHandle) -> Self {
        // 检查环境变量 RSS_READER_DEBUG
        let enabled = std::env::var("RSS_READER_DEBUG").is_ok();

        let debug_dir = if enabled {
            let app_dir = app_handle
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from("."));

            let debug_dir = app_dir.join("debug_logs");

            // 创建调试目录
            if let Err(e) = fs::create_dir_all(&debug_dir) {
                eprintln!("Failed to create debug directory: {}", e);
            }

            debug_dir
        } else {
            PathBuf::from(".")
        };

        Self { enabled, debug_dir }
    }

    /// 创建一个不需要 AppHandle 的调试日志器（用于测试或临时使用）
    pub fn new_temp() -> Self {
        let enabled = std::env::var("RSS_READER_DEBUG").is_ok();

        let debug_dir = if enabled {
            PathBuf::from("./debug_logs_temp")
        } else {
            PathBuf::from(".")
        };

        if enabled {
            if let Err(e) = fs::create_dir_all(&debug_dir) {
                eprintln!("Failed to create debug directory: {}", e);
            }
        }

        Self { enabled, debug_dir }
    }

    /// 记录原始内容（抓取的 RSS XML）
    pub fn log_raw_feed(&self, subscription_id: i64, url: &str, content: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let safe_url = url.replace(
            |c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.',
            "_",
        );
        let filename = format!(
            "raw_{}_{}_{}.xml",
            timestamp,
            subscription_id,
            &safe_url[..safe_url.len().min(50)]
        );
        self.write_file(&filename, content);
    }

    /// 记录解析后的条目
    pub fn log_parsed_items(&self, subscription_id: i64, items_json: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("parsed_{}_{}.json", timestamp, subscription_id);
        self.write_file(&filename, items_json);
    }

    /// 记录网站内容
    pub fn log_website_content(&self, subscription_id: i64, url: &str, content: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let safe_url = url.replace(
            |c: char| !c.is_alphanumeric() && c != '_' && c != '-' && c != '.',
            "_",
        );
        let filename = format!(
            "website_{}_{}_{}.html",
            timestamp,
            subscription_id,
            &safe_url[..safe_url.len().min(50)]
        );
        self.write_file(&filename, content);
    }

    /// 记录错误
    pub fn log_error(&self, context: &str, error: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("error_{}.log", timestamp);
        let content = format!("[{}] {}\nError: {}\n\n", timestamp, context, error);
        self.write_file(&filename, &content);
    }

    /// 记录通用信息
    pub fn log_info(&self, context: &str, info: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("info_{}.log", timestamp);
        let content = format!("[{}] {}\n{}\n\n", timestamp, context, info);
        self.write_file(&filename, &content);
    }

    /// 记录警告信息
    pub fn log_warning(&self, context: &str, warning: &str) {
        if !self.enabled {
            return;
        }

        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let filename = format!("warning_{}.log", timestamp);
        let content = format!("[{}] {}\nWarning: {}\n\n", timestamp, context, warning);
        self.write_file(&filename, &content);
    }

    /// 写入文件
    fn write_file(&self, filename: &str, content: &str) {
        let file_path = self.debug_dir.join(filename);

        match fs::write(&file_path, content) {
            Ok(_) => {
                println!("[DEBUG] Written: {}", file_path.display());
            }
            Err(e) => {
                eprintln!("[DEBUG] Failed to write {}: {}", filename, e);
            }
        }
    }
}
