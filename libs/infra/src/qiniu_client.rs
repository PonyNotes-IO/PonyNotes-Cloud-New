// 七牛云对象存储客户端（使用S3兼容模式）
use anyhow::{anyhow, Result};
use aws_sdk_s3::config::{Credentials, Region, SharedCredentialsProvider};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use std::path::Path;
use tracing::{debug, error, info};

/// 七牛云存储客户端配置
#[derive(Clone, Debug)]
pub struct QiniuClientConfig {
  pub access_key: String,
  pub secret_key: String,
  pub bucket: String,
  pub region: String,
  pub s3_endpoint: String,
  pub domain: String,
  pub private_bucket: bool,
  pub url_expire_seconds: u64,
  pub use_https: bool,
}

/// 七牛云存储客户端
pub struct QiniuClient {
  s3_client: S3Client,
  config: QiniuClientConfig,
}

impl QiniuClient {
  /// 创建七牛云客户端实例
  pub async fn new(config: QiniuClientConfig) -> Result<Self> {
    let credentials = Credentials::new(
      config.access_key.clone(),
      config.secret_key.clone(),
      None,
      None,
      "qiniu",
    );
    let shared_credentials = SharedCredentialsProvider::new(credentials);

    // 配置S3客户端，使用七牛云的S3兼容endpoint
    let s3_config = aws_sdk_s3::Config::builder()
      .credentials_provider(shared_credentials)
      .region(Region::new(config.region.clone()))
      .endpoint_url(&config.s3_endpoint)
      .force_path_style(true) // 七牛云需要使用路径样式
      .build();

    let s3_client = S3Client::from_conf(s3_config);

    info!(
      "七牛云客户端初始化成功 - bucket: {}, region: {}, endpoint: {}",
      config.bucket, config.region, config.s3_endpoint
    );

    Ok(Self { s3_client, config })
  }

  /// 上传文件到七牛云
  /// 
  /// # 参数
  /// - `object_key`: 对象存储路径，如 "ai-chat/workspace-id/user-id/image.jpg"
  /// - `data`: 文件数据
  /// - `content_type`: MIME类型，如 "image/jpeg"
  /// 
  /// # 返回
  /// 返回文件的公开访问URL
  pub async fn upload_file(
    &self,
    object_key: &str,
    data: Vec<u8>,
    content_type: &str,
  ) -> Result<String> {
    let file_size = data.len();
    debug!(
      "开始上传文件到七牛云: key={}, size={} bytes, content_type={}",
      object_key, file_size, content_type
    );

    // 创建ByteStream
    let body = ByteStream::from(data);

    // 上传到七牛云
    self
      .s3_client
      .put_object()
      .bucket(&self.config.bucket)
      .key(object_key)
      .body(body)
      .content_type(content_type)
      .send()
      .await
      .map_err(|e| {
        error!("上传文件到七牛云失败: {}", e);
        anyhow!("上传文件到七牛云失败: {}", e)
      })?;

    info!("文件上传成功: {}", object_key);

    // 生成访问URL
    let url = self.generate_url(object_key)?;
    Ok(url)
  }

  /// 生成文件访问URL
  fn generate_url(&self, object_key: &str) -> Result<String> {
    if self.config.private_bucket {
      // 私有空间：生成带签名的临时URL
      // TODO: 实现签名URL生成逻辑
      // 七牛云的签名URL需要使用特定的算法，这里暂时返回基础URL
      // 实际使用时需要实现完整的签名逻辑
      error!("私有空间签名URL暂未实现，请使用公开空间");
      Err(anyhow!("私有空间签名URL暂未实现"))
    } else {
      // 公开空间：直接拼接URL
      let protocol = if self.config.use_https {
        "https"
      } else {
        "http"
      };
      
      // 如果有自定义域名，使用自定义域名，否则使用默认域名
      let domain = if !self.config.domain.is_empty() {
        self.config.domain.trim_end_matches('/').to_string()
      } else {
        // 默认域名格式: bucket.s3-region.qiniucs.com
        format!(
          "{}.s3-{}.qiniucs.com",
          self.config.bucket, self.config.region
        )
      };

      let url = format!("{}://{}/{}", protocol, domain, object_key);
      debug!("生成文件访问URL: {}", url);
      Ok(url)
    }
  }

  /// 删除文件
  pub async fn delete_file(&self, object_key: &str) -> Result<()> {
    debug!("删除七牛云文件: {}", object_key);

    self
      .s3_client
      .delete_object()
      .bucket(&self.config.bucket)
      .key(object_key)
      .send()
      .await
      .map_err(|e| {
        error!("删除七牛云文件失败: {}", e);
        anyhow!("删除七牛云文件失败: {}", e)
      })?;

    info!("文件删除成功: {}", object_key);
    Ok(())
  }

  /// 检查文件是否存在
  pub async fn file_exists(&self, object_key: &str) -> Result<bool> {
    match self
      .s3_client
      .head_object()
      .bucket(&self.config.bucket)
      .key(object_key)
      .send()
      .await
    {
      Ok(_) => Ok(true),
      Err(e) => {
        let error_str = e.to_string();
        if error_str.contains("NotFound") || error_str.contains("404") {
          Ok(false)
        } else {
          Err(anyhow!("检查文件存在性失败: {}", e))
        }
      }
    }
  }

  /// 生成AI文件的对象存储路径
  /// 格式: ai-chat/{workspace_id}/{user_id}/{timestamp}_{random}_{filename}
  pub fn generate_ai_file_key(
    workspace_id: &str,
    user_id: &str,
    filename: &str,
  ) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .as_secs();

    // 生成随机字符串
    let random = uuid::Uuid::new_v4().to_string();
    let random_short = &random[..8]; // 取前8位

    // 清理文件名（移除特殊字符）
    let clean_filename = sanitize_filename(filename);

    format!(
      "ai-chat/{}/{}/{}_{}_{}"

,
      workspace_id, user_id, timestamp, random_short, clean_filename
    )
  }

  /// 获取bucket名称
  pub fn bucket(&self) -> &str {
    &self.config.bucket
  }

  /// 获取配置
  pub fn config(&self) -> &QiniuClientConfig {
    &self.config
  }
}

/// 清理文件名，移除不安全的字符
fn sanitize_filename(filename: &str) -> String {
  filename
    .chars()
    .map(|c| {
      if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
        c
      } else {
        '_'
      }
    })
    .collect()
}

/// 根据文件扩展名推断MIME类型
pub fn infer_content_type(filename: &str) -> String {
  let path = Path::new(filename);
  let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");

  match extension.to_lowercase().as_str() {
    // 图片
    "jpg" | "jpeg" => "image/jpeg",
    "png" => "image/png",
    "gif" => "image/gif",
    "webp" => "image/webp",
    "svg" => "image/svg+xml",
    "bmp" => "image/bmp",
    
    // 文档
    "pdf" => "application/pdf",
    "doc" => "application/msword",
    "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "xls" => "application/vnd.ms-excel",
    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "ppt" => "application/vnd.ms-powerpoint",
    "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    
    // 文本
    "txt" => "text/plain",
    "md" => "text/markdown",
    "html" | "htm" => "text/html",
    "json" => "application/json",
    "xml" => "application/xml",
    
    // 音视频
    "mp3" => "audio/mpeg",
    "mp4" => "video/mp4",
    "wav" => "audio/wav",
    
    // 默认
    _ => "application/octet-stream",
  }
  .to_string()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_sanitize_filename() {
    assert_eq!(sanitize_filename("test image.jpg"), "test_image.jpg");
    assert_eq!(sanitize_filename("文件名.png"), "______.png");
    assert_eq!(sanitize_filename("file@#$.txt"), "file___.txt");
  }

  #[test]
  fn test_infer_content_type() {
    assert_eq!(infer_content_type("image.jpg"), "image/jpeg");
    assert_eq!(infer_content_type("IMAGE.PNG"), "image/png");
    assert_eq!(infer_content_type("document.pdf"), "application/pdf");
    assert_eq!(infer_content_type("unknown.xyz"), "application/octet-stream");
  }

  #[test]
  fn test_generate_ai_file_key() {
    let key = QiniuClient::generate_ai_file_key(
      "workspace-123",
      "user-456",
      "test image.jpg",
    );
    
    assert!(key.starts_with("ai-chat/workspace-123/user-456/"));
    assert!(key.ends_with("_test_image.jpg"));
    assert!(key.contains("_")); // 包含时间戳和随机字符串
  }
}

